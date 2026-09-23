use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Persisted user preferences. Stored as JSON in the platform config
/// directory (e.g. `%APPDATA%\Voyis\NetworkSpeed\settings.json` on Windows).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppSettings {
    /// Explicit iperf3 executable path chosen by the user, if any.
    /// When absent, the app falls back to the bundled copy / PATH.
    pub iperf3_path: Option<String>,
}

impl AppSettings {
    fn file_path() -> Option<PathBuf> {
        directories::ProjectDirs::from("com", "Voyis", "NetworkSpeed")
            .map(|dirs| dirs.config_dir().join("settings.json"))
    }

    pub fn load() -> Self {
        let path = match Self::file_path() {
            Some(path) => path,
            None => return Self::default(),
        };

        let contents = std::fs::read_to_string(path).unwrap_or_default();

        serde_json::from_str(&contents).unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(path) = Self::file_path() else {
            return;
        };

        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        if let Ok(contents) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, contents);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_through_json() {
        let settings = AppSettings {
            iperf3_path: Some("C:\\tools\\iperf3.exe".to_string()),
        };

        let json = serde_json::to_string(&settings).unwrap();
        let restored: AppSettings = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.iperf3_path, settings.iperf3_path);
    }

    #[test]
    fn corrupt_settings_fall_back_to_default() {
        let restored: AppSettings = serde_json::from_str("{ not valid json").unwrap_or_default();

        assert_eq!(restored.iperf3_path, None);
    }
}
