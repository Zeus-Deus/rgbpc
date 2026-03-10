use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

const DEFAULT_STARTUP_DELAY_MS: u64 = 1500;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SavedState {
    Color { hex: String },
    Off,
    Rainbow,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub omarchy_sync_enabled: bool,
    pub omarchy_sync_devices: HashSet<String>,
    pub restore_on_startup: bool,
    pub startup_delay_ms: u64,
    pub disabled_devices: HashSet<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub saved_device_states: HashMap<String, SavedState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_state: Option<SavedState>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            omarchy_sync_enabled: false,
            omarchy_sync_devices: HashSet::new(),
            restore_on_startup: false,
            startup_delay_ms: DEFAULT_STARTUP_DELAY_MS,
            disabled_devices: HashSet::new(),
            saved_device_states: HashMap::new(),
            last_state: None,
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

    pub fn set_saved_state_for_devices(&mut self, device_keys: &[String], state: SavedState) {
        for device_key in device_keys {
            self.saved_device_states
                .insert(device_key.clone(), state.clone());
        }

        self.last_state = None;
    }

    pub fn get_saved_state_for_device(&self, device_key: &str) -> Option<&SavedState> {
        self.saved_device_states.get(device_key)
    }

    pub fn set_omarchy_sync_devices(&mut self, device_keys: &[String]) {
        self.omarchy_sync_devices = device_keys.iter().cloned().collect();
    }

    pub fn remove_omarchy_sync_devices(&mut self, device_keys: &[String]) {
        for device_key in device_keys {
            self.omarchy_sync_devices.remove(device_key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AppConfig, SavedState, DEFAULT_STARTUP_DELAY_MS};

    #[test]
    fn legacy_config_fields_default_when_missing() {
        let config: AppConfig = toml::from_str("omarchy_sync_enabled = true\n").unwrap();

        assert!(config.omarchy_sync_enabled);
        assert!(config.omarchy_sync_devices.is_empty());
        assert!(!config.restore_on_startup);
        assert_eq!(config.startup_delay_ms, DEFAULT_STARTUP_DELAY_MS);
        assert!(config.saved_device_states.is_empty());
        assert_eq!(config.last_state, None);
    }

    #[test]
    fn saved_state_color_serializes_as_tagged_toml() {
        let config = AppConfig {
            last_state: Some(SavedState::Color {
                hex: "ffaa00".to_string(),
            }),
            ..AppConfig::default()
        };

        let content = toml::to_string(&config).unwrap();

        assert!(content.contains("kind = \"color\""));
        assert!(content.contains("hex = \"ffaa00\""));
    }

    #[test]
    fn saved_device_states_are_serialized_as_tables() {
        let mut config = AppConfig::default();
        config
            .saved_device_states
            .insert("msi::motherboard".to_string(), SavedState::Rainbow);

        let content = toml::to_string(&config).unwrap();

        assert!(content.contains("saved_device_states"));
        assert!(content.contains("msi::motherboard"));
        assert!(content.contains("kind = \"rainbow\""));
    }

    #[test]
    fn omarchy_sync_device_scope_can_be_set_and_removed_incrementally() {
        let mut config = AppConfig::default();
        config.set_omarchy_sync_devices(&[
            "gpu::gpu".to_string(),
            "keyboard::keyboard".to_string(),
            "mouse::mouse".to_string(),
        ]);

        config.remove_omarchy_sync_devices(&["keyboard::keyboard".to_string()]);

        assert!(config.omarchy_sync_devices.contains("gpu::gpu"));
        assert!(config.omarchy_sync_devices.contains("mouse::mouse"));
        assert!(!config.omarchy_sync_devices.contains("keyboard::keyboard"));
    }
}
