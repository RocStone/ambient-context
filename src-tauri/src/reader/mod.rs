pub mod extract;
pub mod macos;
pub mod profiles;
pub mod windows;

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Snapshot {
    pub app: String,
    pub window_title: Option<String>,
    /// Path of the window's backing file, where the app exposes one.
    pub document: Option<String>,
    /// Page URL of the window's main web area, for browsers.
    pub url: Option<String>,
    pub text: Vec<String>,
    /// Set by redaction when a rule says record the heading and drop the
    /// body. Carried this far because only the writer can act on it.
    pub headings_only: bool,
}

pub trait WindowReader {
    fn snapshot(&self) -> Option<Snapshot>;

    /// Seconds since the last keyboard or mouse input. None where the
    /// platform cannot report it, which the idle check reads as "not idle"
    /// rather than guessing.
    fn seconds_since_input(&self) -> Option<f64>;
}

pub struct PlatformReader;

impl WindowReader for PlatformReader {
    fn snapshot(&self) -> Option<Snapshot> {
        #[cfg(target_os = "macos")]
        {
            macos::snapshot()
        }
        #[cfg(target_os = "windows")]
        {
            windows::snapshot()
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            None
        }
    }

    fn seconds_since_input(&self) -> Option<f64> {
        #[cfg(target_os = "macos")]
        {
            macos::seconds_since_input()
        }
        #[cfg(target_os = "windows")]
        {
            windows::seconds_since_input()
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    NotGranted,
    Granted,
}

/// Swift returns a bare `Int32` across the C boundary because `swift!`
/// declarations have no Result-typed return. This turns the code into a
/// type. Pure, so it is unit tested directly rather than through Swift.
pub fn permission_from_code(code: i32) -> Permission {
    match code {
        1 => Permission::Granted,
        _ => Permission::NotGranted,
    }
}

/// The window as the Swift walk saw it: the raw tree, before anything
/// decides what is content.
#[derive(Deserialize)]
pub struct RawSnapshot {
    pub app: String,
    #[serde(default)]
    pub bundle_id: Option<String>,
    pub window_title: Option<String>,
    #[serde(default)]
    pub document: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    pub nodes: Vec<extract::Node>,
}

/// Swift signals failure by returning a string prefixed `"ERROR: "` rather
/// than throwing across the C boundary. Pure, so it is unit tested directly.
pub fn parse_raw_snapshot(raw: &str) -> Result<RawSnapshot, String> {
    if let Some(message) = raw.strip_prefix("ERROR: ") {
        return Err(message.to_string());
    }
    serde_json::from_str(raw).map_err(|e| format!("malformed snapshot: {e}"))
}

/// The snapshot with its text extracted through the app's profile or the
/// generic fallback.
pub fn parse_snapshot_json(raw: &str) -> Result<Snapshot, String> {
    let parsed = parse_raw_snapshot(raw)?;
    let text = extract::extract(&parsed.app, parsed.bundle_id.as_deref(), &parsed.nodes).lines;
    Ok(Snapshot {
        app: parsed.app,
        window_title: parsed.window_title,
        document: parsed.document,
        url: parsed.url,
        text,
        // Snapshots arrive from the accessibility walk with no rule applied.
        headings_only: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_one_to_granted() {
        assert_eq!(permission_from_code(1), Permission::Granted);
    }

    #[test]
    fn maps_zero_to_not_granted() {
        assert_eq!(permission_from_code(0), Permission::NotGranted);
    }

    #[test]
    fn maps_unexpected_codes_to_not_granted() {
        assert_eq!(permission_from_code(99), Permission::NotGranted);
        assert_eq!(permission_from_code(-1), Permission::NotGranted);
    }

    #[test]
    fn parses_a_full_snapshot() {
        let raw = r#"{"app":"Linear","bundle_id":"com.example.linear","window_title":"YN-102",
            "url":null,"visited":3,"truncated":false,"nodes":[
            {"d":0,"role":"AXWindow"},
            {"d":1,"role":"AXStaticText","value":"a"},
            {"d":1,"role":"AXButton","title":"b","classes":["x"]}]}"#;
        let snap = parse_snapshot_json(raw).unwrap();
        assert_eq!(snap.app, "Linear");
        assert_eq!(snap.window_title.as_deref(), Some("YN-102"));
        assert_eq!(snap.text, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn extracts_through_the_apps_profile() {
        let raw = r#"{"app":"Paseo","bundle_id":"sh.paseo.desktop","window_title":"Chat",
            "nodes":[
            {"d":0,"role":"AXWindow","title":"Chat"},
            {"d":1,"role":"AXStaticText","value":"sidebar project"},
            {"d":1,"role":"AXGroup","dom_id":"agent-chat-scroll-web-dom-scroll"},
            {"d":2,"role":"AXStaticText","value":"message text"}]}"#;
        assert_eq!(parse_snapshot_json(raw).unwrap().text, vec!["message text"]);
    }

    #[test]
    fn parses_a_snapshot_with_no_window_title() {
        let raw = r#"{"app":"Finder","window_title":null,"nodes":[]}"#;
        let snap = parse_snapshot_json(raw).unwrap();
        assert_eq!(snap.window_title, None);
        assert!(snap.text.is_empty());
    }

    #[test]
    fn turns_the_error_prefix_into_err() {
        let err = parse_snapshot_json("ERROR: permission not granted").unwrap_err();
        assert_eq!(err, "permission not granted");
    }

    /// Not a check: prints what extraction keeps from a real snapshot made
    /// with `scripts/ax-snapshot.sh`. `AX_SHOW=1` prints the lines too.
    #[test]
    #[ignore]
    fn measure_snapshot() {
        let path = std::env::var("AX_SNAPSHOT").expect("AX_SNAPSHOT=<file from ax-snapshot.sh>");
        let json = std::fs::read_to_string(path).unwrap();
        let raw = parse_raw_snapshot(&json).unwrap();
        // The walk's own counters, which only this measurement reads.
        let counters: serde_json::Value = serde_json::from_str(&json).unwrap();
        let got = extract::extract(&raw.app, raw.bundle_id.as_deref(), &raw.nodes);
        let chars: usize = got.lines.iter().map(|l| l.chars().count()).sum();
        println!(
            "app {} | visited {} (stopped by a budget: {}) | via {:?} | kept {} lines, {} chars",
            raw.app,
            counters["visited"],
            counters["truncated"],
            got.source,
            got.lines.len(),
            chars
        );
        if std::env::var("AX_SHOW").is_ok() {
            for line in &got.lines {
                println!("  {line}");
            }
        }
    }

    #[test]
    fn rejects_malformed_json() {
        assert!(parse_snapshot_json("{not json").is_err());
    }
}
