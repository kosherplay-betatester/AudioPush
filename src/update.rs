//! "Is there a newer version?", asked only when the user presses the button.
//!
//! This is the one thing in AudioPush that leaves your network, so it is
//! deliberately the smallest request that answers the question: one GET to
//! the public releases endpoint, carrying nothing about you. No identifier,
//! no version ping, no telemetry, and nothing happens unless you ask.

use std::time::Duration;

/// Where the releases live. Public, so the request needs no credentials.
const LATEST: &str =
    "https://api.github.com/repos/kosherplay-betatester/AudioPush/releases/latest";

/// GitHub rejects requests without one. Deliberately says nothing about the
/// machine it came from.
const AGENT: &str = "AudioPush-update-check";

const TIMEOUT: Duration = Duration::from_secs(10);

pub struct Release {
    /// The tag with any leading "v" removed, e.g. "0.1.4".
    pub version: String,
    /// The page a human should open to download it.
    pub url: String,
}

/// The version this binary was built as.
pub fn current() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Asks GitHub for the newest release. Blocking: call it off the UI thread.
pub fn latest() -> Result<Release, String> {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .user_agent(AGENT)
        .build();
    let agent: ureq::Agent = config.into();

    let body = agent
        .get(LATEST)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("Could not reach GitHub: {e}"))?
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("Could not read the reply: {e}"))?;

    let tag = field(&body, "tag_name").ok_or("GitHub's reply had no release in it")?;
    let url = field(&body, "html_url")
        .unwrap_or_else(|| "https://github.com/kosherplay-betatester/AudioPush/releases".into());

    Ok(Release {
        version: tag.trim_start_matches('v').to_string(),
        url,
    })
}

/// Pulls one string field out of the JSON.
///
/// A whole JSON parser for two fields would be a dependency to keep current
/// for no gain. This reads exactly what it needs and gives up otherwise,
/// which is the right failure: the button says it could not tell.
fn field(body: &str, name: &str) -> Option<String> {
    let key = format!("\"{name}\":");
    let start = body.find(&key)? + key.len();
    let rest = body[start..].trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// True when `other` is a later version than `ours`.
///
/// Compares the numbers rather than the text, so 0.1.10 is correctly newer
/// than 0.1.9 -- which a string comparison gets backwards.
pub fn is_newer(other: &str, ours: &str) -> bool {
    let parts = |v: &str| -> Vec<u32> {
        v.split(['.', '-', '+'])
            .map(|p| p.parse::<u32>().unwrap_or(0))
            .collect()
    };
    let (a, b) = (parts(other), parts(ours));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number_not_by_text() {
        assert!(is_newer("0.1.10", "0.1.9"));
        assert!(is_newer("0.2.0", "0.1.99"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("0.1.3", "0.1.3"));
        assert!(!is_newer("0.1.2", "0.1.3"));
    }

    #[test]
    fn fields_are_read_out_of_the_reply() {
        let body = r#"{"url":"x","tag_name":"v0.1.4","html_url":"https://example/releases/v0.1.4"}"#;
        assert_eq!(field(body, "tag_name").as_deref(), Some("v0.1.4"));
        assert_eq!(
            field(body, "html_url").as_deref(),
            Some("https://example/releases/v0.1.4")
        );
        assert_eq!(field(body, "missing"), None);
    }
}
