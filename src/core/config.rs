use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub omarchy_sync_enabled: bool,
    pub disabled_devices: HashSet<String>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            omarchy_sync_enabled: false,
            disabled_devices: HashSet::new(),
        }
    }
}

impl AppConfig {
    fn get_path() -> PathBuf {
        let mut path = dirs::config_dir().unwrap_or_else(|| PathBuf::from("~/.config"));
        path.push("rgbpc");
        fs::create_dir_all(&path).ok();
        path.push("config.toml");
        path
    }

    pub fn load() -> Self {
        let path = Self::get_path();
        if let Ok(content) = fs::read_to_string(&path) {
            if let Ok(config) = toml::from_str(&content) {
                return config;
            }
        }
        Self::default()
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::get_path();
        let content = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        fs::write(path, content).map_err(|e| e.to_string())
    }

    pub fn is_device_disabled(&self, device_key: &str, device_name: &str) -> bool {
        self.disabled_devices.contains(device_key) || self.disabled_devices.contains(device_name)
    }

    pub fn set_device_disabled(&mut self, device_key: &str, device_name: &str, disabled: bool) {
        self.disabled_devices.remove(device_name);
        self.disabled_devices.remove(device_key);

        if disabled {
            self.disabled_devices.insert(device_key.to_string());
        }
    }
}
