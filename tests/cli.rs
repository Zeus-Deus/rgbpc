use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static FAST_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Peer(Child);
impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("rgbpc-cli-{}-{id}", std::process::id()));
        fs::create_dir_all(path.join("bin")).unwrap();
        fs::create_dir_all(path.join("config/rgbpc")).unwrap();
        let openrgb = path.join("bin/openrgb");
        let script = r#"#!/bin/sh
printf '%s\n' "$*" >> "$RGBPC_TEST_RAW_LOG"
if [ "$1" = '--nodetect' ] && [ "$RGBPC_TEST_REQUIRE_SERVER" = '1' ]; then
  /usr/bin/python3 -c 'import socket; socket.create_connection(("127.0.0.1",6742),timeout=.3).close()' 2>/dev/null || exit 0
fi
case "$1" in --nodetect|--noautoconnect) shift ;; esac
printf '%s\n' "$*" >> "$RGBPC_TEST_LOG"
if [ "$1" = '--server' ]; then
  printf '%s' "$$" > "$RGBPC_TEST_SERVER_PID"
  exec /usr/bin/python3 "$RGBPC_TEST_SERVER_PEER"
fi
if [ "$1" = '--list-devices' ]; then
  if [ -f "$RGBPC_TEST_DEVICES" ]; then
    /bin/cat "$RGBPC_TEST_DEVICES"
  else
    printf '0: MSI Test Board\n  Type: Motherboard\n'
  fi
fi
if [ "$1" = '-d' ] && [ "$2" = "$RGBPC_TEST_FAIL_DEVICE" ]; then exit 3; fi
"#;
        fs::write(&openrgb, script).unwrap();
        fs::set_permissions(&openrgb, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(
            path.join("config/rgbpc/config.toml"),
            "restore_on_startup = true\nstartup_delay_ms = 0\n[last_state]\nkind = 'color'\nhex = '123456'\n",
        )
        .unwrap();
        Self(path)
    }

    fn restore(&self) -> Vec<String> {
        self.run(&["--standalone", "--restore-last"])
    }

    fn peer(&self, mode: &str) -> Peer {
        let fixture_command = self.command();
        let mut command = Command::new("/usr/bin/python3");
        for (key, value) in fixture_command.get_envs() {
            if let Some(value) = value {
                command.env(key, value);
            }
        }
        let mut peer = Peer(
            command
                .arg(
                    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("tests")
                        .join(if mode.starts_with("solid") {
                            "solid_sdk_peer.py"
                        } else {
                            "openrgb_stub.py"
                        }),
                )
                .env("RGBPC_TEST_SERVER_FAIL", mode)
                .stdout(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let mut ready = String::new();
        BufReader::new(peer.0.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert!(ready.contains("Detection completed"), "{ready}");
        peer
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_rgbpc"));
        command
            .env("HOME", &self.0)
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env("XDG_CACHE_HOME", self.0.join("cache"))
            .env("XDG_RUNTIME_DIR", &self.0)
            .env("RGBPC_TEST_LOG", self.0.join("commands.log"))
            .env("RGBPC_TEST_RAW_LOG", self.0.join("raw-commands.log"))
            .env("RGBPC_TEST_SERVER_PID", self.0.join("server.pid"))
            .env(
                "RGBPC_TEST_SERVER_PEER",
                concat!(env!("CARGO_MANIFEST_DIR"), "/tests/openrgb_stub.py"),
            )
            .env_remove("RGBPC_TEST_SERVER_FAIL")
            .env_remove("RGBPC_TEST_FAIL_DEVICE")
            .env("RGBPC_TEST_DEVICES", self.0.join("devices.txt"))
            .env("RGBPC_TEST_SDK_STATE", self.0.join("sdk-state.json"))
            .env("PATH", self.0.join("bin"));
        command
    }

    fn run(&self, args: &[&str]) -> Vec<String> {
        let output = self.command().args(args).output().unwrap();
        assert!(output.status.success(), "{:?}", output);
        assert!(output.stderr.is_empty(), "{:?}", output);
        fs::read_to_string(self.0.join("commands.log"))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn cli_color_outputs_json_and_persists_only_accepted_state() {
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .args(["--standalone", "--color", "#Aa12Ff", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ok"], true);
    assert_eq!(report["verification"], "command_acceptance");
    assert_eq!(
        report["accepted_devices"],
        serde_json::json!(["msi test board::motherboard"])
    );
    let config: toml::Value =
        toml::from_str(&fs::read_to_string(fixture.0.join("config/rgbpc/config.toml")).unwrap())
            .unwrap();
    assert_eq!(
        config["saved_device_states"]["msi test board::motherboard"]["hex"].as_str(),
        Some("Aa12Ff")
    );
    let commands = fs::read_to_string(fixture.0.join("commands.log")).unwrap();
    assert_eq!(
        commands.lines().filter(|c| c.starts_with("-d ")).count(),
        22
    );
}

#[test]
fn server_color_finishes_noop_cli_writes_using_current_capabilities() {
    let _serial = FAST_TEST_LOCK.lock().unwrap();
    let fixture = Fixture::new();
    let _peer = fixture.peer("solid");
    let output = fixture
        .command()
        .args(["--color", "123456", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.0.join("sdk-state.json")).unwrap()).unwrap();
    assert_eq!(
        state["active_mode"], 1,
        "CLI exit zero must not leave the controller in Direct mode"
    );
    assert_eq!(state["static_color_mode"], 1);
    assert_eq!(
        state["brightness"], 8,
        "Unrelated mode settings must be preserved"
    );
    assert_eq!(state["colors"], serde_json::json!(["123456", "123456"]));
    assert_eq!(state["mode_writes"], 1);
    assert_eq!(state["color_writes"], 1);
    let commands = fs::read_to_string(fixture.0.join("commands.log")).unwrap();
    assert_eq!(
        commands.lines().filter(|c| c.starts_with("-d ")).count(),
        22
    );
}

#[test]
fn server_ignoring_color_writes_does_not_persist_false_success() {
    let _serial = FAST_TEST_LOCK.lock().unwrap();
    let fixture = Fixture::new();
    let path = fixture.0.join("config/rgbpc/config.toml");
    let before = fs::read(&path).unwrap();
    let _peer = fixture.peer("solid-ignore");
    let output = fixture
        .command()
        .args(["--color", "abcdef", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ok"], false);
    assert_eq!(report["accepted_devices"], serde_json::json!([]));
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn last_pass_failed_readback_cannot_keep_first_pass_success() {
    let _serial = FAST_TEST_LOCK.lock().unwrap();
    let fixture = Fixture::new();
    let path = fixture.0.join("config/rgbpc/config.toml");
    let before = fs::read(&path).unwrap();
    let _peer = fixture.peer("solid-second-ignore");
    let output = fixture
        .command()
        .args(["--color", "abcdef", "--json"])
        .output()
        .unwrap();
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.0.join("sdk-state.json")).unwrap()).unwrap();
    assert_eq!(
        state["readbacks"][0]["active_mode"], 1,
        "first pass must really reach its supported state"
    );
    assert_eq!(
        state["readbacks"][1]["active_mode"], 0,
        "second pass must fail its postcondition"
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["accepted_devices"], serde_json::json!([]));
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn identical_controller_success_cannot_hide_another_instances_failed_readback() {
    let _serial = FAST_TEST_LOCK.lock().unwrap();
    let fixture = Fixture::new();
    fs::write(
        fixture.0.join("devices.txt"),
        "0: MSI Test Board\n  Type: Motherboard\n1: MSI Test Board\n  Type: Motherboard\n",
    )
    .unwrap();
    let path = fixture.0.join("config/rgbpc/config.toml");
    let before = fs::read(&path).unwrap();
    let _peer = fixture.peer("solid-duplicates");
    let output = fixture
        .command()
        .args(["--color", "abcdef", "--json"])
        .output()
        .unwrap();
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.0.join("sdk-state.json")).unwrap()).unwrap();
    assert_eq!(state["devices"][0]["active_mode"], 0);
    assert_eq!(state["devices"][1]["active_mode"], 1);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["accepted_devices"], serde_json::json!([]));
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn late_version_reply_does_not_waive_the_last_pass_readback() {
    let _serial = FAST_TEST_LOCK.lock().unwrap();
    let fixture = Fixture::new();
    let path = fixture.0.join("config/rgbpc/config.toml");
    let before = fs::read(&path).unwrap();
    let _peer = fixture.peer("solid-late-version");
    let output = fixture
        .command()
        .args(["--color", "abcdef", "--json"])
        .output()
        .unwrap();
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.0.join("sdk-state.json")).unwrap()).unwrap();
    assert_eq!(state["readbacks"][0]["active_mode"], 1);
    assert_eq!(state["active_mode"], 0);
    assert_eq!(state["late_version_replies"], 1);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["accepted_devices"], serde_json::json!([]));
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn wrong_controller_reply_is_rejected_before_sdk_writes() {
    let _serial = FAST_TEST_LOCK.lock().unwrap();
    let fixture = Fixture::new();
    let _peer = fixture.peer("solid-wrong-device");
    let output = fixture
        .command()
        .args(["--color", "abcdef", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.0.join("sdk-state.json")).unwrap()).unwrap();
    assert_eq!(state["mode_writes"], 0);
    assert_eq!(state["color_writes"], 0);
}

#[test]
fn cli_invalid_color_fails_before_any_hardware_process() {
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .args(["--color", "##123456", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ok"], false);
    assert!(!fixture.0.join("commands.log").exists());
}

#[test]
fn cli_unknown_and_disabled_selectors_never_write_to_another_device() {
    for selector in ["missing", "0"] {
        let fixture = Fixture::new();
        if selector == "0" {
            fs::write(
                fixture.0.join("config/rgbpc/config.toml"),
                "disabled_devices = ['MSI Test Board']\n",
            )
            .unwrap();
        }
        let output = fixture
            .command()
            .args(["--standalone", "--off", "--device", selector, "--json"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["ok"], false);
        let log = fs::read_to_string(fixture.0.join("commands.log")).unwrap();
        assert!(!log.lines().any(|line| line.starts_with("-d ")), "{log}");
    }
}

#[test]
fn cli_list_off_and_rainbow_are_noninteractive_and_documented_states() {
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .args(["--standalone", "--list-devices", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["devices"][0]["key"], "msi test board::motherboard");
    assert_eq!(report["devices"][0]["enabled"], true);
    for (flag, kind) in [("--off", "off"), ("--rainbow", "rainbow")] {
        let output = fixture
            .command()
            .args(["--standalone", flag, "--device", "0", "--json"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["ok"], true);
        let config: toml::Value = toml::from_str(
            &fs::read_to_string(fixture.0.join("config/rgbpc/config.toml")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            config["saved_device_states"]["msi test board::motherboard"]["kind"].as_str(),
            Some(kind)
        );
    }
}

#[test]
fn cli_partial_failures_are_json_failure_exits_and_only_persist_successes() {
    let fixture = Fixture::new();
    fs::write(
        fixture.0.join("devices.txt"),
        "0: Generic Board A\n  Type: Motherboard\n1: Generic Board B\n  Type: Motherboard\n",
    )
    .unwrap();
    let output = fixture
        .command()
        .env("RGBPC_TEST_FAIL_DEVICE", "1")
        .args(["--standalone", "--color", "123456", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ok"], false);
    assert_eq!(
        report["accepted_devices"],
        serde_json::json!(["generic board a::motherboard"])
    );
    assert!(!report["errors"].as_array().unwrap().is_empty());
    let config: toml::Value =
        toml::from_str(&fs::read_to_string(fixture.0.join("config/rgbpc/config.toml")).unwrap())
            .unwrap();
    assert!(config["saved_device_states"]
        .get("generic board a::motherboard")
        .is_some());
    assert!(config["saved_device_states"]
        .get("generic board b::motherboard")
        .is_none());
}

#[test]
fn cli_automatically_falls_back_when_server_startup_fails() {
    let _lock = FAST_TEST_LOCK.lock().unwrap();
    assert!(std::net::TcpListener::bind("127.0.0.1:6742").is_ok());
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .env("RGBPC_TEST_SERVER_FAIL", "exit")
        .args(["--color", "123456", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["backend"], "standalone");
    let raw = fs::read_to_string(fixture.0.join("raw-commands.log")).unwrap();
    assert!(raw
        .lines()
        .filter(|l| !l.starts_with("--server "))
        .all(|l| l.starts_with("--noautoconnect ")));
}

#[test]
fn tui_rescan_server_outlives_the_worker_and_is_reused_then_reaped() {
    let _lock = FAST_TEST_LOCK.lock().unwrap();
    assert!(std::net::TcpListener::bind("127.0.0.1:6742").is_ok());
    let fixture = Fixture::new();
    let environment: Vec<_> = fixture
        .command()
        .get_envs()
        .filter_map(|(key, value)| value.map(|value| (key.to_os_string(), value.to_os_string())))
        .collect();
    let output = Command::new("/usr/bin/python3")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/tui_rescan.py"))
        .arg(env!("CARGO_BIN_EXE_rgbpc"))
        .arg(&fixture.0)
        .envs(environment)
        .env_remove("RGBPC_TEST_SERVER_FAIL")
        .env_remove("RGBPC_TEST_FAIL_DEVICE")
        .env("TERM", "xterm-256color")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn cli_recovers_when_a_borrowed_server_dies_after_preparation() {
    let _lock = FAST_TEST_LOCK.lock().unwrap();
    assert!(std::net::TcpListener::bind("127.0.0.1:6742").is_ok());
    for mode in ["drop", "drop-after-two"] {
        let fixture = Fixture::new();
        let _peer = fixture.peer(mode);
        let output = fixture
            .command()
            .env("RGBPC_TEST_REQUIRE_SERVER", "1")
            .args(["--list-devices", "--json"])
            .output()
            .unwrap();
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(report["devices"].as_array().unwrap().len(), 1, "{report}");
        let raw = fs::read_to_string(fixture.0.join("raw-commands.log")).unwrap();
        assert_eq!(
            raw.lines().filter(|l| l.starts_with("--server ")).count(),
            1
        );
    }
}

#[test]
fn an_unresponsive_borrowed_server_is_not_bypassed_with_a_parallel_scan() {
    let _lock = FAST_TEST_LOCK.lock().unwrap();
    assert!(std::net::TcpListener::bind("127.0.0.1:6742").is_ok());
    let fixture = Fixture::new();
    let mut peer = fixture.peer("mute");
    let output = fixture
        .command()
        .args(["--list-devices", "--json"])
        .output()
        .unwrap();
    assert!(
        peer.0.try_wait().unwrap().is_none(),
        "borrowed server was stopped"
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ok"], false);
    assert!(
        !fixture.0.join("raw-commands.log").exists(),
        "must not detect hardware beside a live unresponsive server"
    );
}

#[test]
fn cli_sync_reports_partial_failures_instead_of_success() {
    let fixture = Fixture::new();
    fs::write(
        fixture.0.join("devices.txt"),
        "0: Generic Board A\n  Type: Motherboard\n1: Generic Board B\n  Type: Motherboard\n",
    )
    .unwrap();
    fs::write(fixture.0.join("config/rgbpc/config.toml"), "omarchy_sync_enabled = true\nomarchy_sync_devices = ['generic board a::motherboard', 'generic board b::motherboard']\n").unwrap();
    let theme = fixture.0.join(".local/state/omarchy/current/theme");
    fs::create_dir_all(&theme).unwrap();
    fs::write(theme.join("colors.toml"), "rgb = '#123456'\n").unwrap();
    let output = fixture
        .command()
        .env("RGBPC_TEST_FAIL_DEVICE", "1")
        .args(["--standalone", "--sync-theme", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ok"], false);
    assert_eq!(
        report["accepted_devices"],
        serde_json::json!(["generic board a::motherboard"])
    );
    assert!(!report["errors"].as_array().unwrap().is_empty());
}

#[test]
fn normal_restore_automatically_uses_a_temporary_server() {
    let _lock = FAST_TEST_LOCK.lock().unwrap();
    assert!(std::net::TcpListener::bind("127.0.0.1:6742").is_ok());
    let commands = Fixture::new().run(&["--restore-last"]);
    assert_eq!(
        commands
            .iter()
            .filter(|c| c.starts_with("--server "))
            .count(),
        1,
        "normal usage must not require --fast"
    );
    assert_eq!(commands.iter().filter(|c| c.starts_with("-d ")).count(), 22);
}

#[test]
fn separate_rgbpc_processes_do_not_interleave_delayed_retry_passes() {
    let fixture = Fixture::new();
    let mut first = fixture
        .command()
        .args(["--standalone", "--restore-last"])
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !fs::read_to_string(fixture.0.join("commands.log"))
        .unwrap_or_default()
        .contains("-d 0 -c 123456")
    {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let config = fixture.0.join("config/rgbpc/config.toml");
    let content = fs::read_to_string(&config)
        .unwrap()
        .replace("123456", "abcdef");
    fs::write(config, content).unwrap();
    let mut second = fixture
        .command()
        .args(["--standalone", "--restore-last"])
        .spawn()
        .unwrap();
    assert!(first.wait().unwrap().success());
    assert!(second.wait().unwrap().success());
    let log = fs::read_to_string(fixture.0.join("commands.log")).unwrap();
    let applies: Vec<_> = log.lines().filter(|l| l.starts_with("-d ")).collect();
    assert_eq!(applies.len(), 44);
    assert!(applies[..22].iter().all(|l| !l.contains("abcdef")), "{log}");
    assert!(applies[22..].iter().all(|l| !l.contains("123456")), "{log}");
}

#[test]
fn fast_restore_accepts_the_flag_without_replacing_the_compatibility_path() {
    let _lock = FAST_TEST_LOCK.lock().unwrap();
    assert!(
        std::net::TcpListener::bind("127.0.0.1:6742").is_ok(),
        "fast integration test needs port 6742 free"
    );
    let fixture = Fixture::new();
    let commands = fixture.run(&["--fast", "--restore-last"]);
    assert_eq!(commands.iter().filter(|c| c.starts_with("-d ")).count(), 22);
    if commands.iter().any(|c| c.starts_with("--server ")) {
        let pid: i32 = fs::read_to_string(fixture.0.join("server.pid"))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "owned server must already be reaped"
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }
}

#[test]
fn fast_start_failures_reap_the_child_and_keep_both_fallback_passes() {
    let _lock = FAST_TEST_LOCK.lock().unwrap();
    for mode in ["exit", "bind", "bytes"] {
        assert!(std::net::TcpListener::bind("127.0.0.1:6742").is_ok());
        let fixture = Fixture::new();
        let output = fixture
            .command()
            .env("RGBPC_TEST_SERVER_FAIL", mode)
            .args(["--fast", "--restore-last"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{mode}: {output:?}");
        if mode == "bytes" {
            assert!(output.stderr.is_empty(), "{output:?}");
        } else {
            assert!(String::from_utf8_lossy(&output.stderr).contains("Fast mode unavailable"));
        }
        let commands = fs::read_to_string(fixture.0.join("commands.log")).unwrap();
        assert_eq!(
            commands.lines().filter(|c| c.starts_with("-d ")).count(),
            22
        );
        let pid: i32 = fs::read_to_string(fixture.0.join("server.pid"))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }
}

#[test]
fn disabled_fast_restore_does_not_start_hardware_detection() {
    let fixture = Fixture::new();
    fs::write(
        fixture.0.join("config/rgbpc/config.toml"),
        "restore_on_startup = false\n",
    )
    .unwrap();
    let output = fixture
        .command()
        .args(["--fast", "--restore-last"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(!fixture.0.join("commands.log").exists());
}

#[test]
fn restore_errors_have_a_failure_exit_status() {
    let fixture = Fixture::new();
    fs::write(
        fixture.0.join("config/rgbpc/config.toml"),
        "restore_on_startup = true\nstartup_delay_ms = 0\n",
    )
    .unwrap();
    let output = fixture.command().arg("--restore-last").output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("No saved RGB state"));
    assert!(!fixture.0.join("commands.log").exists());
}

#[test]
fn restore_discovers_once_per_pass_without_dropping_compatibility_commands() {
    let commands = Fixture::new().restore();
    let applies: Vec<_> = commands
        .iter()
        .filter(|command| command.as_str() != "--list-devices")
        .map(String::as_str)
        .collect();
    let pass = [
        "-d 0 -c 123456",
        "-d 0 -m Direct",
        "-d 0 -z 2 -sz 60 -c 123456 -z 3 -sz 60 -c 123456",
        "-d 0 -z 0 -c 123456",
        "-d 0 -z 1 -c 123456",
        "-d 0 -z 2 -c 123456",
        "-d 0 -z 3 -c 123456",
        "-d 0 -z 4 -c 123456",
        "-d 0 -z 5 -c 123456",
        "-d 0 -m Static -c 123456",
        "-d 0 -m Static -c 123456 --save-profile omarchy.orp",
    ];
    assert_eq!(
        applies,
        pass.repeat(2),
        "keep both complete fallback passes"
    );
    assert_eq!(
        commands.iter().filter(|c| *c == "--list-devices").count(),
        2,
        "a just-discovered roster must not immediately be discovered again"
    );
}
