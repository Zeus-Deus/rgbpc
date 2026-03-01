pub mod openrgb;
pub mod config;
pub mod hook;

use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

pub fn perform_sync(force: bool) -> Result<(), String> {
    let conf = config::AppConfig::load();
    if !conf.omarchy_sync_enabled && !force {
        return Ok(());
    }

    // Get theme color
    let mut theme_path = dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
    theme_path.push(".config/omarchy/current/theme/colors.toml");

    let content = fs::read_to_string(&theme_path)
        .map_err(|e| format!("Failed to read colors.toml: {}", e))?;
    
    let mut hex_color = String::new();
    
    for line in content.lines() {
        if line.starts_with("rgb") || line.starts_with("accent") {
            let parts: Vec<&str> = line.split('=').collect();
            if parts.len() == 2 {
                hex_color = parts[1].trim().trim_matches('"').trim_start_matches('#').to_string();
                if line.starts_with("rgb") {
                    break; // Prefer rgb
                }
            }
        }
    }

    if hex_color.is_empty() {
        return Err("No color found in theme".to_string());
    }

    let devices = openrgb::list_devices()?;
    
    // Apply colors immediately
    for device in &devices {
        if !conf.disabled_devices.contains(&device.name) {
            let _ = openrgb::set_color(device.id, &hex_color);
        }
    }

    // MSI boards often need a second application to "stick"
    let hex_color_clone = hex_color.clone();
    let devices_clone = devices.clone();
    let disabled_clone = conf.disabled_devices.clone();
    
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(1500));
        for device in &devices_clone {
            if !disabled_clone.contains(&device.name) {
                let _ = openrgb::set_color(device.id, &hex_color_clone);
            }
        }
    });

    Ok(())
}
