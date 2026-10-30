pub mod config;
pub mod hook;
pub mod openrgb;
mod sdk;
pub mod session;

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use self::config::{AppConfig, SavedState};

#[derive(Debug, Clone, Default)]
pub struct DeviceActionSummary {
    pub succeeded_keys: Vec<String>,
    pub failed_devices: Vec<String>,
}

impl DeviceActionSummary {
    fn from_counts(succeeded_keys: Vec<String>, failed_devices: Vec<String>) -> Self {
        Self {
            succeeded_keys,
            failed_devices,
        }
    }

    pub fn is_any_success(&self) -> bool {
        !self.succeeded_keys.is_empty()
    }
}

pub fn load_theme_color() -> Result<String, String> {
    let candidates = theme_color_candidates()?;
    load_theme_color_from_candidates(&candidates, |path| fs::read_to_string(path))
}

fn load_theme_color_from_candidates<F>(
    candidates: &[PathBuf],
    mut read_to_string: F,
) -> Result<String, String>
where
    F: FnMut(&Path) -> std::io::Result<String>,
{
    for theme_path in candidates {
        match read_to_string(theme_path) {
            Ok(content) => {
                return parse_theme_color(&content).map_err(|error| {
                    format!(
                        "Invalid Omarchy theme colors at {}: {}",
                        theme_path.display(),
                        error
                    )
                })
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "Failed to read Omarchy theme colors at {}: {}",
                    theme_path.display(),
                    error
                ))
            }
        }
    }

    let checked = candidates
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");

    Err(format!(
        "Omarchy theme colors were not found; checked {}",
        checked
    ))
}

fn theme_color_candidates() -> Result<Vec<PathBuf>, String> {
    let home_dir = dirs::home_dir()
        .ok_or_else(|| "Could not determine the user home directory".to_string())?;
    Ok(theme_color_candidates_from(
        &home_dir,
        dirs::state_dir(),
        dirs::config_dir(),
    ))
}

fn theme_color_candidates_from(
    home_dir: &Path,
    state_dir: Option<PathBuf>,
    config_dir: Option<PathBuf>,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    // Omarchy Quattro writes the active theme to this exact per-user state path.
    push_unique(
        &mut candidates,
        home_dir.join(".local/state/omarchy/current/theme/colors.toml"),
    );

    // Also honor a custom XDG state directory when it differs from Omarchy's default.
    if let Some(state_dir) = state_dir {
        push_unique(
            &mut candidates,
            state_dir.join("omarchy/current/theme/colors.toml"),
        );
    }

    // Omarchy 3.x kept the active theme below ~/.config.
    push_unique(
        &mut candidates,
        home_dir.join(".config/omarchy/current/theme/colors.toml"),
    );

    // Retain compatibility with a customized XDG config directory as a last resort.
    if let Some(config_dir) = config_dir {
        push_unique(
            &mut candidates,
            config_dir.join("omarchy/current/theme/colors.toml"),
        );
    }

    candidates
}

fn push_unique(candidates: &mut Vec<PathBuf>, path: PathBuf) {
    if !candidates.contains(&path) {
        candidates.push(path);
    }
}

fn parse_theme_color(content: &str) -> Result<String, String> {
    let colors = content
        .parse::<toml::Value>()
        .map_err(|error| format!("invalid TOML: {}", error))?;

    for key in ["rgb", "accent"] {
        if let Some(value) = colors.get(key) {
            let raw_color = value
                .as_str()
                .ok_or_else(|| format!("{} must be a string", key))?;
            let color = raw_color
                .trim()
                .strip_prefix('#')
                .unwrap_or(raw_color.trim());

            if color.len() == 6 && color.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Ok(color.to_string());
            }

            return Err(format!(
                "{} must be a six-digit hexadecimal color, got {:?}",
                key, raw_color
            ));
        }
    }

    Err("no rgb or accent color was defined".to_string())
}

#[cfg(test)]
mod tests {
    use super::{load_theme_color_from_candidates, parse_theme_color, theme_color_candidates_from};
    use std::{io, path::PathBuf};

    #[test]
    fn prefers_rgb_over_accent_when_both_exist() {
        let content = "accent = \"#112233\"\nrgb = \"#445566\"\n";
        assert_eq!(parse_theme_color(content).unwrap(), "445566");
    }

    #[test]
    fn falls_back_to_accent_when_rgb_missing() {
        let content = "accent = \"#112233\"\n";
        assert_eq!(parse_theme_color(content).unwrap(), "112233");
    }

    #[test]
    fn parses_valid_toml_instead_of_matching_line_prefixes() {
        let content = "# theme palette\n  accent = \"#Aa12fF\"\n";
        assert_eq!(parse_theme_color(content).unwrap(), "Aa12fF");
    }

    #[test]
    fn rejects_invalid_theme_colors() {
        let error = parse_theme_color("accent = \"blue\"\n").unwrap_err();
        assert!(error.contains("six-digit hexadecimal"));
    }

    #[test]
    fn checks_official_quattro_state_before_xdg_and_legacy_paths() {
        let candidates = theme_color_candidates_from(
            std::path::Path::new("/home/test"),
            Some(PathBuf::from("/tmp/state")),
            Some(PathBuf::from("/tmp/config")),
        );

        assert_eq!(
            candidates,
            vec![
                PathBuf::from("/home/test/.local/state/omarchy/current/theme/colors.toml"),
                PathBuf::from("/tmp/state/omarchy/current/theme/colors.toml"),
                PathBuf::from("/home/test/.config/omarchy/current/theme/colors.toml"),
                PathBuf::from("/tmp/config/omarchy/current/theme/colors.toml"),
            ]
        );
    }

    #[test]
    fn does_not_duplicate_default_xdg_paths() {
        let candidates = theme_color_candidates_from(
            std::path::Path::new("/home/test"),
            Some(PathBuf::from("/home/test/.local/state")),
            Some(PathBuf::from("/home/test/.config")),
        );

        assert_eq!(candidates.len(), 2);
    }

    #[test]
    fn falls_back_to_legacy_theme_when_quattro_state_is_missing() {
        let quattro = PathBuf::from("/tmp/state/omarchy/current/theme/colors.toml");
        let legacy = PathBuf::from("/tmp/config/omarchy/current/theme/colors.toml");
        let candidates = vec![quattro.clone(), legacy.clone()];
        let mut visited = Vec::new();

        let color = load_theme_color_from_candidates(&candidates, |path| {
            visited.push(path.to_path_buf());
            if path == quattro {
                Err(io::Error::from(io::ErrorKind::NotFound))
            } else {
                Ok("accent = \"#abcdef\"\n".to_string())
            }
        })
        .unwrap();

        assert_eq!(color, "abcdef");
        assert_eq!(visited, vec![quattro, legacy]);
    }

    #[test]
    fn does_not_hide_an_invalid_quattro_theme_with_a_legacy_fallback() {
        let quattro = PathBuf::from("/tmp/state/omarchy/current/theme/colors.toml");
        let legacy = PathBuf::from("/tmp/config/omarchy/current/theme/colors.toml");
        let candidates = vec![quattro.clone(), legacy];
        let mut visited = Vec::new();

        let error = load_theme_color_from_candidates(&candidates, |path| {
            visited.push(path.to_path_buf());
            Ok("accent = \"not-a-color\"\n".to_string())
        })
        .unwrap_err();

        assert!(error.contains("Invalid Omarchy theme colors"));
        assert_eq!(visited, vec![quattro]);
    }
}

pub(crate) fn hardware_lock() -> Result<File, String> {
    use std::os::unix::fs::OpenOptionsExt;
    let directory = dirs::cache_dir()
        .ok_or_else(|| "Could not determine the user cache directory".to_string())?
        .join("rgbpc");
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(directory.join("hardware.lock"))
        .map_err(|e| format!("Could not open RGBPC hardware lock: {e}"))?;
    loop {
        // Lock the complete action, including its delayed retry, not each CLI
        // command individually. Closing this File releases the advisory lock.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
            return Ok(file);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(format!("Could not lock RGBPC hardware: {error}"));
        }
    }
}

pub fn with_hardware_lock<T>(action: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let _lock = hardware_lock()?;
    action()
}

pub fn select_devices(
    devices: &[openrgb::OpenRgbDevice],
    selectors: &[String],
    config: &AppConfig,
    writable: bool,
) -> Result<Vec<openrgb::OpenRgbDevice>, String> {
    let enabled = |device: &openrgb::OpenRgbDevice| {
        !config.is_device_disabled(&openrgb::device_profile_key(device), &device.name)
    };
    if selectors.is_empty() {
        return Ok(devices
            .iter()
            .filter(|device| !writable || enabled(device))
            .cloned()
            .collect());
    }
    let mut selected = Vec::new();
    let mut seen = HashSet::new();
    for selector in selectors {
        let matches: Vec<_> = devices
            .iter()
            .filter(|device| {
                selector == &device.id.to_string()
                    || selector == &device.name
                    || selector == &openrgb::device_profile_key(device)
            })
            .collect();
        if matches.len() != 1 {
            return Err(format!("Device selector {selector:?} matched {} devices; use an exact ID, name, or key from --list-devices", matches.len()));
        }
        let device = matches[0];
        if writable && !enabled(device) {
            return Err(format!(
                "{} is disabled in RGBPC; enable it before changing its lighting",
                device.name
            ));
        }
        if seen.insert(device.id) {
            selected.push(device.clone());
        }
    }
    Ok(selected)
}

pub fn manual_cli_action(
    state: SavedState,
    selectors: &[String],
) -> Result<DeviceActionSummary, String> {
    let _lock = hardware_lock()?;
    let config = AppConfig::load();
    let current = openrgb::list_devices()?;
    let devices = select_devices(&current, selectors, &config, true)?;
    if devices.is_empty() {
        return Err("No enabled OpenRGB devices were selected".to_string());
    }
    // Retries resolve by profile identity. Never let a subset of identical
    // controllers accidentally broaden to all matching controllers on retry.
    for device in &devices {
        let key = openrgb::device_profile_key(device);
        let total = current
            .iter()
            .filter(|d| openrgb::device_profile_key(d) == key)
            .count();
        let selected = devices
            .iter()
            .filter(|d| openrgb::device_profile_key(d) == key)
            .count();
        if selected != total {
            return Err(format!("{} has an ambiguous controller identity; cannot safely target only part of this identical-controller group", device.name));
        }
    }
    let mut summary = match &state {
        SavedState::Color { hex } => apply_color_to_devices_summary(devices, hex, true),
        SavedState::Off => apply_color_to_devices_summary(devices, "000000", true),
        SavedState::Rainbow => set_rainbow_for_devices_summary(devices, true),
    };
    persist_manual_state(&mut summary, state);
    summary.succeeded_keys.sort();
    summary.failed_devices.sort();
    Ok(summary)
}

pub fn apply_color_to_selected_devices_summary(
    mut devices: Vec<openrgb::OpenRgbDevice>,
    color: &str,
    state: SavedState,
) -> DeviceActionSummary {
    with_hardware_lock(|| {
        openrgb::refresh_device_ids(&mut devices)?;
        let mut summary = apply_color_to_devices_summary(devices, color, true);
        persist_manual_state(&mut summary, state);
        Ok(summary)
    })
    .unwrap_or_else(|error| DeviceActionSummary::from_counts(Vec::new(), vec![error]))
}

pub fn set_rainbow_for_selected_devices_summary(
    mut devices: Vec<openrgb::OpenRgbDevice>,
) -> DeviceActionSummary {
    with_hardware_lock(|| {
        openrgb::refresh_device_ids(&mut devices)?;
        let mut summary = set_rainbow_for_devices_summary(devices, true);
        persist_manual_state(&mut summary, SavedState::Rainbow);
        Ok(summary)
    })
    .unwrap_or_else(|error| DeviceActionSummary::from_counts(Vec::new(), vec![error]))
}

fn persist_manual_state(summary: &mut DeviceActionSummary, state: SavedState) {
    if !summary.is_any_success() {
        return;
    }
    // Called while the complete hardware action still owns the lock. Read the
    // current config, not the TUI snapshot from before another hook completed.
    let mut config = AppConfig::load();
    config.set_saved_state_for_devices(&summary.succeeded_keys, state);
    config.remove_omarchy_sync_devices(&summary.succeeded_keys);
    if let Err(error) = config.save() {
        summary
            .failed_devices
            .push(format!("Could not save RGB state: {error}"));
    }
}

pub fn perform_sync(force: bool) -> Result<(), String> {
    perform_sync_summary(force)?.into_result()
}

pub fn perform_sync_summary(force: bool) -> Result<DeviceActionSummary, String> {
    let _lock = hardware_lock()?;
    let mut conf = AppConfig::load();
    if !conf.omarchy_sync_enabled && !force {
        return Ok(DeviceActionSummary::from_counts(Vec::new(), Vec::new()));
    }

    // Existing 3.x installations may invoke us from the former flat hook. Migrate
    // only when needed so normal Quattro theme events do not rewrite hook config.
    if conf.omarchy_sync_enabled {
        hook::migrate_hook_if_needed()?;
    }

    let hex_color = load_theme_color()?;

    let devices = openrgb::list_devices()?;

    let enabled_devices: Vec<_> = devices
        .into_iter()
        .filter(|device| sync_target_contains(&conf, device))
        .collect();

    let mut summary = apply_color_to_devices_summary(enabled_devices, &hex_color, true);

    if summary.is_any_success() {
        conf.set_saved_state_for_devices(
            &summary.succeeded_keys,
            SavedState::Color {
                hex: hex_color.clone(),
            },
        );
        if let Err(error) = conf.save() {
            summary
                .failed_devices
                .push(format!("Could not save RGB state: {error}"));
        }
    }

    Ok(summary)
}

pub fn perform_restore(fast: bool) -> Result<(), String> {
    let _lock = hardware_lock()?;
    let conf = AppConfig::load();
    if !conf.restore_on_startup {
        return Ok(());
    }

    validate_restore_config(&conf)?;

    let delay = Duration::from_millis(conf.startup_delay_ms);
    let mut last_error = None;

    for attempt in 0..2 {
        if attempt == 0 {
            thread::sleep(delay);
        } else {
            thread::sleep(Duration::from_millis(1000));
        }

        // Honor login delay and enabled/state checks before detecting hardware.
        // A failed outer attempt gets a fresh server/detection next time.
        let _session = if fast {
            match session::OpenRgbSession::start_locked() {
                Ok(session) => session,
                Err(error) => {
                    openrgb::set_server_mode(false);
                    if session::is_unavailable(&error) {
                        return Err(error);
                    }
                    eprintln!("Fast mode unavailable: {error}. Using standalone OpenRGB.");
                    None
                }
            }
        } else {
            None
        };
        match restore_once(&conf) {
            Ok(()) => return Ok(()),
            Err(err) => last_error = Some(err),
        }
    }

    Err(last_error.unwrap_or_else(|| "Startup restore failed".to_string()))
}

fn restore_once(conf: &AppConfig) -> Result<(), String> {
    let devices = openrgb::list_devices()?;
    if devices.is_empty() {
        return Err("No OpenRGB devices available for startup restore".to_string());
    }

    let mut omarchy_devices = Vec::new();
    let mut saved_state_devices = Vec::new();

    for device in devices {
        let profile_key = openrgb::device_profile_key(&device);
        if conf.omarchy_sync_enabled && conf.omarchy_sync_devices.contains(&profile_key) {
            omarchy_devices.push(device);
        } else {
            saved_state_devices.push(device);
        }
    }

    let mut failures = Vec::new();

    if conf.omarchy_sync_enabled && !omarchy_devices.is_empty() {
        match restore_theme_to_devices(omarchy_devices) {
            Ok(()) => {}
            Err(err) => failures.push(err),
        }
    }

    match restore_saved_device_states(conf, saved_state_devices) {
        Ok(()) => {}
        Err(err) => failures.push(err),
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join(" | "))
    }
}

fn validate_restore_config(conf: &AppConfig) -> Result<(), String> {
    if conf.omarchy_sync_enabled
        || !conf.saved_device_states.is_empty()
        || conf.last_state.is_some()
    {
        Ok(())
    } else {
        Err("No saved RGB state to restore yet. Set a manual color or turn lights off once before using startup restore.".to_string())
    }
}

fn sync_target_contains(conf: &AppConfig, device: &openrgb::OpenRgbDevice) -> bool {
    let profile_key = openrgb::device_profile_key(device);
    if conf.omarchy_sync_enabled {
        conf.omarchy_sync_devices.contains(&profile_key)
    } else {
        !conf.is_device_disabled(&profile_key, &device.name)
    }
}

fn restore_saved_device_states(
    conf: &AppConfig,
    devices: Vec<openrgb::OpenRgbDevice>,
) -> Result<(), String> {
    let legacy_state = conf.last_state.clone();
    let mut color_groups: HashMap<String, Vec<openrgb::OpenRgbDevice>> = HashMap::new();
    let mut off_devices = Vec::new();
    let mut rainbow_devices = Vec::new();

    for device in devices {
        let profile_key = openrgb::device_profile_key(&device);
        let state = conf
            .get_saved_state_for_device(&profile_key)
            .cloned()
            .or_else(|| {
                if conf.is_device_disabled(&profile_key, &device.name) {
                    None
                } else {
                    legacy_state.clone()
                }
            });

        match state {
            Some(SavedState::Color { hex }) => color_groups.entry(hex).or_default().push(device),
            Some(SavedState::Off) => off_devices.push(device),
            Some(SavedState::Rainbow) => rainbow_devices.push(device),
            None => {}
        }
    }

    let mut failures = Vec::new();

    for (hex, grouped_devices) in color_groups {
        let summary = apply_color_to_devices_summary(grouped_devices, &hex, false);
        if !summary.failed_devices.is_empty() {
            failures.push(summary.failed_devices.join(" | "));
        }
    }

    if !off_devices.is_empty() {
        let summary = apply_color_to_devices_summary(off_devices, "000000", false);
        if !summary.failed_devices.is_empty() {
            failures.push(summary.failed_devices.join(" | "));
        }
    }

    if !rainbow_devices.is_empty() {
        let summary = set_rainbow_for_devices_summary(rainbow_devices, false);
        if !summary.failed_devices.is_empty() {
            failures.push(summary.failed_devices.join(" | "));
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join(" | "))
    }
}

fn restore_theme_to_devices(devices: Vec<openrgb::OpenRgbDevice>) -> Result<(), String> {
    let hex_color = load_theme_color()?;
    let summary = apply_color_to_devices_summary(devices, &hex_color, false);

    if summary.failed_devices.is_empty() {
        Ok(())
    } else {
        Err(summary.failed_devices.join(" | "))
    }
}

pub fn apply_color_to_devices_summary(
    devices: Vec<openrgb::OpenRgbDevice>,
    color: &str,
    fail_on_partial: bool,
) -> DeviceActionSummary {
    let mut device_results = HashMap::new();
    let mut retry_keys = HashSet::new();

    for device in devices {
        let key = openrgb::device_profile_key(&device);
        let name = device.name.clone();

        match openrgb::apply_color(&device, color) {
            Ok(result) => {
                device_results.insert(
                    key.clone(),
                    DeviceApplyStatus {
                        name,
                        success: true,
                        failure: None,
                    },
                );
                if result.needs_retry {
                    retry_keys.insert(key);
                }
            }
            Err(err) => {
                device_results.insert(
                    key.clone(),
                    DeviceApplyStatus {
                        name,
                        success: false,
                        failure: Some(err),
                    },
                );
                retry_keys.insert(key);
            }
        }
    }

    retry_color_for_profile_keys(&retry_keys, color, &mut device_results);

    let mut failures = Vec::new();
    let mut succeeded_keys = Vec::new();

    for (key, status) in device_results {
        if status.success {
            succeeded_keys.push(key);
        } else if let Some(err) = status.failure {
            failures.push(format!("{}: {}", status.name, err));
        }
    }

    if fail_on_partial && !failures.is_empty() && succeeded_keys.is_empty() {
        return DeviceActionSummary::from_counts(Vec::new(), failures);
    }

    if fail_on_partial && !failures.is_empty() && !succeeded_keys.is_empty() {
        return DeviceActionSummary::from_counts(succeeded_keys, failures);
    }

    DeviceActionSummary::from_counts(succeeded_keys, failures)
}

fn retry_color_for_profile_keys(
    profile_keys: &HashSet<String>,
    color: &str,
    device_results: &mut HashMap<String, DeviceApplyStatus>,
) {
    if profile_keys.is_empty() {
        return;
    }

    thread::sleep(Duration::from_millis(1500));

    let Ok(devices) = openrgb::list_devices() else {
        return;
    };

    for device in devices {
        let key = openrgb::device_profile_key(&device);
        if !profile_keys.contains(&key) {
            continue;
        }

        if openrgb::apply_color(&device, color).is_ok() {
            let entry = device_results
                .entry(key)
                .or_insert_with(|| DeviceApplyStatus {
                    name: device.name.clone(),
                    success: false,
                    failure: None,
                });
            entry.name = device.name.clone();
            entry.success = true;
            entry.failure = None;
        }
    }
}

struct DeviceApplyStatus {
    name: String,
    success: bool,
    failure: Option<String>,
}

pub fn set_rainbow_for_devices_summary(
    devices: Vec<openrgb::OpenRgbDevice>,
    fail_on_partial: bool,
) -> DeviceActionSummary {
    let mut failures = Vec::new();
    let mut succeeded_keys = Vec::new();

    for device in devices {
        let key = openrgb::device_profile_key(&device);
        match openrgb::set_rainbow(&device) {
            Ok(()) => succeeded_keys.push(key),
            Err(err) => failures.push(err),
        }
    }

    if fail_on_partial && !failures.is_empty() && succeeded_keys.is_empty() {
        return DeviceActionSummary::from_counts(Vec::new(), failures);
    }

    DeviceActionSummary::from_counts(succeeded_keys, failures)
}

impl DeviceActionSummary {
    fn into_result(self) -> Result<(), String> {
        if self.failed_devices.is_empty() {
            Ok(())
        } else {
            Err(self.failed_devices.join(" | "))
        }
    }
}

#[cfg(test)]
mod restore_tests {
    use super::{
        apply_color_to_devices_summary, restore_theme_to_devices, set_rainbow_for_devices_summary,
        sync_target_contains, validate_restore_config, AppConfig,
    };
    use crate::core::config::SavedState;
    use crate::core::openrgb::OpenRgbDevice;

    #[test]
    fn cli_selector_never_broadens_a_missing_or_disabled_target() {
        let devices = vec![OpenRgbDevice {
            id: 7,
            name: "Generic Board".to_string(),
            device_type: "Motherboard".to_string(),
        }];
        let mut config = AppConfig::default();
        assert!(super::select_devices(&devices, &["missing".to_string()], &config, true).is_err());
        assert!(super::select_devices(&devices, &["07".to_string()], &config, true).is_err());
        config.disabled_devices.insert("Generic Board".to_string());
        assert!(super::select_devices(&devices, &["7".to_string()], &config, true).is_err());
        assert_eq!(
            super::select_devices(&devices, &[], &config, false)
                .unwrap()
                .len(),
            1
        );
        assert!(super::select_devices(&devices, &[], &config, true)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn cli_selector_is_exact_deduplicated_and_ambiguity_is_an_error() {
        let devices = vec![
            OpenRgbDevice {
                id: 7,
                name: "Board".to_string(),
                device_type: "Motherboard".to_string(),
            },
            OpenRgbDevice {
                id: 8,
                name: "Board Pro".to_string(),
                device_type: "Motherboard".to_string(),
            },
        ];
        let config = AppConfig::default();
        let selected = super::select_devices(
            &devices,
            &[
                "Board".to_string(),
                "7".to_string(),
                "board::motherboard".to_string(),
            ],
            &config,
            true,
        )
        .unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].id, 7);
        assert!(super::select_devices(&devices, &["board".to_string()], &config, true).is_err());
        let mut ambiguous = devices;
        ambiguous[1].name = "Board".to_string();
        assert!(super::select_devices(&ambiguous, &["Board".to_string()], &config, true).is_err());
    }

    #[test]
    fn apply_color_to_devices_succeeds_for_empty_list() {
        assert!(apply_color_to_devices_summary(Vec::new(), "ffffff", true)
            .failed_devices
            .is_empty());
    }

    #[test]
    fn set_rainbow_for_devices_succeeds_for_empty_list() {
        assert!(set_rainbow_for_devices_summary(Vec::new(), true)
            .failed_devices
            .is_empty());
    }

    #[test]
    fn restore_requires_saved_state_when_omarchy_sync_is_off() {
        let config = AppConfig {
            restore_on_startup: true,
            ..AppConfig::default()
        };

        assert!(validate_restore_config(&config).is_err());
    }

    #[test]
    fn restore_allows_missing_saved_state_when_omarchy_sync_is_on() {
        let config = AppConfig {
            restore_on_startup: true,
            omarchy_sync_enabled: true,
            ..AppConfig::default()
        };

        assert!(validate_restore_config(&config).is_ok());
    }

    #[test]
    fn omarchy_sync_does_not_fall_back_to_live_checkbox_state() {
        let mut config = AppConfig {
            omarchy_sync_enabled: true,
            ..AppConfig::default()
        };
        config.disabled_devices.clear();

        let device = OpenRgbDevice {
            id: 1,
            name: "Keyboard".to_string(),
            device_type: "Keyboard".to_string(),
        };

        assert!(!sync_target_contains(&config, &device));
    }

    #[test]
    fn mixed_restore_scope_routes_synced_and_manual_devices_differently() {
        let mut config = AppConfig {
            omarchy_sync_enabled: true,
            ..AppConfig::default()
        };
        config.set_omarchy_sync_devices(&["mouse::mouse".to_string()]);
        config.saved_device_states.insert(
            "motherboard::motherboard".to_string(),
            SavedState::Color {
                hex: "ff0000".to_string(),
            },
        );

        let mouse = OpenRgbDevice {
            id: 1,
            name: "Mouse".to_string(),
            device_type: "Mouse".to_string(),
        };
        let motherboard = OpenRgbDevice {
            id: 2,
            name: "Motherboard".to_string(),
            device_type: "Motherboard".to_string(),
        };

        assert!(sync_target_contains(&config, &mouse));
        assert!(!sync_target_contains(&config, &motherboard));
        assert!(config
            .get_saved_state_for_device("motherboard::motherboard")
            .is_some());
    }

    #[test]
    fn restore_theme_to_devices_succeeds_for_empty_list() {
        assert_eq!(restore_theme_to_devices(Vec::new()), Ok(()));
    }
}
