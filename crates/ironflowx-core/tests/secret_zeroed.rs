//! TEN-3: a `Secret` is zeroed on drop. A global allocator looks at every block as it is freed;
//! the marker must never be in one freed by dropping a `Secret`. Its own test binary, since the
//! allocator is process-wide.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, Ordering};

use ironflowx_core::Secret;

const MARKER: &[u8] = b"ixk_marker_that_must_not_survive_a_drop";

static ARMED: AtomicBool = AtomicBool::new(false);
static SEEN: AtomicBool = AtomicBool::new(false);

struct Probe;

// SAFETY: forwards to `System`; reads a block only while it is still allocated, before freeing.
unsafe impl GlobalAlloc for Probe {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ARMED.load(Ordering::SeqCst) {
            let block = unsafe { std::slice::from_raw_parts(ptr, layout.size()) };
            if block.windows(MARKER.len()).any(|w| w == MARKER) {
                SEEN.store(true, Ordering::SeqCst);
            }
        }
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static PROBE: Probe = Probe;

/// Drops `value` with the probe armed; true if a freed block still held the marker.
fn marker_survives<T>(value: T) -> bool {
    SEEN.store(false, Ordering::SeqCst);
    ARMED.store(true, Ordering::SeqCst);
    drop(value);
    ARMED.store(false, Ordering::SeqCst);
    SEEN.load(Ordering::SeqCst)
}

fn text() -> String {
    let mut s = String::with_capacity(MARKER.len());
    s.push_str(std::str::from_utf8(MARKER).unwrap());
    s
}

#[test]
fn a_secret_is_zeroed_on_drop() {
    // The probe works: a plain String leaves its bytes behind.
    assert!(marker_survives(text()), "the probe sees an unzeroed block");
    let secret = Secret::new(text());
    assert_eq!(secret.expose().as_bytes(), MARKER);
    assert!(
        !marker_survives(secret),
        "a dropped Secret left its text in memory"
    );
}
