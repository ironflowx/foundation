//! `ironflowx serve`, as a process.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, ChildStderr, Command, Stdio};
use std::time::{Duration, Instant};

use ironflowx_cell::{HostClock, new_env_ref};
use ironflowx_operations::Engine;
use ironflowx_store::{Bootstrap, ControlPlane};
use serde_json::Value;

const ID: &str = "0190b5f0-0000-7000-8000-000000000001";

fn command(data_dir: &Path, listen: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ironflowx"));
    command.args(["serve", "--listen", listen, "--data-dir"]);
    command
        .arg(data_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    command
}

/// A served engine; killed when dropped.
struct Served {
    child: Child,
    /// What it has logged so far: its `listening` line.
    listening: String,
    log: BufReader<ChildStderr>,
    addr: SocketAddr,
}

impl Drop for Served {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Served {
    /// Starts it and waits until it is ready.
    fn start(data_dir: &Path, listen: &str) -> Self {
        let mut child = command(data_dir, listen)
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut log = BufReader::new(child.stderr.take().unwrap());
        let mut listening = String::new();
        log.read_line(&mut listening).unwrap();
        let line: Value = serde_json::from_str(&listening).unwrap();
        assert_eq!(line["fields"]["message"], "listening", "{line}");
        let url = line["fields"]["url"].as_str().unwrap();
        let addr: SocketAddr = url.strip_prefix("http://").unwrap().parse().unwrap();
        let addr = SocketAddr::from(([127, 0, 0, 1], addr.port()));
        let served = Self {
            child,
            listening,
            log,
            addr,
        };
        let started = Instant::now();
        while !get(served.addr, "/ready").starts_with("HTTP/1.0 200 ") {
            assert!(started.elapsed() < Duration::from_secs(30), "not ready");
            std::thread::sleep(Duration::from_millis(10));
        }
        served
    }

    /// Stops it; all it logged.
    fn stop(mut self) -> String {
        self.child.kill().unwrap();
        let mut log = std::mem::take(&mut self.listening);
        self.log.read_to_string(&mut log).unwrap();
        log
    }
}

fn get(addr: SocketAddr, path: &str) -> String {
    let mut stream = TcpStream::connect(addr).unwrap();
    write!(stream, "GET {path} HTTP/1.0\r\n\r\n").unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    answer
}

/// Runs a start that must fail: its exit code and its log.
fn refused(data_dir: &Path, listen: &str) -> (Option<i32>, String) {
    let out = command(data_dir, listen).output().unwrap();
    (out.status.code(), String::from_utf8(out.stderr).unwrap())
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

#[test]
fn the_first_start_writes_the_key_and_a_later_one_keeps_what_is_there() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("a/data");
    let key_file = data_dir.join("admin.key");
    let log = Served::start(&data_dir, "127.0.0.1:0").stop();
    assert_eq!((mode(&data_dir), mode(&key_file)), (0o700, 0o400));
    let written = std::fs::read_to_string(&key_file).unwrap();
    let key = written.strip_suffix('\n').unwrap();
    assert_eq!((key.len(), &key[..4]), (84, "ixk_"));
    assert!(log.contains(key_file.to_str().unwrap()), "{log}");
    assert!(!log.contains(&key[41..]), "{log}");
    assert!(!log.contains("TLS proxy"), "{log}");
    let names: Vec<_> = std::fs::read_dir(&data_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.starts_with("admin.key"))
        .collect();
    assert_eq!(names, ["admin.key"]);

    // Not stale: this directory's engine wrote it.
    let log = Served::start(&data_dir, "127.0.0.1:0").stop();
    assert_eq!(std::fs::read_to_string(&key_file).unwrap(), written);
    assert!(!log.contains("admin.key"), "{log}");
    assert!(!log.contains(&key[41..]), "{log}");
    std::fs::remove_file(&key_file).unwrap();
    Served::start(&data_dir, "127.0.0.1:0").stop();
    assert!(!key_file.exists());
}

#[test]
fn a_data_directory_that_is_there_keeps_its_mode() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), PermissionsExt::from_mode(0o750)).unwrap();
    Served::start(dir.path(), "127.0.0.1:0").stop();
    assert_eq!(mode(dir.path()), 0o750);
}

#[test]
fn a_key_file_without_a_control_plane_is_stale_before_the_address_is_bound() {
    let taken = TcpListener::bind("127.0.0.1:0").unwrap();
    let listen = taken.local_addr().unwrap().to_string();
    for link in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let key_file = dir.path().join("admin.key");
        if link {
            std::os::unix::fs::symlink("nothing", &key_file).unwrap();
        } else {
            std::fs::write(&key_file, "another install's").unwrap();
        }
        let (code, log) = refused(dir.path(), &listen);
        assert_eq!(code, Some(2), "{log}");
        assert!(log.contains("admin.key is another install's"), "{log}");
        assert!(!log.contains("listen"), "{log}");
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["admin.key"]);
        if !link {
            assert_eq!(std::fs::read(&key_file).unwrap(), b"another install's");
        }
    }
}

/// SRV-7: an open writes `admin.key` only once it has marked the bootstrap.
#[test]
fn a_key_file_before_the_bootstrap_is_marked_is_stale() {
    for bootstrapped in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let control = ControlPlane::open(&dir.path().join("control.db")).unwrap();
        if bootstrapped {
            let (_, digest) = ironflowx_authz::mint(ID.parse().unwrap(), [7; 32]);
            let b = Bootstrap {
                env: new_env_ref(),
                tenant: ID.parse().unwrap(),
                project: ID.parse().unwrap(),
                key: ironflowx_authz::KeyParts {
                    id: ID.parse().unwrap(),
                    digest,
                },
                by_seed: false,
            };
            control.ensure_bootstrap(&b, &HostClock).unwrap();
        }
        drop(control);
        let key_file = dir.path().join("admin.key");
        std::fs::write(&key_file, "another install's").unwrap();
        let (code, log) = refused(dir.path(), "127.0.0.1:0");
        assert_eq!(code, Some(2), "{log}");
        assert!(log.contains("admin.key is another install's"), "{log}");
        assert_eq!(std::fs::read(&key_file).unwrap(), b"another install's");
        // The engine did not open: it would have made these.
        assert!(!dir.path().join("engine.lock").exists());
        assert!(!dir.path().join("ledgers").exists());
    }
}

/// Whether the bootstrap is marked cannot be read, so the open says why.
#[test]
fn a_key_file_beside_a_broken_control_plane_is_not_called_stale() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("control.db"), "not a database").unwrap();
    std::fs::write(dir.path().join("admin.key"), "a key").unwrap();
    let (code, log) = refused(dir.path(), "127.0.0.1:0");
    assert_eq!(code, Some(1), "{log}");
    assert!(!log.contains("another install's"), "{log}");
    assert!(log.contains("not a database"), "{log}");
}

/// D62: a start that cannot listen changes nothing on disk.
#[test]
fn an_address_in_use_leaves_no_data_directory() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let taken = TcpListener::bind("127.0.0.1:0").unwrap();
    let listen = taken.local_addr().unwrap().to_string();
    let (code, log) = refused(&data_dir, &listen);
    assert_eq!(code, Some(1), "{log}");
    assert!(log.contains(&format!("cannot listen: {listen}: ")), "{log}");
    assert!(!data_dir.exists());
}

#[test]
fn any_other_refusal_exits_1_and_names_it() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let opened = runtime
        .block_on(Engine::open(dir.path(), HostClock, None))
        .unwrap();
    let (code, log) = refused(dir.path(), "127.0.0.1:0");
    assert_eq!(code, Some(1), "{log}");
    assert!(log.contains("the engine root is in use"), "{log}");
    runtime.block_on(opened.engine.close());

    let file = dir.path().join("file");
    std::fs::write(&file, "").unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink("nothing", &link).unwrap();
    for data_dir in [file.join("data"), file, link] {
        let (code, log) = refused(&data_dir, "127.0.0.1:0");
        assert_eq!(code, Some(1), "{log}");
        assert!(log.contains("cannot create "), "{log}");
    }
}

#[test]
fn an_address_off_this_machine_warns_that_keys_travel_in_clear() {
    let dir = tempfile::tempdir().unwrap();
    let log = Served::start(dir.path(), "0.0.0.0:0").stop();
    let warned: Vec<Value> = log
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .filter(|l: &Value| l["level"] == "WARN")
        .collect();
    assert_eq!(warned.len(), 1, "{log}");
    let message = &warned[0]["fields"]["message"];
    assert_eq!(message, "keys travel in clear without a TLS proxy");
}

/// The deadline of `accept.rs`'s tests, on a real connection.
#[test]
fn a_silent_connection_is_closed() {
    let dir = tempfile::tempdir().unwrap();
    let served = Served::start(dir.path(), "127.0.0.1:0");
    let mut stream = TcpStream::connect(served.addr).unwrap();
    let late = Some(Duration::from_secs(30));
    stream.set_read_timeout(late).unwrap();
    assert_eq!(stream.read(&mut [0]).unwrap(), 0);
}

fn wait_for(path: &Path) {
    let started = Instant::now();
    while !path.exists() && started.elapsed() < Duration::from_secs(30) {
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// `serve` under `umask`.
fn under_umask(umask: &str, data_dir: &Path) -> Child {
    let shell = "umask $2; exec \"$0\" serve --listen 127.0.0.1:0 --data-dir \"$1\"";
    Command::new("sh")
        .args(["-c", shell, env!("CARGO_BIN_EXE_ironflowx")])
        .arg(data_dir)
        .arg(umask)
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

#[test]
fn a_directory_above_the_data_directory_is_private_too() {
    let dir = tempfile::tempdir().unwrap();
    let above = dir.path().join("above");
    let data_dir = above.join("data");
    let mut child = under_umask("0000", &data_dir);
    wait_for(&data_dir.join("admin.key"));
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!((mode(&above), mode(&data_dir)), (0o700, 0o700));
}

#[test]
fn the_key_file_is_its_owners_to_read_whatever_the_umask() {
    let dir = tempfile::tempdir().unwrap();
    let key_file = dir.path().join("admin.key");
    // A bootstrap left unmarked, its ledger built: the next open mints and makes no other file.
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let open = Engine::open(dir.path(), HostClock, None);
    let engine = runtime.block_on(open).unwrap().engine;
    runtime.block_on(engine.close());
    let control = rusqlite::Connection::open(dir.path().join("control.db")).unwrap();
    let unmark = "UPDATE environments SET ledger_created_at = NULL";
    assert_eq!(control.execute(unmark, []).unwrap(), 1);
    drop(control);
    let mut child = under_umask("0477", dir.path());
    wait_for(&key_file);
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(mode(&key_file), 0o400);
}
