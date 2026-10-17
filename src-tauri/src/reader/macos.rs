#![cfg(target_os = "macos")]

use super::{parse_snapshot_json, permission_from_code, Permission, Snapshot};
use swift_rs::{autoreleasepool, swift, SRString};

swift!(fn ambient_ax_permission_status() -> i32);
swift!(fn ambient_ax_request_permission() -> i32);
swift!(fn ambient_ax_snapshot() -> SRString);
swift!(fn ambient_ax_seconds_since_input() -> f64);

// Every call below runs inside its own autorelease pool. The callers are
// plain Rust threads, the capture loop's among them, and a thread without a
// pool sends whatever Swift autoreleases to one the runtime makes for it and
// drains only when the thread exits. The capture thread never exits, so
// every snapshot stayed in memory: a few hundred KB a tick, 4 GB in two
// days. Swift returns `SRString` autoreleased, as Objective-C returns
// any object, so the pool must outlive the call on this side; a pool inside
// the Swift function cannot reach it.

pub fn permission_status() -> Permission {
    permission_from_code(autoreleasepool!(unsafe { ambient_ax_permission_status() }))
}

pub fn request_permission() -> Permission {
    permission_from_code(autoreleasepool!(unsafe { ambient_ax_request_permission() }))
}

pub fn snapshot() -> Option<Snapshot> {
    match with_snapshot_json(parse_snapshot_json) {
        Ok(snapshot) => Some(snapshot),
        Err(message) => {
            eprintln!("[ax] snapshot failed: {message}");
            None
        }
    }
}

/// Hands the walk's JSON to `read` while the string Swift returned is still
/// alive, then drains the pool it was autoreleased into.
fn with_snapshot_json<R>(read: impl FnOnce(&str) -> R) -> R {
    autoreleasepool!({
        let raw = unsafe { ambient_ax_snapshot() };
        read(raw.as_str())
    })
}

/// Negative means Swift could not read the idle time; the caller must not
/// treat that as a fresh keystroke, so it becomes None.
pub fn seconds_since_input() -> Option<f64> {
    let seconds = autoreleasepool!(unsafe { ambient_ax_seconds_since_input() });
    (seconds >= 0.0).then_some(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::c_void;

    // The leading fields of `struct rusage_info_v0` from <sys/resource.h>,
    // which is all `proc_pid_rusage` needs to be handed for flavor 0.
    #[repr(C)]
    #[derive(Default)]
    struct RusageInfoV0 {
        uuid: [u8; 16],
        user_time: u64,
        system_time: u64,
        pkg_idle_wkups: u64,
        interrupt_wkups: u64,
        pageins: u64,
        wired_size: u64,
        resident_size: u64,
        phys_footprint: u64,
        proc_start_abstime: u64,
        proc_exit_abstime: u64,
    }

    extern "C" {
        fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut c_void) -> i32;
    }

    /// The process's physical footprint, the number Activity Monitor shows
    /// as Memory and `footprint` reports.
    fn footprint() -> u64 {
        let mut info = RusageInfoV0::default();
        let status = unsafe {
            proc_pid_rusage(
                std::process::id() as i32,
                0,
                &mut info as *mut RusageInfoV0 as *mut c_void,
            )
        };
        assert_eq!(status, 0, "proc_pid_rusage failed");
        info.phys_footprint
    }

    /// Takes snapshots the way the capture loop does, from a plain Rust
    /// thread, and checks the footprint does not grow with them. Run with
    /// the Accessibility grant and a window holding some text in front:
    ///
    ///   cd src-tauri && cargo test --lib snapshots_leave_memory_flat -- --ignored --nocapture
    ///
    /// AX_LEAK_ROUNDS sets the number of snapshots measured (default 300).
    #[test]
    #[ignore = "needs the Accessibility grant and a focused window with text"]
    fn snapshots_leave_memory_flat() {
        let rounds: u64 = std::env::var("AX_LEAK_ROUNDS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(300);
        std::thread::spawn(move || {
            let bytes = with_snapshot_json(|json| json.len()) as u64;
            assert!(
                bytes > 1000,
                "the focused window gave a {bytes} byte snapshot; put a window with text in front"
            );
            for _ in 0..20 {
                snapshot();
            }
            let before = footprint();
            for _ in 0..rounds {
                snapshot();
            }
            let after = footprint();
            let per_snapshot = after.saturating_sub(before) / rounds;
            println!(
                "{rounds} snapshots of ~{} KB: footprint {} MB -> {} MB, {} KB per snapshot",
                bytes / 1024,
                before / 1_048_576,
                after / 1_048_576,
                per_snapshot / 1024
            );
            // A retained snapshot costs more than its own JSON size. A tenth
            // of it leaves room for allocator noise and nothing else.
            assert!(
                per_snapshot < bytes / 10,
                "footprint grew {per_snapshot} bytes per {bytes} byte snapshot"
            );
        })
        .join()
        .unwrap();
    }
}
