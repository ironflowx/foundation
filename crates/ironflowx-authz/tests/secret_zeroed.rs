//! TEN-3: parsing and minting a key leave no copy of its secret in freed memory. A global
//! allocator looks at every block as it is freed. Its own test binary, since the allocator is
//! process-wide.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, Ordering};

use ironflowx_authz::{mint, parse};
use ironflowx_core::{KeyId, Secret};

/// A key's 256 bits, as drawn.
const RAW: &[u8; 32] = b"bits that must not outlive a key";
/// The same, as they stand in its text: base64url without padding.
const ENCODED: &[u8] = b"Yml0cyB0aGF0IG11c3Qgbm90IG91dGxpdmUgYSBrZXk";

static ARMED: AtomicBool = AtomicBool::new(false);
static SEEN: AtomicBool = AtomicBool::new(false);

struct Probe;

// SAFETY: forwards to `System`; reads a block only while it is still allocated, before freeing.
// The default `realloc` frees the old block through `dealloc`, so a growing one is seen too.
unsafe impl GlobalAlloc for Probe {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ARMED.load(Ordering::SeqCst) {
            let block = unsafe { std::slice::from_raw_parts(ptr, layout.size()) };
            // Half of it is enough to matter, and a block grown in steps may hold no more.
            for part in [&RAW[..16], &ENCODED[..21]] {
                if block.windows(part.len()).any(|w| w == part) {
                    SEEN.store(true, Ordering::SeqCst);
                }
            }
        }
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static PROBE: Probe = Probe;

/// Runs `f` with the probe armed: its result, and whether a block freed meanwhile held the
/// secret.
fn probed<T>(f: impl FnOnce() -> T) -> (T, bool) {
    SEEN.store(false, Ordering::SeqCst);
    ARMED.store(true, Ordering::SeqCst);
    let result = f();
    ARMED.store(false, Ordering::SeqCst);
    (result, SEEN.load(Ordering::SeqCst))
}

/// One test: the probe is process-wide, so two at once would see each other's blocks.
#[test]
fn parsing_and_minting_a_key_leave_no_copy_of_its_secret() {
    // The probe works: a plain buffer leaves either form behind.
    for copy in [&RAW[..], ENCODED] {
        let ((), seen) = probed(|| drop(black_box(copy.to_vec())));
        assert!(seen, "the probe sees an unzeroed block");
    }

    let id: KeyId = "019a9f3e-7c2b-7d41-8a3e-5f6b7c8d9e0f".parse().unwrap();
    let ((key, digest), seen) = probed(|| mint(id, *RAW));
    assert!(!seen, "minting left the secret in freed memory");
    assert_eq!(&key.expose().as_bytes()[41..], ENCODED);

    let (parts, seen) = probed(|| parse(&key));
    assert!(!seen, "parsing left the secret in freed memory");
    assert!(parts.unwrap().digest.ct_eq(&digest));

    let padded = Secret::new(format!("{}=", &key.expose()[..83]));
    let (parts, seen) = probed(|| parse(&padded));
    assert!(parts.is_err());
    assert!(!seen, "refusing left the secret in freed memory");
}
