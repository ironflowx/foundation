//! The host while `Engine::open` is held at a barrier. Alone in its process: the fault hook is
//! process-wide.

use std::io::{Read as _, Write as _};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use ironflowx_bin::{ServeError, run};
use ironflowx_core::{ErrorKind, FaultPoint, Secret, set_fault_hook};
use ironflowx_proto::{Call, Client, OpError, Operations, WhoamiRequest};
use tokio::net::TcpListener;

/// Says that the open has reached the barrier, then waits to be let go.
static BARRIER: Mutex<Option<(Sender<()>, Receiver<()>)>> = Mutex::new(None);

fn hold(point: FaultPoint) {
    if point == FaultPoint::BeforeRename {
        let (reached, go) = BARRIER.lock().unwrap().take().expect("a barrier");
        reached.send(()).unwrap();
        go.recv().unwrap();
    }
}

fn barrier() -> (Receiver<()>, Sender<()>) {
    set_fault_hook(hold);
    let ((tell, reached), (go, gone)) = (channel(), channel());
    *BARRIER.lock().unwrap() = Some((tell, gone));
    (reached, go)
}

async fn bound() -> (TcpListener, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    (listener, addr)
}

fn get(addr: SocketAddr, path: &str) -> String {
    let mut stream = std::net::TcpStream::connect(addr).unwrap();
    write!(stream, "GET {path} HTTP/1.0\r\n\r\n").unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    answer
}

/// Waits until `path` is there: the host has written it.
async fn written(path: &Path) -> String {
    for _ in 0..1_000 {
        if let Ok(text) = std::fs::read_to_string(path) {
            return text;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{} is not written", path.display());
}

const LATE: Duration = Duration::from_secs(10);

/// One test, since the barrier is the process's: SRV-9's opening state, then a key that cannot be
/// written.
#[tokio::test(flavor = "multi_thread")]
async fn the_slot_answers_until_the_engine_is_open_and_the_key_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let key_file = data_dir.join("admin.key");
    let (reached, go) = barrier();
    let (listener, addr) = bound().await;
    let host = tokio::spawn({
        let data_dir = data_dir.clone();
        async move { run(listener, &data_dir).await }
    });
    reached.recv_timeout(LATE).expect("the open is held");
    let client = Client::new(&format!("http://{addr}")).unwrap();
    let call = Call::with_key(Secret::new("not a key".into()));
    let starting = OpError {
        kind: ErrorKind::Unavailable,
        message: "the engine is unavailable".into(),
        hint: Some("the engine is starting".into()),
    };
    let answer = client.whoami(&call, WhoamiRequest {}).await;
    assert_eq!(answer, Err(starting));
    let ready = get(addr, "/ready");
    assert!(ready.starts_with("HTTP/1.0 503 "), "{ready}");
    assert!(
        ready.ends_with(r#"{"ready":false,"waiting_on":"open"}"#),
        "{ready}"
    );
    assert!(get(addr, "/health").starts_with("HTTP/1.0 200 "));
    assert!(!key_file.exists());

    go.send(()).unwrap();
    let key = written(&key_file).await;
    for _ in 0..1_000 {
        if get(addr, "/ready").starts_with("HTTP/1.0 200 ") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let ready = get(addr, "/ready");
    assert!(ready.starts_with("HTTP/1.0 200 "), "{ready}");
    let call = Call::with_key(Secret::new(key.trim_end().to_owned()));
    let me = client.whoami(&call, WhoamiRequest {}).await.unwrap();
    assert_eq!(me.key_id.to_string(), key[4..40]);
    host.abort();

    // Another's `admin.key` appears while the engine opens: it is kept, and the host gives up.
    let data_dir = dir.path().join("other");
    let key_file = data_dir.join("admin.key");
    let (reached, go) = barrier();
    let (listener, _) = bound().await;
    let host = tokio::spawn({
        let data_dir = data_dir.clone();
        async move { run(listener, &data_dir).await }
    });
    reached.recv_timeout(LATE).expect("the open is held");
    std::fs::write(&key_file, "another's").unwrap();
    go.send(()).unwrap();
    let refused = host.await.unwrap().unwrap_err();
    assert!(
        matches!(&refused, ServeError::KeyFile(path, e)
            if *path == key_file && e.kind() == std::io::ErrorKind::AlreadyExists),
        "{refused}"
    );
    assert_eq!(refused.exit_code(), 1);
    assert_eq!(std::fs::read_to_string(&key_file).unwrap(), "another's");
    assert_eq!(temporaries(&data_dir), 0);
    // The engine was closed: its root is free, and bootstrapped, so `admin.key` is not stale.
    let (listener, addr) = bound().await;
    tokio::spawn(async move { run(listener, &data_dir).await });
    for _ in 0..1_000 {
        if get(addr, "/ready").starts_with("HTTP/1.0 200 ") {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the engine is not ready");
}

fn temporaries(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .unwrap()
        .filter(|e| {
            let name = e.as_ref().unwrap().file_name();
            name.to_string_lossy().ends_with(".tmp")
        })
        .count()
}
