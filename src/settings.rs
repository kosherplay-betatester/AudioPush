//! Remembered choices, kept between runs.
//!
//! A plain `key=value` file in `%APPDATA%\AudioPush\settings.txt`. It holds
//! only preferences, so a missing, unreadable or corrupt file is never an
//! error: every field falls back to its default and the app carries on.

use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;

use crate::config::Latency;

#[derive(Clone, Debug, Default)]
pub struct Settings {
    /// The receiver last streamed to, offered again on the next run.
    pub last_target: Option<SocketAddr>,
    /// Its hostname at the time, for a friendlier label than a bare IP.
    pub last_target_name: String,
    pub preset: Option<Latency>,
    /// WASAPI endpoint ids; `None` means "follow the Windows default".
    pub capture_device: Option<String>,
    pub playback_device: Option<String>,
}

impl Settings {
    fn path() -> Option<PathBuf> {
        let appdata = std::env::var("APPDATA").ok()?;
        Some(PathBuf::from(appdata).join("AudioPush").join("settings.txt"))
    }

    pub fn load() -> Settings {
        let mut settings = Settings::default();
        let Some(path) = Settings::path() else {
            return settings;
        };
        let Ok(text) = fs::read_to_string(path) else {
            return settings;
        };

        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            if value.is_empty() {
                continue;
            }
            match key.trim() {
                "last_target" => settings.last_target = value.parse().ok(),
                "last_target_name" => settings.last_target_name = value.to_string(),
                "preset" => {
                    settings.preset = match value {
                        "Low" => Some(Latency::Low),
                        "Balanced" => Some(Latency::Balanced),
                        "Stable" => Some(Latency::Stable),
                        _ => None,
                    }
                }
                "capture_device" => settings.capture_device = Some(value.to_string()),
                "playback_device" => settings.playback_device = Some(value.to_string()),
                _ => {}
            }
        }
        settings
    }

    /// Best-effort write. Losing a preference is not worth interrupting the
    /// user over, so failures are swallowed.
    pub fn save(&self) {
        let Some(path) = Settings::path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        let mut text = String::new();
        if let Some(target) = self.last_target {
            text.push_str(&format!("last_target={target}\n"));
        }
        if !self.last_target_name.is_empty() {
            text.push_str(&format!("last_target_name={}\n", self.last_target_name));
        }
        if let Some(preset) = self.preset {
            text.push_str(&format!("preset={}\n", preset.label()));
        }
        if let Some(device) = &self.capture_device {
            text.push_str(&format!("capture_device={device}\n"));
        }
        if let Some(device) = &self.playback_device {
            text.push_str(&format!("playback_device={device}\n"));
        }

        let _ = fs::write(path, text);
    }

    /// Forgets the saved receiver, leaving device and preset choices alone.
    pub fn forget_target(&mut self) {
        self.last_target = None;
        self.last_target_name.clear();
        self.save();
    }
}
