pub mod config;
pub mod hook;
pub mod openrgb;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
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
    let mut theme_path = dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
    theme_path.push(".config/omarchy/current/theme/colors.toml");

    let content = fs::read_to_string(&theme_path)
        .map_err(|e| format!("Failed to read colors.toml: {}", e))?;

    parse_theme_color(&content).ok_or_else(|| "No color found in theme".to_string())
}

fn parse_theme_color(content: &str) -> Option<String> {
    let mut accent_color = None;

    for line in content.lines() {
        if line.starts_with("rgb") || line.starts_with("accent") {
            let parts: Vec<&str> = line.split('=').collect();
            if parts.len() == 2 {
                let color = parts[1]
                    .trim()
                    .trim_matches('"')
                    .trim_start_matches('#')
                    .to_string();
                if !color.is_empty() {
                    if line.starts_with("rgb") {
                        return Some(color);
                    }
                    if accent_color.is_none() {
                        accent_color = Some(color);
                    }
                }
            }
        }
    }

    accent_color
}

#[cfg(test)]
mod tests {
    use super::parse_theme_color;

    #[test]
    fn prefers_rgb_over_accent_when_both_exist() {
        let content = "accent = \"#112233\"\nrgb = \"#445566\"\n";
        assert_eq!(parse_theme_color(content), Some("445566".to_string()));
    }

    #[test]
    fn falls_back_to_accent_when_rgb_missing() {
        let content = "accent = \"#112233\"\n";
        assert_eq!(parse_theme_color(content), Some("112233".to_string()));
    }
}

pub fn perform_sync(force: bool) -> Result<(), String> {
    let mut conf = AppConfig::load();
    if !conf.omarchy_sync_enabled && !force {
        return Ok(());
    }

    let hex_color = load_theme_color()?;

    let mut devices = openrgb::list_devices()?;
    let _ = openrgb::refresh_device_ids(&mut devices);

    let enabled_devices: Vec<_> = devices
        .into_iter()
        .filter(|device| {
            let profile_key = openrgb::device_profile_key(device);
            !conf.is_device_disabled(&profile_key, &device.name)
        })
        .collect();

    let summary = apply_color_to_devices_summary(enabled_devices, &hex_color, true);

    if summary.is_any_success() {
        conf.set_saved_state_for_devices(
            &summary.succeeded_keys,
            SavedState::Color {
                hex: hex_color.clone(),
            },
        );
        let _ = conf.save();
    }

    summary.into_result()
}

pub fn perform_restore() -> Result<(), String> {
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

        match restore_once(&conf) {
            Ok(()) => return Ok(()),
            Err(err) => last_error = Some(err),
        }
    }

    Err(last_error.unwrap_or_else(|| "Startup restore failed".to_string()))
}

fn restore_once(conf: &AppConfig) -> Result<(), String> {
    let mut devices = openrgb::list_devices()?;
    if devices.is_empty() {
        return Err("No OpenRGB devices available for startup restore".to_string());
    }

    let _ = openrgb::refresh_device_ids(&mut devices);

    if conf.omarchy_sync_enabled {
        let managed_devices: Vec<_> = devices
            .into_iter()
            .filter(|device| {
                let profile_key = openrgb::device_profile_key(device);
                !conf.is_device_disabled(&profile_key, &device.name)
            })
            .collect();

        if managed_devices.is_empty() {
            return Ok(());
        }

        return apply_theme_to_devices(managed_devices, false);
    }

    restore_saved_device_states(conf, devices)
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
    let mut successes = 0;

    for (hex, grouped_devices) in color_groups {
        match apply_color_to_devices_lenient(grouped_devices, &hex) {
            Ok(()) => successes += 1,
            Err(err) => failures.push(err),
        }
    }

    if !off_devices.is_empty() {
        match apply_color_to_devices_lenient(off_devices, "000000") {
            Ok(()) => successes += 1,
            Err(err) => failures.push(err),
        }
    }

    if !rainbow_devices.is_empty() {
        match set_rainbow_for_devices_lenient(rainbow_devices) {
            Ok(()) => successes += 1,
            Err(err) => failures.push(err),
        }
    }

    if failures.is_empty() || successes > 0 {
        Ok(())
    } else {
        Err(failures.join(" | "))
    }
}

fn apply_theme_to_devices(
    devices: Vec<openrgb::OpenRgbDevice>,
    fail_on_partial: bool,
) -> Result<(), String> {
    let hex_color = load_theme_color()?;
    if fail_on_partial {
        apply_color_to_devices(devices, &hex_color)
    } else {
        apply_color_to_devices_lenient(devices, &hex_color)
    }
}

pub fn apply_color_to_devices(
    devices: Vec<openrgb::OpenRgbDevice>,
    color: &str,
) -> Result<(), String> {
    apply_color_to_devices_summary(devices, color, true).into_result()
}

fn apply_color_to_devices_lenient(
    devices: Vec<openrgb::OpenRgbDevice>,
    color: &str,
) -> Result<(), String> {
    apply_color_to_devices_summary(devices, color, false).into_result()
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

    let Ok(mut devices) = openrgb::list_devices() else {
        return;
    };
    let _ = openrgb::refresh_device_ids(&mut devices);

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

fn set_rainbow_for_devices_lenient(devices: Vec<openrgb::OpenRgbDevice>) -> Result<(), String> {
    set_rainbow_for_devices_summary(devices, false).into_result()
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
        if self.failed_devices.is_empty() || self.is_any_success() {
            Ok(())
        } else {
            Err(self.failed_devices.join(" | "))
        }
    }
}

#[cfg(test)]
mod restore_tests {
    use super::{
        apply_color_to_devices, set_rainbow_for_devices_summary, validate_restore_config, AppConfig,
    };

    #[test]
    fn apply_color_to_devices_succeeds_for_empty_list() {
        assert_eq!(apply_color_to_devices(Vec::new(), "ffffff"), Ok(()));
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
}
