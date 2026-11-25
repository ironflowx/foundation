//! An engine behind the HTTP door, called over the network.

#[allow(dead_code)]
#[path = "../../ironflowx-operations/tests/refusals/mod.rs"]
mod refusals;

use std::net::SocketAddr;
use std::sync::Arc;

use connectrpc::Protocol;
use connectrpc::client::{CallOptions, ClientConfig, HttpClient};
use ironflowx_doors::{Readiness, Slot, WaitingOn, router};
use ironflowx_operations::{Engine, Opened};
use ironflowx_proto::wire::{self, EventServiceClient};
use ironflowx_proto::{Client, EventPage, ListEventsRequest, Operations as _};
use refusals::{clock, key};

struct Ready;

impl Readiness for Ready {
    async fn ready(&self) -> Result<(), WaitingOn> {
        Ok(())
    }
}

/// Serves `engine` on a port of its own.
async fn serve(engine: Arc<Engine>) -> SocketAddr {
    let slot = Arc::new(Slot::starting());
    slot.open(engine, Ready);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router(slot)).await });
    addr
}

/// TEN-7, through the client.
#[tokio::test(flavor = "multi_thread")]
async fn the_five_refusals_are_one_answer_through_the_client() {
    refusals::five(async |engine| {
        let addr = serve(engine.clone()).await;
        Arc::new(Client::new(&format!("http://{addr}")).unwrap())
    })
    .await;
}

/// Writes JSON text at random, the same each time (xorshift).
struct Json(u64);

impl Json {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n
    }

    fn any<'a>(&mut self, of: &[&'a str]) -> &'a str {
        of[self.below(of.len() as u64) as usize]
    }

    fn space(&mut self, out: &mut String) {
        for _ in 0..self.below(3) {
            out.push_str(self.any(&["", " ", "\n", "\r\n", "\t", "  "]));
        }
    }

    fn string(&mut self, out: &mut String) {
        out.push('"');
        for _ in 0..self.below(8) {
            out.push_str(self.any(&[
                "a",
                "Z",
                "é",
                "e\u{301}",
                "\u{7f}",
                "\u{2028}",
                "漢",
                "😀",
                "/",
                "'",
                "<",
                "&",
                "\u{feff}",
                "\\\"",
                "\\\\",
                "\\/",
                "\\b",
                "\\f",
                "\\n",
                "\\r",
                "\\t",
                "\\u0001",
                "\\u001F",
                "\\u00e9",
                "\\u00E9",
                "\\ud83d\\ude00",
                "\\uFFFF",
                "\u{ffff}",
            ]));
        }
        out.push('"');
    }

    fn number(&mut self, out: &mut String) {
        out.push_str(self.any(&["", "-"]));
        match self.below(4) {
            0 => out.push_str(&(u64::MAX - self.below(1 << 40)).to_string()),
            1 => out.push_str(&((1 << 53) + self.below(1 << 20)).to_string()),
            // At most 30 digits, the first no zero.
            2 => {
                out.push('9');
                out.push_str(&"00719925474099310071992547409"[self.below(29) as usize..]);
            }
            _ => out.push_str(self.any(&["0", "0.0", "1.50", "1e2", "1E+2", "2.5e-3", "1e-400"])),
        }
    }

    fn object(&mut self, depth: u64, out: &mut String) {
        out.push('{');
        let keys = self.below(5);
        for i in 0..keys {
            self.space(out);
            let mut key = String::new();
            self.string(&mut key);
            // No key twice, in an order of its own.
            let at = self.below(90);
            key.insert_str(1, &format!("{}{}", at * 7 + i, keys - i));
            out.push_str(&key);
            self.space(out);
            out.push(':');
            self.value(depth, out);
            if i + 1 < keys {
                out.push(',');
            }
        }
        self.space(out);
        out.push('}');
    }

    fn value(&mut self, depth: u64, out: &mut String) {
        self.space(out);
        match self.below(if depth == 0 { 4 } else { 7 }) {
            0 => out.push_str(self.any(&["null", "true", "false"])),
            1 => self.number(out),
            2 | 3 => self.string(out),
            4 => {
                out.push('[');
                let items = self.below(5);
                for i in 0..items {
                    self.value(depth - 1, out);
                    out.push_str(if i + 1 < items { "," } else { "" });
                }
                self.space(out);
                out.push(']');
            }
            _ => self.object(depth - 1, out),
        }
        self.space(out);
    }
}

const PROTOCOLS: [&str; 4] = ["Connect JSON", "Connect binary", "gRPC", "gRPC-Web"];

fn client(protocol: &str, addr: SocketAddr) -> EventServiceClient<HttpClient> {
    let config = ClientConfig::new(format!("http://{addr}").parse().unwrap());
    let http = match protocol {
        "gRPC" => HttpClient::plaintext_http2_only(),
        _ => HttpClient::plaintext(),
    };
    let config = match protocol {
        "Connect JSON" => config.json(),
        "Connect binary" => config,
        "gRPC" => config.with_protocol(Protocol::Grpc),
        "gRPC-Web" => config.with_protocol(Protocol::GrpcWeb),
        _ => unreachable!(),
    };
    EventServiceClient::new(http, config)
}

/// SRV-5: whatever JSON an event holds, sent over each protocol, the engine stores those bytes and
/// each protocol reads them back.
#[tokio::test(flavor = "multi_thread")]
async fn event_data_crosses_every_protocol_byte_for_byte() {
    let dir = tempfile::tempdir().unwrap();
    let opened = Engine::open(dir.path(), clock(), None).await;
    let Opened { engine, minted, .. } = opened.unwrap();
    let admin = minted.unwrap();
    let bearer = || {
        let value = format!("Bearer {}", admin.expose());
        CallOptions::default().with_header("authorization", value)
    };
    let engine = Arc::new(engine);
    let addr = serve(engine.clone()).await;

    let mut json = Json(0x9e37_79b9_7f4a_7c15);
    let documents: Vec<(String, String)> = (0..100)
        .map(|_| {
            let (mut data, mut metadata) = (String::new(), String::new());
            json.value(4, &mut data);
            json.space(&mut metadata);
            json.object(2, &mut metadata);
            json.space(&mut metadata);
            (data, metadata)
        })
        .collect();
    // What a decoder that is not a copy would change is there.
    for held in [
        "\r\n",
        "\\ud83d",
        "\u{2028}",
        "-0",
        "90071992547",
        "184467",
        "e\u{301}",
    ] {
        let holds = |(d, m): &(String, String)| d.contains(held) || m.contains(held);
        assert!(documents.iter().any(holds), "{held:?}");
    }
    for sent_over in PROTOCOLS {
        let items = documents.iter().map(|(data, metadata)| wire::EmitRequest {
            name: "a".into(),
            data: Some(data.clone()),
            metadata: Some(metadata.clone()),
            ..Default::default()
        });
        let items: Vec<_> = items.collect();
        let sender = client(sent_over, addr);
        let first = sender
            .emit_with_options(items[0].clone(), bearer())
            .await
            .unwrap()
            .into_owned();
        let request = wire::EmitBatchRequest {
            items,
            ..Default::default()
        };
        let batch = sender.emit_batch_with_options(request, bearer()).await;
        let batch = ironflowx_proto::EmitBatchResponse::try_from(batch.unwrap().into_owned());
        for (item, (data, metadata)) in batch.unwrap().items.iter().zip(&documents) {
            assert!(item.is_ok(), "{item:?}: {data:?} {metadata:?}");
        }
        let expected = |page: EventPage| {
            // Newest first.
            let got: Vec<_> = page
                .events
                .into_iter()
                .rev()
                .map(|e| {
                    let text = |bytes| String::from_utf8(bytes).unwrap();
                    (text(e.data), text(e.metadata.unwrap()))
                })
                .collect();
            assert_eq!(got, documents, "sent over {sent_over}");
        };
        let cx = key(admin.expose());
        let newest = ListEventsRequest {
            limit: std::num::NonZeroU32::new(100),
            cursor: None,
        };
        expected(engine.list_events(&cx, newest.clone()).await.unwrap());
        let remote = Client::new(&format!("http://{addr}")).unwrap();
        expected(remote.list_events(&cx, newest).await.unwrap());
        for read_over in PROTOCOLS {
            let reader = client(read_over, addr);
            let request = wire::ListEventsRequest {
                limit: Some(100),
                ..Default::default()
            };
            let page = reader.list_events_with_options(request, bearer()).await;
            expected(page.unwrap().into_owned().try_into().unwrap());
            let request = wire::GetEventRequest {
                event_id: first.event_id.clone(),
                ..Default::default()
            };
            let one = reader.get_event_with_options(request, bearer()).await;
            let one = one.unwrap().into_owned();
            let (data, metadata) = &documents[0];
            assert_eq!(one.data.as_ref(), Some(data), "{read_over}");
            assert_eq!(one.metadata.as_ref(), Some(metadata), "{read_over}");
        }
    }
}
