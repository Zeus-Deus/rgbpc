pub mod config;
pub mod hook;
pub mod openrgb;

use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

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
    let conf = config::AppConfig::load();
    if !conf.omarchy_sync_enabled && !force {
        return Ok(());
    }

    let hex_color = load_theme_color()?;

    let mut devices = openrgb::list_devices()?;
    let _ = openrgb::refresh_device_ids(&mut devices);

    let mut retry_devices = Vec::new();

    // Apply colors immediately
    for device in &devices {
        let profile_key = openrgb::device_profile_key(device);
        if !conf.is_device_disabled(&profile_key, &device.name) {
            match openrgb::apply_color(device, &hex_color) {
                Ok(result) => {
                    if result.needs_retry {
                        retry_devices.push(device.clone());
                    }
                }
                Err(_) => {
                    retry_devices.push(device.clone());
                }
            }
        }
    }

    if retry_devices.is_empty() {
        return Ok(());
    }

    // Some boards/devices need a second application to "stick"
    let hex_color_clone = hex_color.clone();
    let devices_clone = retry_devices;
    let disabled_clone = conf.disabled_devices.clone();

    thread::spawn(move || {
        thread::sleep(Duration::from_millis(1500));
        for device in &devices_clone {
            let profile_key = openrgb::device_profile_key(device);
            let is_disabled =
                disabled_clone.contains(&device.name) || disabled_clone.contains(&profile_key);
            if !is_disabled {
                let _ = openrgb::apply_color(device, &hex_color_clone);
            }
        }
    });

    Ok(())
}
