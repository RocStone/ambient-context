import Foundation
import AppKit
import ApplicationServices
import CoreGraphics
import SwiftRs

// Status codes crossing the C boundary: 0 = not granted, 1 = granted.
// A codes-based contract is simpler to keep in sync by hand than a
// marshalled enum.

/// Reads the current Accessibility trust state. Does not prompt.
@_cdecl("ambient_ax_permission_status")
public func ambientAxPermissionStatus() -> Int32 {
    return AXIsProcessTrusted() ? 1 : 0
}

/// Raises the system prompt if the app is not yet trusted. The prompt only
/// offers to open System Settings; the grant itself happens there, so this
/// almost always returns 0 on first call and the caller must poll
/// `ambient_ax_permission_status` afterwards.
@_cdecl("ambient_ax_request_permission")
public func ambientAxRequestPermission() -> Int32 {
    let key = kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String
    let options = [key: true] as CFDictionary
    return AXIsProcessTrustedWithOptions(options) ? 1 : 0
}

/// Seconds since the last keyboard or mouse event anywhere in the session,
/// so a machine left alone stops extending the open block. Returns -1 when
/// the "any input" event type cannot be formed, which the caller reads as
/// "no idle reading on this platform" rather than as zero seconds idle.
@_cdecl("ambient_ax_seconds_since_input")
public func ambientAxSecondsSinceInput() -> Double {
    guard let anyInput = CGEventType(rawValue: ~0) else { return -1 }
    return CGEventSource.secondsSinceLastEventType(.combinedSessionState, eventType: anyInput)
}

// The walk reads the window's tree as it is and leaves the choice of what
// is content to the Rust side, where each app's extraction profile and the
// generic fallback live and are tested. Nothing here bounds by depth:
// Paseo's chat transcript sits 54 to 62 levels down, past the 40 that once
// cut it off. Work is bounded by nodes visited, by text read and by wall
// time instead. The text caps are three times what the Rust side keeps,
// room for the chrome it leaves out. The depth guard only protects the
// stack against a cyclic tree.
private let maxVisited = 20000
private let maxTextNodes = 6000
private let maxTextChars = 300_000
private let walkSeconds: TimeInterval = 1.5
private let stackGuardDepth = 256
// A value this long is a document or a terminal's scrollback, not a label.
// Only the part on screen is kept, read through the visible range.
private let maxValueChars = 8000

// Read in one round trip per node. The text attributes are a second trip,
// made only once the node is known not to be a secure field, so a password
// is never read at all.
private let structureAttributes = [
    kAXRoleAttribute as String,
    kAXSubroleAttribute as String,
    kAXIdentifierAttribute as String,
    "AXDOMIdentifier",
    "AXDOMClassList",
    kAXChildrenAttribute as String,
    "AXURL",
    kAXPositionAttribute as String,
    kAXSizeAttribute as String,
] as CFArray
private let textAttributes = [kAXValueAttribute as String, kAXTitleAttribute as String] as CFArray

private func copyAttribute(_ element: AXUIElement, _ attribute: String) -> CFTypeRef? {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, attribute as CFString, &value) == .success else {
        return nil
    }
    return value
}

private func copyAttributes(_ element: AXUIElement, _ attributes: CFArray) -> [Any]? {
    var values: CFArray?
    guard AXUIElementCopyMultipleAttributeValues(
        element, attributes, AXCopyMultipleAttributeOptions(rawValue: 0), &values) == .success
    else {
        return nil
    }
    return values as? [Any]
}

/// `[x, y, width, height]` from AXPosition and AXSize, which is how the
/// Rust side tells a sidebar from a content pane.
private func frame(_ position: Any, _ size: Any) -> [Double]? {
    let position = position as CFTypeRef
    let size = size as CFTypeRef
    guard CFGetTypeID(position) == AXValueGetTypeID(),
          CFGetTypeID(size) == AXValueGetTypeID() else {
        return nil
    }
    var point = CGPoint.zero
    var extent = CGSize.zero
    guard AXValueGetValue(position as! AXValue, .cgPoint, &point),
          AXValueGetValue(size as! AXValue, .cgSize, &extent) else {
        return nil
    }
    return finiteFrame(point, extent)
}

/// The frame as plain numbers, or nil when any of them is NaN or infinite.
/// An app can report such a position or size for an element, and
/// JSONSerialization raises an Objective-C exception on a non-finite number,
/// which `try?` cannot catch: it aborted the whole app. A node without a
/// frame is only ever left out of the sidebar test.
func finiteFrame(_ point: CGPoint, _ extent: CGSize) -> [Double]? {
    let values = [point.x, point.y, extent.width, extent.height].map { Double($0) }
    return values.allSatisfy(\.isFinite) ? values : nil
}

/// The on-screen part of a long text value, or nil when the element does
/// not report one.
private func visibleText(_ element: AXUIElement) -> String? {
    guard let range = copyAttribute(element, kAXVisibleCharacterRangeAttribute as String) else {
        return nil
    }
    var value: CFTypeRef?
    guard AXUIElementCopyParameterizedAttributeValue(
        element, kAXStringForRangeParameterizedAttribute as CFString, range, &value) == .success
    else {
        return nil
    }
    return value as? String
}

private struct Walk {
    var nodes: [[String: Any]] = []
    var visited = 0
    var textNodes = 0
    var textChars = 0
    var webUrl: String? = nil
    let deadline = Date().addingTimeInterval(walkSeconds)
    var stopped = false
}

/// Appends the subtree at `element` to `walk.nodes` in document order. Each
/// node carries its depth, which is all the Rust side needs to rebuild the
/// tree from the flat list. A nested JSON tree would hit serde_json's
/// recursion limit at the depths Electron apps reach.
private func walkTree(_ element: AXUIElement, depth: Int, into walk: inout Walk) {
    if walk.stopped { return }
    walk.visited += 1
    if walk.visited > maxVisited || walk.textNodes >= maxTextNodes
        || walk.textChars >= maxTextChars || Date() > walk.deadline {
        walk.stopped = true
        return
    }
    if depth > stackGuardDepth { return }
    guard let structure = copyAttributes(element, structureAttributes) else { return }

    let role = structure[0] as? String
    let subrole = structure[1] as? String

    // A secure field and everything inside it is skipped entirely. This is
    // the first of the redaction layers and the only one that can be done
    // before the text is ever read.
    if role == "AXSecureTextField" || subrole == (kAXSecureTextFieldSubrole as String) {
        return
    }

    var node: [String: Any] = ["d": depth]
    if let role { node["role"] = role }
    if let subrole { node["subrole"] = subrole }
    if let identifier = structure[2] as? String, !identifier.isEmpty {
        node["identifier"] = identifier
    }
    if let domId = structure[3] as? String, !domId.isEmpty { node["dom_id"] = domId }
    if let classes = structure[4] as? [String], !classes.isEmpty { node["classes"] = classes }
    if let frame = frame(structure[7], structure[8]) { node["frame"] = frame }

    // The page URL lives on the web area, not the window. First one wins:
    // the focused window's main web area is reached before any embedded one.
    if walk.webUrl == nil, role == "AXWebArea", let url = structure[6] as? NSURL {
        walk.webUrl = url.absoluteString
    }

    if let text = copyAttributes(element, textAttributes) {
        if var value = text[0] as? String {
            if value.count >= maxValueChars { value = visibleText(element) ?? "" }
            if value.count < maxValueChars, !value.isEmpty {
                node["value"] = value
                walk.textChars += value.count
            }
        }
        if let title = text[1] as? String, !title.isEmpty, title.count < maxValueChars {
            node["title"] = title
            walk.textChars += title.count
        }
    }
    if node["value"] != nil || node["title"] != nil { walk.textNodes += 1 }
    walk.nodes.append(node)

    if let children = structure[5] as? [AXUIElement] {
        for child in children {
            walkTree(child, depth: depth + 1, into: &walk)
            if walk.stopped { return }
        }
    }
}

// Chromium builds its accessibility tree only when assistive technology asks
// for it, so a capture tool must ask. AXManualAccessibility is the attribute
// Electron added for exactly this per-application switch; Chrome itself only
// responds to AXEnhancedUserInterface, the VoiceOver signal. Native apps
// return an error for both, harmlessly. Once per pid, so the poll loop does
// not spam IPC; a relaunched app gets a new pid and is enabled again.
private let enabledPidsLock = NSLock()
private var enabledPids = Set<pid_t>()

private func enableAccessibilityOnce(_ pid: pid_t, _ appElement: AXUIElement, _ appName: String) {
    enabledPidsLock.lock()
    let alreadyTried = enabledPids.contains(pid)
    if !alreadyTried { enabledPids.insert(pid) }
    enabledPidsLock.unlock()
    if alreadyTried { return }

    if AXUIElementSetAttributeValue(
        appElement, "AXManualAccessibility" as CFString, kCFBooleanTrue) == .success {
        FileHandle.standardError.write(Data("[ax] enabled AXManualAccessibility for \(appName)\n".utf8))
        return
    }
    if AXUIElementSetAttributeValue(
        appElement, "AXEnhancedUserInterface" as CFString, kCFBooleanTrue) == .success {
        FileHandle.standardError.write(Data("[ax] enabled AXEnhancedUserInterface for \(appName)\n".utf8))
    }
}

/// Reads the focused window of the frontmost application and returns its
/// accessibility tree as JSON. Only the focused window is touched:
/// background windows, minimised windows and unfocused displays are never
/// walked.
@_cdecl("ambient_ax_snapshot")
public func ambientAxSnapshot() -> SRString {
    guard AXIsProcessTrusted() else {
        return SRString("ERROR: permission not granted")
    }

    // A locked screen is not work. Skipping here also lets the capture loop
    // flush the open block, so the morning's last block ends at the lock
    // rather than spanning lunch.
    if let session = CGSessionCopyCurrentDictionary() as? [String: Any],
       session["CGSSessionScreenIsLocked"] as? Bool == true {
        return SRString("ERROR: screen locked")
    }

    guard let frontmost = NSWorkspace.shared.frontmostApplication else {
        return SRString("ERROR: no frontmost application")
    }

    // This app's own window is never read. An accessibility query on the
    // calling process does not cross a process boundary: HIServices answers
    // it in-process, straight into AppKit's NSWindow accessors, and those
    // trap when called off the main thread. Seen as a SIGTRAP ("Must only
    // be used from the main thread") the moment a poll tick lands while the
    // window has just been brought to the front from the Dock. The Rust
    // side already treats a look at the app itself as not work, so an error
    // here costs nothing that was wanted.
    if frontmost.processIdentifier == ProcessInfo.processInfo.processIdentifier {
        return SRString("ERROR: own window in front")
    }

    return SRString(snapshotJSON(of: frontmost))
}

/// The snapshot of one application's focused window, or an `ERROR: `
/// string. Separate from the frontmost lookup so `scripts/ax-snapshot.sh`
/// can measure a named app without bringing it to the front.
func snapshotJSON(of frontmost: NSRunningApplication) -> String {
    let appName = frontmost.localizedName ?? "Unknown"
    let appElement = AXUIElementCreateApplication(frontmost.processIdentifier)

    // AX calls are synchronous IPC into the target process. Without a
    // timeout, a hung application freezes this thread with it; with one, a
    // slow target costs a bounded slice of a single tick.
    AXUIElementSetMessagingTimeout(appElement, 0.5)

    // The tree takes a few seconds to build after this first asks, so the
    // first reads of a freshly enabled app are thin and fill in on later
    // polls. That is fine for a background loop.
    enableAccessibilityOnce(frontmost.processIdentifier, appElement, appName)

    guard let focused = copyAttribute(appElement, kAXFocusedWindowAttribute as String) else {
        return "ERROR: no focused window"
    }
    let window = focused as! AXUIElement

    let title = copyAttribute(window, kAXTitleAttribute as String) as? String

    // The window's backing file, where the app exposes one (Preview, Xcode,
    // TextEdit and most document apps do). A path is worth more than any
    // amount of scraped text: the consuming LLM can open the real document.
    var document = copyAttribute(window, "AXDocument") as? String
    if document == nil, let docUrl = copyAttribute(window, "AXDocument") {
        document = (docUrl as? NSURL)?.absoluteString
    }

    var walk = Walk()
    walkTree(window, depth: 0, into: &walk)

    let payload: [String: Any] = [
        "app": appName,
        "bundle_id": frontmost.bundleIdentifier as Any,
        "window_title": title as Any,
        "document": document as Any,
        "url": walk.webUrl as Any,
        "nodes": walk.nodes,
        "visited": walk.visited,
        "truncated": walk.stopped,
    ]

    return serialiseSnapshot(payload)
}

/// The payload as JSON, or an `ERROR: ` string the capture loop skips.
/// JSONSerialization reports a bad value by raising an Objective-C
/// exception rather than throwing, so `try?` alone does not protect the
/// app: the payload is checked first. `finiteFrame` keeps the known source
/// of non-finite numbers out; this catches any that slip in later.
func serialiseSnapshot(_ payload: [String: Any]) -> String {
    guard JSONSerialization.isValidJSONObject(payload) else {
        FileHandle.standardError.write(Data("[ax] skipped a snapshot holding a non-JSON value\n".utf8))
        return "ERROR: snapshot holds a non-JSON value"
    }
    guard let data = try? JSONSerialization.data(withJSONObject: payload),
          let json = String(data: data, encoding: .utf8) else {
        return "ERROR: could not serialise snapshot"
    }
    return json
}

// MARK: - Window chrome

/// Hides the three traffic light buttons on one window, leaving the rest of
/// the native title bar in place. The window keeps its system corner mask
/// and its edge resizing, which going borderless would both give up; only
/// the buttons go, because the page draws its own.
///
/// Identified by the NSWindow pointer Tauri already holds rather than by
/// title, which would pick the wrong window the moment two of them matched.
/// AppKit is main-thread only, so the caller marshals.
@_cdecl("ambient_hide_window_buttons")
public func ambientHideWindowButtons(_ windowPointer: Int64) -> Int32 {
    guard windowPointer != 0,
          let raw = UnsafeRawPointer(bitPattern: Int(windowPointer)) else {
        return 0
    }
    let window = Unmanaged<NSWindow>.fromOpaque(raw).takeUnretainedValue()
    var hidden: Int32 = 0
    for kind: NSWindow.ButtonType in [.closeButton, .miniaturizeButton, .zoomButton] {
        if let button = window.standardWindowButton(kind) {
            button.isHidden = true
            hidden += 1
        }
    }
    return hidden
}

/// Lightens the traffic lights on an unfocused window. Since macOS 26 the
/// inactive buttons are not a fixed grey but a translucent tint over
/// whatever sits behind them, so on this app's navy title bar the light
/// appearance renders them darker than the bar itself (measured #000058 on
/// #000080) and they read as three black holes. The dark appearance tints
/// the other way and they come out lighter than the bar. The focused red,
/// amber and green are opaque and unchanged either way.
///
/// The appearance goes on the window, which is what the title bar's theme
/// frame draws the buttons from. The content view is then pinned back to
/// light. That second half is belt and braces rather than load bearing:
/// WebKit takes prefers-color-scheme from the page's own `color-scheme`,
/// which this one never declares, so the content renders light either way,
/// and captures of the Settings tab with and without it are identical. It
/// stays because the day someone writes `color-scheme: light dark` the
/// window's appearance becomes the thing that decides, and a page with no
/// dark palette would turn dark with no obvious cause.
///
/// Returns the number of layers set, so a missing content view is visible
/// to the caller rather than passing silently. AppKit is main-thread only,
/// so the caller marshals.
@_cdecl("ambient_lighten_inactive_traffic_lights")
public func ambientLightenInactiveTrafficLights(_ windowPointer: Int64) -> Int32 {
    guard windowPointer != 0,
          let raw = UnsafeRawPointer(bitPattern: Int(windowPointer)) else {
        return 0
    }
    let window = Unmanaged<NSWindow>.fromOpaque(raw).takeUnretainedValue()
    window.appearance = NSAppearance(named: .darkAqua)
    guard let content = window.contentView else { return 1 }
    content.appearance = NSAppearance(named: .aqua)
    return 2
}

/// Moves the traffic-light cluster without touching the title bar container,
/// which keeps the window layout intact. `x` is the close button's leading
/// edge in window coordinates; `yFromTop` is the distance from the window's
/// top edge to the top of the buttons. AppKit is main-thread only.
@_cdecl("ambient_position_traffic_lights")
public func ambientPositionTrafficLights(
    _ windowPointer: Int64,
    x: Double,
    yFromTop: Double
) -> Int32 {
    guard windowPointer != 0,
          let raw = UnsafeRawPointer(bitPattern: Int(windowPointer)) else {
        return 0
    }
    let window = Unmanaged<NSWindow>.fromOpaque(raw).takeUnretainedValue()
    guard let close = window.standardWindowButton(.closeButton),
          let mini = window.standardWindowButton(.miniaturizeButton),
          let zoom = window.standardWindowButton(.zoomButton) else {
        return 0
    }

    let windowHeight = window.frame.size.height
    let spaceBetween = mini.frame.origin.x - close.frame.origin.x
    let buttons = [close, mini, zoom]

    func place() {
        for (index, button) in buttons.enumerated() {
            guard let superview = button.superview else { continue }
            button.isHidden = false
            let originInWindow = NSPoint(
                x: x + Double(index) * spaceBetween,
                y: windowHeight - yFromTop - button.frame.size.height
            )
            button.setFrameOrigin(superview.convert(originInWindow, from: nil))
        }
    }

    // One layout pass after show, so standardWindowButton frames are real.
    DispatchQueue.main.async(execute: place)

    return Int32(buttons.count)
}
