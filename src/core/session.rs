//! Scoped OpenRGB transport with CLI compatibility commands and checked finishing.
use std::io::{BufRead, BufReader};
use std::net::{SocketAddr, TcpStream};
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

const LOCAL_SERVER: &str = "127.0.0.1:6742";
const START_TIMEOUT: Duration = Duration::from_secs(60);
const UNAVAILABLE: &str = "OpenRGB server unavailable before discovery: ";

pub fn is_unavailable(error: &str) -> bool {
    error.starts_with(UNAVAILABLE)
}

pub(super) fn validate_transport() -> Result<(), String> {
    if super::openrgb::server_mode() {
        super::sdk::probe(LOCAL_SERVER.parse().unwrap(), Duration::from_secs(1))
            .map_err(|error| format!("{UNAVAILABLE}{error}"))?;
    }
    Ok(())
}

pub(super) fn finish_solid_color(
    device: &super::openrgb::OpenRgbDevice,
    color: &str,
    preferred_mode: Option<&str>,
) -> Result<(), String> {
    if super::openrgb::server_mode() {
        // Callers retain the whole-action lock and have just resolved live IDs.
        // An old/unsupported SDK keeps the unchanged CLI compatibility path;
        // a supported transaction must confirm its state before persistence.
        super::sdk::solid::finalize_solid(
            LOCAL_SERVER.parse().unwrap(),
            device.id,
            &device.name,
            color,
            preferred_mode,
            Duration::from_secs(2),
        )
        .map_err(|error| {
            format!(
                "{}: OpenRGB color confirmation failed: {error}",
                device.name
            )
        })?;
    }
    Ok(())
}

pub struct OpenRgbSession {
    child: Child,
    reader: Option<JoinHandle<()>>,
    parent_thread: Option<JoinHandle<()>>,
    release_parent: Option<mpsc::Sender<()>>,
    ready: bool,
    external_lock: bool,
}

impl OpenRgbSession {
    /// The restore transaction retains its lock through this session's Drop.
    pub(super) fn start_locked() -> Result<Option<Self>, String> {
        Self::start_inner(true)
    }

    fn start_inner(external_lock: bool) -> Result<Option<Self>, String> {
        let address: SocketAddr = LOCAL_SERVER.parse().unwrap();
        if TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_ok() {
            super::sdk::probe(address, Duration::from_secs(1))
                .map_err(|error| format!("{UNAVAILABLE}{error}"))?;
            super::openrgb::set_server_mode(true);
            return Ok(None);
        }
        let mut command = super::openrgb::base_command();
        command.args([
            "--server",
            "--server-host",
            "127.0.0.1",
            "--server-port",
            "6742",
            "--noautoconnect",
            "--verbose",
        ]);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let (child_tx, child_rx) = mpsc::sync_channel(0);
        let (release_parent, hold_parent) = mpsc::channel::<()>();
        // Linux PDEATHSIG follows the spawning THREAD, not merely its process.
        // A rescan worker must not kill the server when that worker returns.
        let parent_thread = thread::spawn(move || {
            if let Err(error) = child_tx.send(command.spawn()) {
                if let Ok(mut child) = error.0 {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                return;
            }
            let _ = hold_parent.recv();
        });
        let mut child = match child_rx.recv() {
            Ok(Ok(child)) => child,
            failure => {
                drop(release_parent);
                let _ = parent_thread.join();
                let error = match failure {
                    Ok(Err(error)) => error.to_string(),
                    Err(error) => error.to_string(),
                    _ => unreachable!(),
                };
                return Err(format!("Could not start OpenRGB session: {error}"));
            }
        };
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        // Drain the entire stream, including after readiness, so logging cannot
        // fill a pipe and stop hardware updates. Never retain device serials.
        let reader = thread::spawn(move || {
            for line in BufReader::new(stdout).split(b'\n') {
                let Ok(line) = line else { break };
                let line = String::from_utf8_lossy(&line);
                if line.contains("Could not bind network socket") {
                    let _ = tx.send(Err("OpenRGB could not bind its local server".to_string()));
                } else if line.contains("Detection completed") {
                    let _ = tx.send(Ok(()));
                }
            }
        });
        let mut session = Self {
            child,
            reader: Some(reader),
            parent_thread: Some(parent_thread),
            release_parent: Some(release_parent),
            ready: false,
            external_lock,
        };
        // A listening socket is NOT readiness: OpenRGB opens it before scanning.
        rx.recv_timeout(START_TIMEOUT).map_err(|_| {
            "OpenRGB session did not finish detection within 60 seconds".to_string()
        })??;
        if session
            .child
            .try_wait()
            .map_err(|e| e.to_string())?
            .is_some()
        {
            return Err("OpenRGB session exited during startup".to_string());
        }
        TcpStream::connect_timeout(&address, Duration::from_secs(1)).map_err(|error| {
            format!("OpenRGB finished detection but its server is unavailable: {error}")
        })?;
        super::sdk::probe(address, Duration::from_secs(1))?;
        session.ready = true;
        super::openrgb::set_server_mode(true);
        Ok(Some(session))
    }
}

pub type SharedBackend = std::sync::Arc<std::sync::Mutex<Backend>>;

/// Transport selection is per session, not learned per device.
pub struct Backend {
    automatic: bool,
    owned: Option<OpenRgbSession>,
    pub warning: Option<String>,
    blocked: Option<String>,
}

impl Backend {
    pub fn new(automatic: bool) -> Self {
        super::openrgb::set_server_mode(false);
        let mut backend = Self {
            automatic,
            owned: None,
            warning: None,
            blocked: None,
        };
        if automatic {
            if let Err(error) = backend.prepare() {
                backend.warning = Some(error);
            }
        }
        backend
    }

    pub fn label(&self) -> &'static str {
        if self.blocked.is_some() {
            "unavailable"
        } else if super::openrgb::server_mode() {
            "server"
        } else {
            "standalone"
        }
    }

    pub fn prepare(&mut self) -> Result<(), String> {
        let _lock = super::hardware_lock()?;
        self.prepare_locked();
        match &self.blocked {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }

    fn stop_locked(&mut self) {
        if let Some(mut session) = self.owned.take() {
            session.external_lock = true;
            drop(session);
        }
        super::openrgb::set_server_mode(false);
    }

    fn prepare_locked(&mut self) {
        self.blocked = None;
        if !self.automatic {
            return;
        }
        if let Some(session) = self.owned.as_mut() {
            if matches!(session.child.try_wait(), Ok(None)) {
                super::openrgb::set_server_mode(true);
                return;
            }
        }
        if self.owned.is_none()
            && self
                .warning
                .as_ref()
                .is_some_and(|error| !is_unavailable(error))
            && TcpStream::connect_timeout(
                &LOCAL_SERVER.parse().unwrap(),
                Duration::from_millis(100),
            )
            .is_err()
        {
            // A genuine startup failure stays on the compatibility fallback;
            // do not repeat an expensive failed startup before every action.
            // R explicitly retries acceleration.
            return;
        }
        self.stop_locked();
        match OpenRgbSession::start_locked() {
            Ok(owned) => {
                self.owned = owned.map(|mut session| {
                    session.external_lock = false;
                    session
                });
                self.warning = None;
            }
            Err(error) => {
                super::openrgb::set_server_mode(false);
                if is_unavailable(&error) {
                    self.blocked = Some(error.clone());
                }
                self.warning = Some(error);
            }
        }
    }

    pub fn run<T>(&mut self, mut action: impl FnMut() -> Result<T, String>) -> Result<T, String> {
        for attempt in 0..2 {
            let result = self.prepare().and_then(|_| action());
            if attempt == 0
                && self.automatic
                && result.as_ref().is_err_and(|error| is_unavailable(error))
            {
                thread::sleep(Duration::from_millis(25));
                continue;
            }
            return result;
        }
        unreachable!("the final attempt always returns")
    }

    pub fn rescan(&mut self) -> Result<Vec<super::openrgb::OpenRgbDevice>, String> {
        let _lock = super::hardware_lock()?;
        self.warning = None;
        self.blocked = None;
        let address: SocketAddr = LOCAL_SERVER.parse().unwrap();
        if self.owned.is_some() {
            // Restart only our own server. This performs real fresh detection,
            // including newly installed parts, without reusing numeric IDs.
            self.stop_locked();
            self.prepare_locked();
        } else if self.automatic && super::sdk::probe(address, Duration::from_secs(1)).is_ok() {
            // Never kill someone else's OpenRGB instance. Ask it to rescan only
            // if it advertises the matching SDK capability.
            super::sdk::rescan(address, START_TIMEOUT)?;
            super::openrgb::set_server_mode(true);
        } else {
            self.prepare_locked();
        }
        if let Some(error) = &self.blocked {
            return Err(error.clone());
        }
        super::openrgb::list_devices()
    }
}

pub fn with_backend<T>(
    backend: &SharedBackend,
    action: impl FnMut() -> Result<T, String>,
) -> Result<T, String> {
    let mut backend = backend
        .lock()
        .map_err(|_| "RGB backend lock was poisoned".to_string())?;
    backend.run(action)
}

impl Drop for OpenRgbSession {
    fn drop(&mut self) {
        // Another RGBPC process may be using this server. Wait for its entire
        // action before shutdown. Failed startup still holds the startup lock.
        let _lock = if self.ready && !self.external_lock {
            super::hardware_lock().ok()
        } else {
            None
        };
        if self.ready {
            // Allow the server's queued hardware writes to settle before kill.
            thread::sleep(Duration::from_millis(250));
        }
        // Reap our exact child before allowing standalone CLI fallback. No
        // system service, autostart entry, or user's server is changed.
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        drop(self.release_parent.take());
        if let Some(parent) = self.parent_thread.take() {
            let _ = parent.join();
        }
    }
}
