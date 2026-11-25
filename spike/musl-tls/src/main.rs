//! The probe of #160. `spike engine <dir>` | `spike https <url>` | `spike serve`.

use std::error::Error;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use connectrpc::client::{ClientTransport as _, HttpClient, full_body};
use connectrpc::rustls::{ClientConfig, RootCertStore, client::WebPkiServerVerifier};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use hyper_util::service::TowerToHyperService;
use ironflowx_cell::HostClock;
use ironflowx_operations::Engine;
use ironflowx_proto::{Call, EmitRequest, GetEventRequest, Operations as _};

type Failure = Box<dyn Error + Send + Sync>;

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let probe = match args[..] {
        ["engine", dir] => engine(Path::new(dir)).await,
        ["https", url] => https(url).await,
        ["serve"] => serve().await,
        _ => Err("usage: spike engine <dir> | https <url> | serve".into()),
    };
    match probe {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            chain(&*e);
            ExitCode::FAILURE
        }
    }
}

/// Every cause, with the `rustls` error an I/O error holds named as such.
fn chain(e: &(dyn Error + 'static)) {
    let mut cause = Some(e);
    while let Some(e) = cause {
        println!("error: {e}");
        if let Some(tls) = e.downcast_ref::<rustls::Error>() {
            println!("rustls: {tls:?}");
        }
        let inner = e.downcast_ref::<std::io::Error>().and_then(|io| io.get_ref());
        cause = match inner {
            Some(inner) => Some(inner),
            None => e.source(),
        };
    }
}

/// Bootstraps an engine in an empty directory, emits, reads back, reopens and reads again.
async fn engine(dir: &Path) -> Result<(), Failure> {
    let held = std::fs::read_dir(dir)?.count();
    println!("engine: {} holds {held} entries", dir.display());
    if held != 0 {
        return Err("the directory is not empty".into());
    }
    let opened = Engine::open(dir, HostClock, None).await.map_err(|e| format!("{e:?}"))?;
    let call = Call::with_key(opened.minted.ok_or("the first open minted no key")?);
    let emit = EmitRequest {
        name: "order.placed".to_owned(),
        data: Some(br#"{"id":1}"#.to_vec()),
        metadata: None,
        version: None,
        idempotency_key: None,
    };
    let emitted = opened.engine.emit(&call, emit).await.map_err(|e| format!("{e:?}"))?;
    println!("engine: emitted {}, duplicate {}", emitted.event_id, emitted.duplicate);
    opened.engine.close().await;
    let reopened = Engine::open(dir, HostClock, None).await.map_err(|e| format!("{e:?}"))?;
    if reopened.minted.is_some() {
        return Err("the reopen minted a key, so it did not find the first engine".into());
    }
    let get = GetEventRequest { event_id: emitted.event_id.to_string() };
    let event = reopened.engine.get_event(&call, get).await.map_err(|e| format!("{e:?}"))?;
    if event.data != br#"{"id":1}"# {
        return Err("the event reads back with other data".into());
    }
    println!("engine: reopened, read {} {}", event.name, String::from_utf8_lossy(&event.data));
    reopened.engine.close().await;
    list(dir)
}

fn list(dir: &Path) -> Result<(), Failure> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            list(&path)?;
        } else {
            println!("engine: file {} {} bytes", path.display(), path.metadata()?.len());
        }
    }
    Ok(())
}

/// One GET through `connectrpc`'s own TLS client, as #148 would build it.
async fn https(url: &str) -> Result<(), Failure> {
    for name in ["SSL_CERT_FILE", "SSL_CERT_DIR"] {
        println!("https: {name}={:?}", std::env::var_os(name));
    }
    let native = rustls_native_certs::load_native_certs();
    println!("https: native roots {}, errors {:?}", native.certs.len(), native.errors);
    let mut roots = RootCertStore::empty();
    let (added, ignored) = roots.add_parsable_certificates(native.certs);
    println!("https: roots added {added}, ignored {ignored}");
    let verifier = WebPkiServerVerifier::builder(Arc::new(roots.clone())).build();
    println!("https: WebPkiServerVerifier::builder(roots).build() is {:?}", verifier.map(|_| "ok"));
    let config = ClientConfig::builder().with_root_certificates(roots).with_no_client_auth();
    let provider = config.crypto_provider();
    println!(
        "https: provider's first key exchange {:?}, random {:?}",
        provider.kx_groups[0], provider.secure_random
    );
    let client = HttpClient::with_tls(Arc::new(config));
    let request = http::Request::get(url).body(full_body(Default::default()))?;
    let answer = tokio::time::timeout(Duration::from_secs(30), client.send(request)).await??;
    println!("https: {url} answered {:?} {}", answer.version(), answer.status());
    Ok(())
}

/// A `connectrpc` router in axum, on `hyper-util`'s accept loop, asked by `connectrpc`'s client.
async fn serve() -> Result<(), Failure> {
    let app = axum::Router::new()
        .route("/health", axum::routing::get(|| async { "ok" }))
        .fallback_service(connectrpc::Router::new().into_axum_service());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let service = TowerToHyperService::new(app.clone());
            tokio::spawn(async move {
                let http = auto::Builder::new(TokioExecutor::new());
                http.serve_connection(TokioIo::new(stream), service).await
            });
        }
    });
    let client = HttpClient::plaintext();
    let request = http::Request::get(format!("http://{address}/health"));
    let answer = client.send(request.body(full_body(Default::default()))?).await?;
    println!("serve: /health answered {}", answer.status());
    let request = http::Request::post(format!("http://{address}/no.Service/Method"))
        .header("content-type", "application/json");
    let answer = client.send(request.body(full_body("{}".into()))?).await?;
    println!("serve: a procedure no service has answered {}", answer.status());
    Ok(())
}
