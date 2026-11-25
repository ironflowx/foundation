//! The host's accept loop: HTTP/1.1 and cleartext HTTP/2. A connection has 10 s to send its first
//! request's head, and on HTTP/1.1 10 s for each later one from its first byte. Nothing else limits
//! an idle connection.

use std::convert::Infallible;
use std::io;
use std::pin::{Pin, pin};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::task::{Context, Poll, ready};
use std::time::{Duration, Instant};

use axum::Router;
use hyper::rt::{Sleep, Timer};
use hyper::service::{Service as _, service_fn};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use hyper_util::service::TowerToHyperService;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;

const HEAD: Duration = Duration::from_secs(10);

pub async fn connections(listener: TcpListener, router: Router) -> Infallible {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                tokio::spawn(connection(stream, router.clone()));
            }
            // The peer's fault, or too many open files: neither ends the loop.
            Err(e) => {
                tracing::warn!("accept: {e}");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

async fn connection(stream: TcpStream, router: Router) {
    let _ = stream.set_nodelay(true);
    let read = Arc::new(AtomicBool::new(false));
    let mut server = auto::Builder::new(TokioExecutor::new());
    server
        .http1()
        .timer(HeadTimer(read.clone()))
        .header_read_timeout(HEAD);
    let arrived = Arc::new(Notify::new());
    let (router, request) = (TowerToHyperService::new(router), arrived.clone());
    let service = service_fn(move |head| {
        request.notify_one();
        router.call(head)
    });
    let io = TokioIo::new(Noted { stream, read });
    let mut serving = pin!(server.serve_connection(io, service));
    // A connection that fails is its peer's to notice.
    tokio::select! {
        biased;
        _ = &mut serving => return,
        () = arrived.notified() => {}
        () = tokio::time::sleep(HEAD) => return,
    }
    let _ = serving.await;
}

/// Times an HTTP/1.1 head from its first byte. hyper's own timer runs from the end of the request
/// before, which would close an idle connection; the first head has the connection's deadline.
/// One whose first bytes came with the request before it counts as idle, and has no limit.
#[derive(Clone)]
struct HeadTimer(Arc<AtomicBool>);

impl Timer for HeadTimer {
    fn sleep(&self, _: Duration) -> Pin<Box<dyn Sleep>> {
        self.0.store(false, Relaxed);
        Box::pin(HeadSleep {
            read: self.0.clone(),
            sleep: None,
        })
    }

    fn sleep_until(&self, deadline: Instant) -> Pin<Box<dyn Sleep>> {
        self.sleep(deadline.saturating_duration_since(Instant::now()))
    }
}

/// Polled at each read of a head that is not whole yet.
struct HeadSleep {
    read: Arc<AtomicBool>,
    sleep: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl Future for HeadSleep {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if !self.read.load(Relaxed) {
            return Poll::Pending;
        }
        let sleep = self
            .sleep
            .get_or_insert_with(|| Box::pin(tokio::time::sleep(HEAD)));
        sleep.as_mut().poll(cx)
    }
}

impl Sleep for HeadSleep {}

/// A connection that notes when bytes were read from it.
struct Noted {
    stream: TcpStream,
    read: Arc<AtomicBool>,
}

impl AsyncRead for Noted {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        ready!(Pin::new(&mut self.stream).poll_read(cx, buf))?;
        self.read.store(true, Relaxed);
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for Noted {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.stream.is_write_vectored()
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}
