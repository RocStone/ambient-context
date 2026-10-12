//! Extraction profiles: where an app's core content lives in its
//! accessibility tree, and what to leave out. One entry per app or family
//! of apps; adding one is adding a value to `PROFILES`. How to find the
//! attributes to match on is in the README, under "Adding an extraction
//! profile".

/// A test on one accessibility element. Every string is compared exactly.
/// The full vocabulary a profile can use; not every variant has a profile
/// using it yet.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub enum Matcher {
    /// AXRole, such as `AXTextArea` or `AXWebArea`.
    Role(&'static str),
    /// AXSubrole. Web landmarks arrive as subroles: `AXLandmarkMain`,
    /// `AXLandmarkNavigation`, `AXDocumentArticle`.
    Subrole(&'static str),
    /// AXIdentifier, set by the developer on a native element.
    Identifier(&'static str),
    /// AXDOMIdentifier, the HTML `id` behind web and Electron content.
    DomId(&'static str),
    /// An AXDOMIdentifier starting with this.
    DomIdPrefix(&'static str),
    /// One entry of AXDOMClassList, the HTML `class` list.
    DomClass(&'static str),
    /// Every one of these on the same element.
    All(&'static [Matcher]),
}

/// Where to find one piece of content: alternatives tried in order, the
/// first one with any match on screen wins. Every match of that
/// alternative is read, outermost first, in document order.
pub type Region = &'static [Matcher];

pub struct Profile {
    /// Shown in measurements and tests.
    pub name: &'static str,
    /// Matched exactly. The sturdier key: it survives localisation and
    /// renaming.
    pub bundle_ids: &'static [&'static str],
    /// Matched case-insensitively against the whole application name, for
    /// a snapshot that carries no bundle id.
    pub app_names: &'static [&'static str],
    /// The content regions, read in this order. When none of them is on
    /// screen the window goes to the generic fallback.
    pub content: &'static [Region],
    /// Subtrees left out even inside a content region, and in the
    /// fallback too.
    pub skip: &'static [Matcher],
}

pub const PROFILES: &[Profile] = &[
    // Paseo's web build sets stable DOM ids from React Native test ids.
    // The transcript is `agent-chat-scroll-web-dom-scroll`, or
    // `...-virtualized` once a chat is long, 49 levels down with its
    // messages at 54 to 62. The composer is the window's one text area; a
    // draft in a new tab has no transcript yet and is read through it alone.
    // Other views (a file preview, a plugin page, settings) have neither
    // and go to the fallback, whose sidebar rule leaves out the project
    // list: it is a full-height column 264 points wide in a 1728-point
    // window.
    Profile {
        name: "Paseo",
        bundle_ids: &["sh.paseo.desktop"],
        app_names: &["Paseo"],
        content: &[
            &[Matcher::DomIdPrefix("agent-chat-scroll-web-dom-")],
            &[Matcher::Role("AXTextArea")],
        ],
        skip: &[],
    },
    // Browsers: the page's main landmark when it marks one, the whole web
    // area otherwise. Tab strip, toolbar, omnibox and bookmarks bar sit
    // outside the web area and are never read. Navigation, header and
    // footer landmarks inside it are left out.
    Profile {
        name: "Browser",
        bundle_ids: &[
            "com.google.Chrome",
            "com.google.Chrome.beta",
            "com.google.Chrome.canary",
            "org.chromium.Chromium",
            "com.apple.Safari",
            "com.apple.SafariTechnologyPreview",
            "company.thebrowser.Browser",
            "company.thebrowser.dia",
            "com.brave.Browser",
            "com.microsoft.edgemac",
            "com.vivaldi.Vivaldi",
            "com.operasoftware.Opera",
            "org.mozilla.firefox",
            "app.zen-browser.zen",
        ],
        app_names: &[
            "Google Chrome",
            "Chromium",
            "Safari",
            "Arc",
            "Dia",
            "Brave Browser",
            "Microsoft Edge",
            "Vivaldi",
            "Opera",
            "Firefox",
            "Zen",
        ],
        content: &[&[
            Matcher::Subrole("AXLandmarkMain"),
            Matcher::Role("AXWebArea"),
        ]],
        skip: &[
            // The tab strip, in the fallback for a tab with no web area
            // (Chrome's PDF viewer exposes none). Never page content.
            Matcher::Role("AXTabGroup"),
            Matcher::Subrole("AXLandmarkNavigation"),
            Matcher::Subrole("AXLandmarkBanner"),
            Matcher::Subrole("AXLandmarkContentInfo"),
        ],
    },
];

/// The profile for an application, by bundle id first and name second.
pub fn find(app: &str, bundle_id: Option<&str>) -> Option<&'static Profile> {
    if let Some(bundle_id) = bundle_id {
        if let Some(profile) = PROFILES.iter().find(|p| p.bundle_ids.contains(&bundle_id)) {
            return Some(profile);
        }
    }
    PROFILES.iter().find(|p| {
        p.app_names
            .iter()
            .any(|name| name.eq_ignore_ascii_case(app.trim()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_profile_by_bundle_id_whatever_the_name() {
        let profile = find("Renamed Build", Some("sh.paseo.desktop")).unwrap();
        assert_eq!(profile.name, "Paseo");
    }

    #[test]
    fn finds_a_profile_by_name_case_insensitively() {
        assert_eq!(find("paseo", None).unwrap().name, "Paseo");
        assert_eq!(find("Google Chrome", None).unwrap().name, "Browser");
    }

    #[test]
    fn a_name_must_match_whole_not_as_a_substring() {
        assert!(find("Paseo Helper", None).is_none());
        assert!(find("Safari Technology Preview Notes", None).is_none());
    }

    #[test]
    fn an_unknown_bundle_id_still_falls_back_to_the_name() {
        assert_eq!(
            find("Safari", Some("com.example.unknown")).unwrap().name,
            "Browser"
        );
    }

    #[test]
    fn an_app_with_no_profile_gets_none() {
        assert!(find("TextEdit", Some("com.apple.TextEdit")).is_none());
    }

    #[test]
    fn every_profile_has_a_key_and_a_content_region() {
        for profile in PROFILES {
            assert!(
                !profile.bundle_ids.is_empty() || !profile.app_names.is_empty(),
                "{} matches no app",
                profile.name
            );
            assert!(
                !profile.content.is_empty(),
                "{} reads nothing",
                profile.name
            );
            for region in profile.content {
                assert!(!region.is_empty(), "{} has an empty region", profile.name);
            }
        }
    }

    #[test]
    fn no_bundle_id_is_claimed_by_two_profiles() {
        let mut seen = std::collections::HashSet::new();
        for profile in PROFILES {
            for id in profile.bundle_ids {
                assert!(seen.insert(*id), "{id} is in two profiles");
            }
        }
    }
}
