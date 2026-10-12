#!/bin/bash
# Prints the raw accessibility snapshot of a running app's focused window,
# exactly as the capture loop's Swift walk produces it, without bringing the
# app to the front. For writing and measuring extraction profiles:
#
#   scripts/ax-snapshot.sh com.example.app > /tmp/app.json
#   cd src-tauri && AX_SNAPSHOT=/tmp/app.json \
#     cargo test --lib measure_snapshot -- --ignored --nocapture
#
# The terminal running it needs the Accessibility grant. The output holds
# whatever the window shows, so keep it out of the repository.
set -euo pipefail

if [ $# -ne 1 ]; then
  echo "usage: $0 <bundle-id>" >&2
  exit 2
fi

root="$(cd "$(dirname "$0")/.." && pwd)"
build="$(mktemp -d)"
trap 'rm -rf "$build"' EXIT

# The plugin talks to Rust through swift-rs; a one-line stand-in for its
# string type lets the same source build as a plain executable.
grep -v '^import SwiftRs$' "$root/src-tauri/plugins/ax/macos/Sources/AxPlugin.swift" \
  > "$build/AxPlugin.swift"
cat > "$build/main.swift" <<'SWIFT'
import AppKit

public final class SRString: NSObject {
    let value: String
    init(_ value: String) { self.value = value }
}

let bundleId = CommandLine.arguments[1]
guard let app = NSRunningApplication.runningApplications(withBundleIdentifier: bundleId).first else {
    FileHandle.standardError.write(Data("\(bundleId) is not running\n".utf8))
    exit(1)
}
print(snapshotJSON(of: app))
SWIFT

swiftc -O -o "$build/ax-snapshot" "$build/AxPlugin.swift" "$build/main.swift"
"$build/ax-snapshot" "$1"
