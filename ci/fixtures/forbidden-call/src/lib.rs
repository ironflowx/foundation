//! Purity fixture: a pure crate that reads the clock, the filesystem, the network and OS
//! randomness, spawns a thread, prints and keeps a HashMap.
//! `ci/must-reject.sh` requires clippy to refuse it. Its header matches the pure crates' lib.rs.

// Purity (LED-15): forbid, so an inner #[allow] cannot switch these off.
#![forbid(unsafe_code)]
#![forbid(clippy::disallowed_methods, clippy::disallowed_types)]
#![forbid(clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro)]

pub fn reads_the_clock() -> std::time::SystemTime {
    std::time::SystemTime::now()
}

pub fn reads_a_file() -> std::io::Result<Vec<u8>> {
    std::fs::read("x")
}

pub fn holds_a_socket(_socket: std::net::TcpStream) {}

pub fn spawns_a_thread() {
    std::thread::spawn(|| ());
}

pub fn reads_os_randomness() -> Result<u64, getrandom::Error> {
    getrandom::u64()
}

pub fn checks_a_path() -> bool {
    std::path::Path::new("x").exists()
}

pub fn prints() {
    println!("x");
}

pub fn keeps_a_hash_map() -> usize {
    std::collections::HashMap::<u8, u8>::default().len()
}
