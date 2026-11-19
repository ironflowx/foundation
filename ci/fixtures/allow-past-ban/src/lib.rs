//! Purity fixture: a pure crate that switches a ban off with #[allow]. Its header matches the
//! pure crates' lib.rs, whose forbid must turn the #[allow] into an error.

// Purity (LED-15): forbid, so an inner #[allow] cannot switch these off.
#![forbid(unsafe_code)]
#![forbid(clippy::disallowed_methods, clippy::disallowed_types)]
#![forbid(clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro)]

#[allow(clippy::disallowed_methods)]
pub fn allows_itself_the_clock() -> std::time::Instant {
    std::time::Instant::now()
}
