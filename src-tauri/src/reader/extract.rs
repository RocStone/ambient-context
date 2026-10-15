//! Turns a window's raw accessibility tree into the lines worth recording.
//!
//! The Swift walk hands over every node it reached, in document order, each
//! with its depth. What counts as content is decided here: by the app's
//! extraction profile when it has one, and by the generic fallback when it
//! has none or the profile finds nothing on screen. Pure, so both paths are
//! tested against synthetic trees rather than a live window.

use super::profiles::{self, Matcher, Profile};
use serde::Deserialize;

/// One accessibility element as the walk saw it. Only the attributes the
/// profiles and the fallback match on are carried.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct Node {
    #[serde(rename = "d")]
    pub depth: usize,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub subrole: Option<String>,
    /// AXIdentifier: the developer-set identifier of a native element.
    #[serde(default)]
    pub identifier: Option<String>,
    /// AXDOMIdentifier: the `id` of the HTML element behind web content.
    #[serde(default)]
    pub dom_id: Option<String>,
    /// AXDOMClassList: the `class` list of the HTML element.
    #[serde(default)]
    pub classes: Vec<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub value: Option<String>,
    /// AXPosition and AXSize as `[x, y, width, height]` in screen points.
    #[serde(default)]
    pub frame: Option<[f64; 4]>,
}

/// At most this many text elements per snapshot, as before profiles. Far
/// more than a screen holds; it only stops a pathological page.
pub const MAX_LINES: usize = 2000;
/// At most this many characters per snapshot. A full screen of chat is
/// around 15,000; the writer keeps 4,000 per block by default anyway.
pub const MAX_CHARS: usize = 100_000;
/// A single value this long is a document dump, not a line of text.
const MAX_LINE_CHARS: usize = 8000;

/// Chrome that the fallback leaves out when anything else is on screen:
/// menu bars and menus, toolbars, the tab buttons of a tab bar, web
/// navigation, header and footer landmarks, and sidebars (`is_sidebar`).
const CHROME_ROLES: &[&str] = &["AXMenuBar", "AXMenu", "AXToolbar"];
const CHROME_SUBROLES: &[&str] = &[
    "AXTabButton",
    "AXLandmarkNavigation",
    "AXLandmarkBanner",
    "AXLandmarkContentInfo",
];

/// Which path produced the lines, for the measurements and the tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Profile(&'static str),
    Fallback,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extracted {
    pub lines: Vec<String>,
    pub source: Source,
}

/// The window's text: through the app's profile when one matches the app
/// and finds its content, otherwise through the generic fallback.
pub fn extract(app: &str, bundle_id: Option<&str>, nodes: &[Node]) -> Extracted {
    let profile = profiles::find(app, bundle_id);
    if let Some(profile) = profile {
        if let Some(lines) = by_profile(profile, nodes) {
            return Extracted {
                lines,
                source: Source::Profile(profile.name),
            };
        }
    }
    let mask = profile.map_or_else(|| vec![false; nodes.len()], |p| skipped_mask(nodes, p));
    Extracted {
        lines: fallback(nodes, &mask),
        source: Source::Fallback,
    }
}

/// One past the last node of the subtree rooted at `index`.
fn subtree_end(nodes: &[Node], index: usize) -> usize {
    let depth = nodes[index].depth;
    nodes[index + 1..]
        .iter()
        .position(|n| n.depth <= depth)
        .map_or(nodes.len(), |offset| index + 1 + offset)
}

fn matches(node: &Node, matcher: &Matcher) -> bool {
    let eq = |field: &Option<String>, wanted: &str| field.as_deref() == Some(wanted);
    match matcher {
        Matcher::Role(role) => eq(&node.role, role),
        Matcher::Subrole(subrole) => eq(&node.subrole, subrole),
        Matcher::Identifier(id) => eq(&node.identifier, id),
        Matcher::DomId(id) => eq(&node.dom_id, id),
        Matcher::DomIdPrefix(prefix) => node
            .dom_id
            .as_deref()
            .is_some_and(|id| id.starts_with(prefix)),
        Matcher::DomClass(class) => node.classes.iter().any(|c| c == class),
        Matcher::All(all) => all.iter().all(|m| matches(node, m)),
    }
}

/// Marks every node inside a subtree whose root matches one of the
/// profile's `skip` matchers.
fn skipped_mask(nodes: &[Node], profile: &Profile) -> Vec<bool> {
    let mut mask = vec![false; nodes.len()];
    let mut i = 0;
    while i < nodes.len() {
        if profile.skip.iter().any(|m| matches(&nodes[i], m)) {
            let end = subtree_end(nodes, i);
            mask[i..end].fill(true);
            i = end;
        } else {
            i += 1;
        }
    }
    mask
}

/// Collects text from the nodes in `range` that are not left out, value
/// before title, as the walk always has. Stops at the budgets.
fn collect(
    nodes: &[Node],
    range: std::ops::Range<usize>,
    left_out: impl Fn(usize) -> bool,
    out: &mut Budgeted,
) {
    for i in range {
        if left_out(i) {
            continue;
        }
        for text in [&nodes[i].value, &nodes[i].title].into_iter().flatten() {
            if !out.push(text) {
                return;
            }
        }
    }
}

struct Budgeted {
    lines: Vec<String>,
    chars: usize,
}

impl Budgeted {
    fn new() -> Self {
        Budgeted {
            lines: Vec::new(),
            chars: 0,
        }
    }

    /// False once a budget is spent, so the caller stops reading.
    fn push(&mut self, text: &str) -> bool {
        if self.lines.len() >= MAX_LINES || self.chars >= MAX_CHARS {
            return false;
        }
        let trimmed = text.trim();
        let length = trimmed.chars().count();
        // A button's title repeats the text of the label inside it; the
        // second copy says nothing new.
        let repeat = self.lines.last().is_some_and(|last| last == trimmed);
        if !trimmed.is_empty() && length < MAX_LINE_CHARS && !repeat {
            self.lines.push(trimmed.to_string());
            self.chars += length;
        }
        true
    }
}

/// The text of the profile's content regions, in the profile's order. None
/// when they hold no text, which hands the window to the fallback: a view
/// the profile was not written for, or a fresh chat whose only region is
/// an empty composer.
fn by_profile(profile: &Profile, nodes: &[Node]) -> Option<Vec<String>> {
    let mask = skipped_mask(nodes, profile);
    // A node already read by an earlier region is not read again.
    let mut taken = vec![false; nodes.len()];
    let mut out = Budgeted::new();
    for region in profile.content {
        // The first alternative with any match on screen is the region.
        let roots = region.iter().find_map(|matcher| {
            let roots = region_roots(nodes, matcher, &mask, &taken);
            (!roots.is_empty()).then_some(roots)
        });
        for root in roots.unwrap_or_default() {
            let end = subtree_end(nodes, root);
            collect(nodes, root..end, |i| mask[i] || taken[i], &mut out);
            taken[root..end].fill(true);
        }
    }
    (!out.lines.is_empty()).then_some(out.lines)
}

/// The outermost nodes matching `matcher`, skipping anything masked or
/// already taken. A match inside another match is covered by it.
fn region_roots(nodes: &[Node], matcher: &Matcher, mask: &[bool], taken: &[bool]) -> Vec<usize> {
    let mut roots = Vec::new();
    let mut i = 0;
    while i < nodes.len() {
        if !mask[i] && !taken[i] && matches(&nodes[i], matcher) {
            roots.push(i);
            i = subtree_end(nodes, i);
        } else {
            i += 1;
        }
    }
    roots
}

/// Every node's text except chrome, for apps with no profile or whose
/// profile found nothing; `mask` is what that profile leaves out. If
/// leaving the chrome out leaves nothing, the chrome was the content (a
/// menu-only utility, a toolbar palette) and is read after all.
fn fallback(nodes: &[Node], mask: &[bool]) -> Vec<String> {
    let chrome = chrome_mask(nodes);
    let mut out = Budgeted::new();
    collect(nodes, 0..nodes.len(), |i| mask[i] || chrome[i], &mut out);
    if !out.lines.is_empty() {
        return out.lines;
    }
    let mut out = Budgeted::new();
    collect(nodes, 0..nodes.len(), |i| mask[i], &mut out);
    out.lines
}

fn is_chrome(node: &Node) -> bool {
    node.role
        .as_deref()
        .is_some_and(|role| CHROME_ROLES.contains(&role))
        || node
            .subrole
            .as_deref()
            .is_some_and(|subrole| CHROME_SUBROLES.contains(&subrole))
}

/// Chrome subtrees, plus sidebars.
fn chrome_mask(nodes: &[Node]) -> Vec<bool> {
    let parents = parents(nodes);
    let mut mask = vec![false; nodes.len()];
    for i in 0..nodes.len() {
        if mask[i] {
            continue;
        }
        let chrome = is_chrome(&nodes[i]) || parents[i].is_some_and(|p| is_sidebar(nodes, i, p));
        if chrome {
            let end = subtree_end(nodes, i);
            mask[i..end].fill(true);
        }
    }
    mask
}

/// Each node's parent, in one pass: the nearest earlier node that is
/// shallower, kept on a stack of the current ancestors.
fn parents(nodes: &[Node]) -> Vec<Option<usize>> {
    let mut ancestors: Vec<usize> = Vec::new();
    nodes
        .iter()
        .enumerate()
        .map(|(i, node)| {
            while ancestors
                .last()
                .is_some_and(|&a| nodes[a].depth >= node.depth)
            {
                ancestors.pop();
            }
            let parent = ancestors.last().copied();
            ancestors.push(i);
            parent
        })
        .collect()
}

fn children(nodes: &[Node], index: usize) -> Vec<usize> {
    let depth = nodes[index].depth;
    let end = subtree_end(nodes, index);
    (index + 1..end)
        .filter(|&c| nodes[c].depth == depth + 1)
        .collect()
}

/// A sidebar is told apart by its shape rather than its markup, which
/// differs in every app and toolkit: a column as tall as its container,
/// at most a third of its width, against the left or right edge, beside a
/// sibling at least twice as wide. Finder's and Mail's source lists,
/// Paseo's project list and a web app's navigation column all look like
/// this; content panes do not. A container under 500 points wide is a
/// narrow window, where the shape says nothing. Nor does a container with
/// no height, which an app can report for an element: every
/// column would pass as full height.
fn is_sidebar(nodes: &[Node], index: usize, parent: usize) -> bool {
    let (Some(own), Some(outer)) = (nodes[index].frame, nodes[parent].frame) else {
        return false;
    };
    let [x, _, width, height] = own;
    let [outer_x, _, outer_width, outer_height] = outer;
    let edge = 2.0;
    let shaped = outer_width >= 500.0
        && outer_height > 0.0
        && width > 0.0
        && width <= outer_width / 3.0
        && height >= outer_height * 0.9
        && ((x - outer_x).abs() <= edge || (x + width - (outer_x + outer_width)).abs() <= edge);
    shaped
        && children(nodes, parent).into_iter().any(|sibling| {
            sibling != index
                && nodes[sibling]
                    .frame
                    .is_some_and(|[_, _, w, h]| w >= width * 2.0 && h >= outer_height * 0.5)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a node list from `(depth, role, extra)` rows, where `extra`
    /// is a small spec: `id=`, `dom=`, `class=`, `sub=`, `title=`, `value=`,
    /// `frame=x,y,w,h`, separated by `;`. Keeps the synthetic trees readable.
    fn tree(rows: &[(usize, &str, &str)]) -> Vec<Node> {
        rows.iter()
            .map(|(depth, role, extra)| {
                let mut node = Node {
                    depth: *depth,
                    role: Some(role.to_string()),
                    ..Node::default()
                };
                for part in extra.split(';').filter(|p| !p.is_empty()) {
                    let (key, value) = part.split_once('=').unwrap();
                    let text = Some(value.to_string());
                    match key {
                        "id" => node.identifier = text,
                        "dom" => node.dom_id = text,
                        "class" => node.classes.push(value.to_string()),
                        "sub" => node.subrole = text,
                        "title" => node.title = text,
                        "value" => node.value = text,
                        "frame" => {
                            let f: Vec<f64> =
                                value.split(',').map(|n| n.parse().unwrap()).collect();
                            node.frame = Some([f[0], f[1], f[2], f[3]]);
                        }
                        other => panic!("unknown key {other}"),
                    }
                }
                node
            })
            .collect()
    }

    /// A chat app laid out the way Paseo's web build is: a full-height
    /// sidebar of projects with no identifier, and a main pane holding a
    /// header, a transcript container with a DOM id and a composer text
    /// area, all nested far past the old 40-level cap.
    fn paseo_like(transcript: &[&str], composer: &str) -> Vec<Node> {
        let mut nodes = tree(&[
            (0, "AXWindow", "title=Fixture chat;frame=0,0,1700,1000"),
            (1, "AXWebArea", "frame=0,0,1700,1000"),
            (2, "AXGroup", "dom=root;frame=0,0,1700,1000"),
            (3, "AXGroup", "frame=0,0,260,1000"),
            (4, "AXGroup", "sub=AXApplicationGroup;title=alpha-project"),
            (5, "AXStaticText", "value=alpha-project"),
            (4, "AXGroup", "sub=AXApplicationGroup;title=beta-project"),
            (5, "AXStaticText", "value=beta-project"),
            (4, "AXButton", "dom=sidebar-settings"),
            (3, "AXGroup", "frame=260,0,1440,1000"),
        ]);
        nodes.extend((4..49).map(|depth| tree(&[(depth, "AXGroup", "")]).remove(0)));
        nodes.extend(tree(&[
            (49, "AXStaticText", "value=Fixture chat header"),
            (49, "AXGroup", "dom=agent-chat-scroll-web-dom-scroll"),
        ]));
        for line in transcript {
            let value = format!("value={line}");
            nodes.extend(tree(&[(55, "AXGroup", ""), (59, "AXStaticText", &value)]));
        }
        let value = format!("value={composer}");
        nodes.extend(tree(&[(49, "AXGroup", ""), (52, "AXTextArea", &value)]));
        nodes
    }

    #[test]
    fn parents_are_the_nearest_shallower_node_before() {
        let nodes = tree(&[
            (0, "AXWindow", ""),
            (1, "AXGroup", ""),
            (3, "AXStaticText", ""),
            (1, "AXGroup", ""),
            (2, "AXStaticText", ""),
        ]);
        assert_eq!(
            parents(&nodes),
            vec![None, Some(0), Some(1), Some(0), Some(3)]
        );
    }

    #[test]
    fn subtree_end_stops_at_the_next_sibling_or_shallower_node() {
        let nodes = tree(&[
            (0, "AXWindow", ""),
            (1, "AXGroup", ""),
            (2, "AXStaticText", ""),
            (1, "AXGroup", ""),
        ]);
        assert_eq!(subtree_end(&nodes, 1), 3);
        assert_eq!(subtree_end(&nodes, 0), 4);
        assert_eq!(subtree_end(&nodes, 3), 4);
    }

    #[test]
    fn paseo_reads_the_transcript_and_composer_but_not_the_sidebar() {
        let nodes = paseo_like(&["first message", "second message"], "a draft");
        let got = extract("Paseo", Some("sh.paseo.desktop"), &nodes);
        assert_eq!(got.source, Source::Profile("Paseo"));
        assert_eq!(
            got.lines,
            vec!["first message", "second message", "a draft"]
        );
    }

    #[test]
    fn paseo_reads_a_long_chat_through_its_virtualized_transcript() {
        let nodes = tree(&[
            (0, "AXWindow", ""),
            (3, "AXStaticText", "value=sidebar-project"),
            (49, "AXGroup", "dom=agent-chat-scroll-web-dom-virtualized"),
            (57, "AXStaticText", "value=an older message"),
        ]);
        let got = extract("Paseo", Some("sh.paseo.desktop"), &nodes);
        assert_eq!(got.lines, vec!["an older message"]);
    }

    #[test]
    fn paseo_matches_by_name_when_the_bundle_id_is_missing() {
        let nodes = paseo_like(&["only message"], "");
        let got = extract("Paseo", None, &nodes);
        assert_eq!(got.source, Source::Profile("Paseo"));
        assert_eq!(got.lines, vec!["only message"]);
    }

    #[test]
    fn an_empty_chat_falls_back_and_still_leaves_the_sidebar_out() {
        let nodes = paseo_like(&[], "");
        let got = extract("Paseo", Some("sh.paseo.desktop"), &nodes);
        assert_eq!(got.source, Source::Fallback);
        assert_eq!(got.lines, vec!["Fixture chat", "Fixture chat header"]);
    }

    #[test]
    fn a_profile_whose_content_is_absent_falls_back() {
        // A settings screen: no transcript, no composer.
        let nodes = tree(&[
            (0, "AXWindow", "title=Settings"),
            (7, "AXWebArea", ""),
            (9, "AXStaticText", "value=Theme and fonts for the app"),
        ]);
        let got = extract("Paseo", Some("sh.paseo.desktop"), &nodes);
        assert_eq!(got.source, Source::Fallback);
        assert_eq!(got.lines, vec!["Settings", "Theme and fonts for the app"]);
    }

    #[test]
    fn paseo_leaves_the_sidebar_out_of_a_view_with_no_transcript() {
        // A plugin page: no transcript, no composer, so the fallback reads
        // the window, minus the project list by its shape.
        let nodes = tree(&[
            (0, "AXWindow", "title=Plugin page;frame=0,30,1700,1000"),
            (7, "AXWebArea", "frame=0,30,1700,1000"),
            (15, "AXGroup", "frame=0,30,1700,1000"),
            (16, "AXGroup", "frame=0,30,260,1000"),
            (22, "AXStaticText", "value=alpha-project;frame=10,80,120,20"),
            (
                20,
                "AXButton",
                "dom=sidebar-settings;title=Settings;frame=10,990,20,20",
            ),
            (16, "AXGroup", "frame=260,30,1440,1000"),
            (
                30,
                "AXStaticText",
                "value=Plugin description;frame=300,100,400,20",
            ),
        ]);
        let got = extract("Paseo", Some("sh.paseo.desktop"), &nodes);
        assert_eq!(got.source, Source::Fallback);
        assert_eq!(got.lines, vec!["Plugin page", "Plugin description"]);
    }

    #[test]
    fn regions_read_in_profile_order_not_document_order() {
        // The composer comes first in the document here; the profile still
        // reads the transcript first.
        let nodes = tree(&[
            (0, "AXWindow", ""),
            (2, "AXTextArea", "value=draft text"),
            (2, "AXGroup", "dom=agent-chat-scroll-web-dom-scroll"),
            (3, "AXStaticText", "value=a message"),
        ]);
        let got = extract("Paseo", Some("sh.paseo.desktop"), &nodes);
        assert_eq!(got.lines, vec!["a message", "draft text"]);
    }

    #[test]
    fn a_node_is_never_read_twice_by_two_regions() {
        // A text area inside the transcript belongs to the transcript.
        let nodes = tree(&[
            (0, "AXWindow", ""),
            (1, "AXGroup", "dom=agent-chat-scroll-web-dom-scroll"),
            (2, "AXTextArea", "value=inline editor"),
        ]);
        let got = extract("Paseo", Some("sh.paseo.desktop"), &nodes);
        assert_eq!(got.lines, vec!["inline editor"]);
    }

    #[test]
    fn browser_reads_the_main_landmark_over_the_rest_of_the_page() {
        let nodes = tree(&[
            (0, "AXWindow", "title=Example page"),
            (1, "AXToolbar", ""),
            (2, "AXTextField", "value=example.com/page"),
            (1, "AXWebArea", "title=Example page"),
            (2, "AXGroup", "sub=AXLandmarkNavigation"),
            (3, "AXLink", "title=Home"),
            (2, "AXGroup", "sub=AXLandmarkMain"),
            (3, "AXStaticText", "value=The article body"),
        ]);
        let got = extract("Google Chrome", Some("com.google.Chrome"), &nodes);
        assert_eq!(got.source, Source::Profile("Browser"));
        assert_eq!(got.lines, vec!["The article body"]);
    }

    #[test]
    fn browser_without_a_main_landmark_reads_the_web_area_not_the_toolbar() {
        let nodes = tree(&[
            (0, "AXWindow", "title=Example page"),
            (1, "AXToolbar", ""),
            (2, "AXTextField", "value=example.com/page"),
            (1, "AXWebArea", ""),
            (2, "AXStaticText", "value=Page text without landmarks"),
        ]);
        let got = extract("Safari", Some("com.apple.Safari"), &nodes);
        assert_eq!(got.lines, vec!["Page text without landmarks"]);
    }

    #[test]
    fn browser_tab_with_no_web_area_reads_neither_tabs_nor_toolbar() {
        // A PDF tab in Chrome: the viewer exposes no web area.
        let nodes = tree(&[
            (0, "AXWindow", "title=paper.pdf"),
            (1, "AXTabGroup", ""),
            (2, "AXButton", "title=New tab"),
            (2, "AXRadioButton", "sub=AXTabButton;title=Another tab"),
            (1, "AXToolbar", ""),
            (2, "AXPopUpButton", "title=Some extension"),
            (1, "AXGroup", ""),
        ]);
        let got = extract("Google Chrome", Some("com.google.Chrome"), &nodes);
        assert_eq!(got.source, Source::Fallback);
        assert_eq!(got.lines, vec!["paper.pdf"]);
    }

    #[test]
    fn fallback_leaves_out_menus_toolbars_and_tab_buttons() {
        let nodes = tree(&[
            (0, "AXWindow", "title=Some App"),
            (1, "AXMenuBar", ""),
            (2, "AXMenuBarItem", "title=File"),
            (1, "AXToolbar", ""),
            (2, "AXButton", "title=Share"),
            (1, "AXTabGroup", ""),
            (2, "AXRadioButton", "sub=AXTabButton;title=Tab one"),
            (2, "AXStaticText", "value=Content of the open tab"),
        ]);
        let got = extract("Some App", Some("com.example.someapp"), &nodes);
        assert_eq!(got.source, Source::Fallback);
        assert_eq!(got.lines, vec!["Some App", "Content of the open tab"]);
    }

    #[test]
    fn fallback_reads_the_chrome_when_it_is_the_only_content() {
        let nodes = tree(&[
            (0, "AXWindow", ""),
            (1, "AXToolbar", ""),
            (2, "AXButton", "title=Only a palette"),
        ]);
        let got = extract("Palette", None, &nodes);
        assert_eq!(got.lines, vec!["Only a palette"]);
    }

    #[test]
    fn fallback_drops_a_sidebar_on_either_edge_but_keeps_the_content_pane() {
        let nodes = tree(&[
            (0, "AXWindow", "frame=0,0,1200,800"),
            (1, "AXSplitGroup", "frame=0,0,1200,800"),
            (2, "AXScrollArea", "frame=0,0,220,800"),
            (3, "AXOutline", ""),
            (4, "AXStaticText", "value=Favourites"),
            (2, "AXScrollArea", "frame=220,0,760,800"),
            (3, "AXOutline", ""),
            (4, "AXStaticText", "value=report-draft.txt"),
            (2, "AXGroup", "frame=980,0,220,800"),
            (3, "AXStaticText", "value=Inspector panel"),
        ]);
        let got = extract("Files", None, &nodes);
        assert_eq!(got.lines, vec!["report-draft.txt"]);
    }

    #[test]
    fn a_column_that_is_not_full_height_or_not_narrow_is_not_a_sidebar() {
        let nodes = tree(&[
            (0, "AXWindow", "frame=0,0,1200,800"),
            (1, "AXGroup", "frame=0,0,220,300"),
            (2, "AXStaticText", "value=Short box"),
            (1, "AXGroup", "frame=0,300,500,500"),
            (2, "AXStaticText", "value=Wide half"),
            (1, "AXGroup", "frame=500,300,700,500"),
            (2, "AXStaticText", "value=Other half"),
        ]);
        let got = extract("Layout", None, &nodes);
        assert_eq!(got.lines, vec!["Short box", "Wide half", "Other half"]);
    }

    #[test]
    fn a_narrow_window_has_no_sidebar() {
        let nodes = tree(&[
            (0, "AXWindow", "frame=0,0,400,800"),
            (1, "AXGroup", "frame=0,0,100,800"),
            (2, "AXStaticText", "value=Left column"),
            (1, "AXGroup", "frame=100,0,300,800"),
            (2, "AXStaticText", "value=Right column"),
        ]);
        let got = extract("Narrow", None, &nodes);
        assert_eq!(got.lines, vec!["Left column", "Right column"]);
    }

    #[test]
    fn a_container_with_no_height_has_no_sidebar() {
        let nodes = tree(&[
            (0, "AXWindow", "frame=0,0,1000,0"),
            (1, "AXGroup", "frame=0,0,200,0"),
            (2, "AXStaticText", "value=Left column"),
            (1, "AXGroup", "frame=200,0,800,0"),
            (2, "AXStaticText", "value=Right column"),
        ]);
        let got = extract("Unlaid", None, &nodes);
        assert_eq!(got.lines, vec!["Left column", "Right column"]);
    }

    #[test]
    fn zero_size_frames_are_read_without_trouble() {
        let nodes = tree(&[
            (0, "AXWindow", "frame=0,0,0,0"),
            (1, "AXGroup", "frame=0,0,0,0"),
            (2, "AXStaticText", "value=Inside nothing"),
            (1, "AXGroup", "frame=0,0,0,0"),
            (2, "AXStaticText", "value=Also inside nothing"),
        ]);
        let got = extract("Zero", None, &nodes);
        assert_eq!(got.lines, vec!["Inside nothing", "Also inside nothing"]);
    }

    #[test]
    fn a_window_that_is_only_a_sidebar_is_read_after_all() {
        let nodes = tree(&[
            (0, "AXWindow", "frame=0,0,1000,800"),
            (1, "AXGroup", "frame=0,0,200,800"),
            (2, "AXStaticText", "value=Navigation entry"),
            (1, "AXGroup", "frame=200,0,800,800"),
        ]);
        let got = extract("Empty", None, &nodes);
        assert_eq!(got.lines, vec!["Navigation entry"]);
    }

    #[test]
    fn fallback_reads_deep_content_with_no_depth_limit() {
        let mut rows = vec![(0, "AXWindow", "")];
        for depth in 1..120 {
            rows.push((depth, "AXGroup", ""));
        }
        rows.push((120, "AXStaticText", "value=Deep text"));
        assert_eq!(extract("Deep", None, &tree(&rows)).lines, vec!["Deep text"]);
    }

    #[test]
    fn text_is_trimmed_and_empty_or_oversized_values_are_dropped() {
        let long = "x".repeat(MAX_LINE_CHARS);
        let nodes = vec![
            Node {
                depth: 0,
                role: Some("AXWindow".into()),
                ..Node::default()
            },
            Node {
                depth: 1,
                value: Some("  padded  ".into()),
                title: Some("   ".into()),
                ..Node::default()
            },
            Node {
                depth: 1,
                value: Some(long),
                ..Node::default()
            },
        ];
        assert_eq!(extract("App", None, &nodes).lines, vec!["padded"]);
    }

    #[test]
    fn a_line_repeating_the_one_before_is_dropped() {
        let nodes = tree(&[
            (0, "AXWindow", ""),
            (1, "AXButton", "title=Ran 3 commands"),
            (2, "AXStaticText", "value=Ran 3 commands"),
            (1, "AXStaticText", "value=Next line"),
            (1, "AXStaticText", "value=Ran 3 commands"),
        ]);
        assert_eq!(
            extract("App", None, &nodes).lines,
            vec!["Ran 3 commands", "Next line", "Ran 3 commands"]
        );
    }

    #[test]
    fn the_line_budget_caps_a_huge_window() {
        let values: Vec<String> = (0..MAX_LINES + 50)
            .map(|i| format!("value=row {i}"))
            .collect();
        let mut rows: Vec<(usize, &str, &str)> = vec![(0, "AXWindow", "")];
        for value in &values {
            rows.push((1, "AXStaticText", value));
        }
        assert_eq!(extract("Big", None, &tree(&rows)).lines.len(), MAX_LINES);
    }

    #[test]
    fn the_character_budget_caps_a_huge_window() {
        let nodes: Vec<Node> = std::iter::once(Node::default())
            .chain((0..40).map(|i| Node {
                depth: 1,
                value: Some(format!("{i:02}{}", "y".repeat(4998))),
                ..Node::default()
            }))
            .collect();
        let got = extract("Big", None, &nodes).lines;
        let chars: usize = got.iter().map(|l| l.chars().count()).sum();
        assert!((MAX_CHARS..MAX_CHARS + 5000).contains(&chars), "{chars}");
    }
}
