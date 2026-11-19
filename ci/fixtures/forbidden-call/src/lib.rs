//! Purity fixture: a pure crate that reads the clock, the filesystem, the network and OS
//! randomness, and spawns a thread. `ci/must-reject.sh` requires clippy to refuse it.

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
