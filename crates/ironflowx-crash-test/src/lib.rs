//! The crash test (#6). The child process, `crash-child`, owns a ledger; the parent, in
//! `tests/crash.rs`, drives it over a pipe, kills it at a named fault point and checks what a
//! reopened ledger and the read store show.
