//! Remembered choices, kept between runs.
//!
//! A plain `key=value` file in `%APPDATA%\AudioPush\settings.txt`. It holds
//! only preferences, so a missing, unreadable or corrupt file is never an
//! error: every field falls back to its default and the app carries on.

use std::fs;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use crate::config::Latency;
use crate::pairing::Token;

/// How many previously used receivers to keep, most recent first.
pub const RECENT_LIMIT: usize = 5;

/// A receiver streamed to before.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recent {
    pub addr: SocketAddr,
    /// Its hostname at the time, for a friendlier label than a bare IP.
    pub name: String,
}

impl Recent {
    /// What to show in the list.
    pub fn label(&self) -> String {
        if self.name.is_empty() {
            self.addr.ip().to_string()
        } else {
            format!("{}  —  {}", self.name, self.addr.ip())
        }
    }
}

/// A machine this one has paired with, and the secret they share.
#[derive(Clone, Debug)]
pub struct Pairing {
    pub ip: IpAddr,
    pub name: String,
    pub token: Token,
}

#[derive(Clone, Debug, Default)]
pub struct Settings {
    /// Paired machines. The same list serves both roles: a sender looks up
    /// the token for the receiver it is calling, a receiver trusts every
    /// token it has ever issued.
    pub pairings: Vec<Pairing>,
    /// Receivers used before, most recent first, at most [`RECENT_LIMIT`].
    pub recent: Vec<Recent>,
    pub preset: Option<Latency>,
    /// WASAPI endpoint ids; `None` means "follow the Windows default".
    pub capture_device: Option<String>,
    pub playback_device: Option<String>,
    /// Return microphone stream.
    pub mic_enabled: bool,
    /// Microphone endpoint to capture from, when receiving.
    pub mic_input_device: Option<String>,
    /// Output endpoint to play the incoming microphone into, when sending.
    pub mic_output_device: Option<String>,
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

        // Carried over from the single-entry format, so an older settings
        // file still restores the last receiver instead of losing it.
        let mut legacy_addr: Option<SocketAddr> = None;
        let mut legacy_name = String::new();

        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            if value.is_empty() {
                continue;
            }
            match key.trim() {
                "recent" => {
                    // "name|addr"; the name may be empty, the address may not.
                    let (name, addr) = match value.split_once('|') {
                        Some((name, addr)) => (name, addr),
                        None => ("", value),
                    };
                    if let Ok(addr) = addr.parse() {
                        settings.push_recent(Recent {
                            addr,
                            name: name.to_string(),
                        });
                    }
                }
                "last_target" => legacy_addr = value.parse().ok(),
                "last_target_name" => legacy_name = value.to_string(),
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
                "mic_enabled" => settings.mic_enabled = value == "true",
                "mic_input_device" => settings.mic_input_device = Some(value.to_string()),
                "mic_output_device" => settings.mic_output_device = Some(value.to_string()),
                "pair" => {
                    // "ip|name|token-hex"
                    let parts: Vec<&str> = value.splitn(3, '|').collect();
                    if parts.len() == 3 {
                        if let (Ok(ip), Some(token)) =
                            (parts[0].parse::<IpAddr>(), Token::from_hex(parts[2]))
                        {
                            settings.pairings.push(Pairing {
                                ip,
                                name: parts[1].to_string(),
                                token,
                            });
                        }
                    }
                }
                _ => {}
            }
        }

        if let Some(addr) = legacy_addr {
            settings.push_recent(Recent {
                addr,
                name: legacy_name,
            });
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
        for entry in &self.recent {
            // The name cannot contain '|' or a newline: hostnames never do,
            // and anything odd is written out and simply ignored on reload.
            text.push_str(&format!("recent={}|{}\n", entry.name, entry.addr));
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
        text.push_str(&format!("mic_enabled={}\n", self.mic_enabled));
        if let Some(device) = &self.mic_input_device {
            text.push_str(&format!("mic_input_device={device}\n"));
        }
        if let Some(device) = &self.mic_output_device {
            text.push_str(&format!("mic_output_device={device}\n"));
        }
        // Pairing secrets. This file is in the user's roaming profile, which
        // is as private as the rest of their settings; a token is worth no
        // more than permission to stream to one machine on one LAN.
        for pairing in &self.pairings {
            text.push_str(&format!(
                "pair={}|{}|{}\n",
                pairing.ip,
                pairing.name.replace('|', " "),
                pairing.token.to_hex()
            ));
        }

        let _ = fs::write(path, text);
    }

    /// Moves a receiver to the front of the list, trimming to the limit.
    ///
    /// Matching is by address, so reconnecting to a machine that has been
    /// renamed updates the name rather than adding a second entry.
    pub fn push_recent(&mut self, entry: Recent) {
        self.recent.retain(|existing| existing.addr != entry.addr);
        self.recent.insert(0, entry);
        self.recent.truncate(RECENT_LIMIT);
    }

    /// The receiver to offer on start-up.
    pub fn most_recent(&self) -> Option<&Recent> {
        self.recent.first()
    }

    /// The secret shared with one machine, if any.
    pub fn token_for(&self, ip: IpAddr) -> Option<Token> {
        self.pairings
            .iter()
            .find(|p| p.ip == ip)
            .map(|p| p.token.clone())
    }

    /// Every token this machine will accept, for the receiver side.
    pub fn all_tokens(&self) -> Vec<Token> {
        self.pairings.iter().map(|p| p.token.clone()).collect()
    }

    /// Records a pairing, replacing any earlier one with the same machine.
    pub fn add_pairing(&mut self, pairing: Pairing) {
        self.pairings.retain(|p| p.ip != pairing.ip);
        self.pairings.push(pairing);
        self.save();
    }

    /// Revokes every pairing. Both ends must pair again before anything flows.
    pub fn clear_pairings(&mut self) {
        self.pairings.clear();
        self.save();
    }

    /// Forgets every saved receiver, leaving device and preset choices alone.
    pub fn clear_recent(&mut self) {
        self.recent.clear();
        self.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(ip: &str, name: &str) -> Recent {
        Recent {
            addr: format!("{ip}:45778").parse().unwrap(),
            name: name.to_string(),
        }
    }

    #[test]
    fn most_recent_moves_to_front_without_duplicating() {
        let mut settings = Settings::default();
        settings.push_recent(entry("192.168.1.10", "A"));
        settings.push_recent(entry("192.168.1.11", "B"));
        settings.push_recent(entry("192.168.1.10", "A-renamed"));

        assert_eq!(settings.recent.len(), 2);
        assert_eq!(settings.most_recent().unwrap().name, "A-renamed");
        assert_eq!(settings.recent[1].name, "B");
    }

    #[test]
    fn keeps_at_most_five() {
        let mut settings = Settings::default();
        for i in 1..=8 {
            settings.push_recent(entry(&format!("192.168.1.{i}"), &format!("PC{i}")));
        }
        assert_eq!(settings.recent.len(), RECENT_LIMIT);
        assert_eq!(settings.most_recent().unwrap().name, "PC8");
        assert_eq!(settings.recent[4].name, "PC4");
    }
}
