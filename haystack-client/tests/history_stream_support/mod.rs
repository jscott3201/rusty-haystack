#![allow(dead_code)]
use haystack_core::{
    codecs::{Codec, history::*, zinc::ZincCodec},
    graph::GraphState,
    kinds::{HDateTime, Kind},
};

pub fn date(text: &str) -> HDateTime {
    let Kind::DateTime(value) = ZincCodec.decode_scalar(text).unwrap() else {
        panic!()
    };
    value
}
pub fn request() -> HistoryReadRequest {
    HistoryReadRequest {
        id: "point".into(),
        range: "2024-06-01".into(),
    }
}
pub fn result(rows: usize, width: usize) -> HistoryReadResult {
    let start = date("2024-06-01T00:00:00Z UTC");
    let samples: Vec<_> = (0..rows)
        .map(|i| HistorySample {
            ts: date(&format!(
                "2024-06-01T{:02}:{:02}:{:02}Z UTC",
                i / 3600,
                i / 60 % 60,
                i % 60
            )),
            val: Kind::Str(format!("{i:05}:{}", "é,\"\\\n$".repeat(width))),
        })
        .collect();
    HistoryReadResult {
        metadata: HistoryMetadata {
            id: "point".into(),
            requested_range: "2024-06-01".into(),
            evaluated_at: start.clone(),
            start,
            end: date("2024-06-02T00:00:00Z UTC"),
            schema: HistorySchema {
                kind: HistoryKind::Str,
                unit: None,
                timezone: "UTC".into(),
            },
            capabilities: HistoryCapabilities::BOUNDED_LIVE,
            bounds: HistoryBounds {
                batch_rows: 64,
                batch_bytes: 65536,
                total_rows: 10000,
                total_bytes: 16 * 1024 * 1024,
                total_work: 64 * 1024 * 1024,
                response_bytes: 16 * 1024 * 1024,
            },
            graph: GraphState {
                incarnation: [0x11; 16],
                revision: 9007199254740993,
                catalog_generation: 17,
            },
            policy_observation: [0x22; 16],
            history: HistoryState {
                authority: [0x33; 16],
                incarnation: [0x44; 16],
                generation: 9007199254740995,
            },
            coverage: HistoryCoverage {
                retained_start: samples.first().map(|s| s.ts.clone()),
                retained_end: samples.last().map(|s| s.ts.clone()),
                retained_count: rows as u64,
                evicted_through: Some(date("2024-05-31T23:59:59Z UTC")),
            },
        },
        samples,
        terminal: HistoryTerminal::Complete,
    }
}
pub fn encoded(value: &HistoryReadResult) -> Vec<u8> {
    encode_result(value, &ZincCodec).unwrap()
}
pub fn config() -> haystack_client::ClientConfig {
    haystack_client::ClientConfig {
        auth_mode: haystack_client::AuthMode::Basic,
        allow_plaintext_basic: true,
        ..Default::default()
    }
}
pub async fn read_request<S: tokio::io::AsyncRead + Unpin>(stream: &mut S) {
    use tokio::io::AsyncReadExt;
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0];
        stream.read_exact(&mut byte).await.unwrap();
        bytes.push(byte[0]);
        if bytes.ends_with(b"\r\n\r\n") {
            break;
        }
        assert!(bytes.len() < 65536);
    }
    let text = std::str::from_utf8(&bytes).unwrap();
    let len = text
        .lines()
        .find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("content-length: ")
                .map(|n| n.parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    let mut body = vec![0; len];
    stream.read_exact(&mut body).await.unwrap();
}

/// Real HTTP/1.1 server. The response bytes are owned only by the server task;
/// each HTTP chunk can deliberately split a single UTF-8 code point or escape.
pub struct Server {
    pub url: String,
    task: Option<tokio::task::JoinHandle<()>>,
}
impl Drop for Server {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
impl Server {
    pub async fn bytes(bytes: Vec<u8>, chunks: Vec<usize>, extra: &str) -> Self {
        use tokio::{io::AsyncWriteExt, net::TcpListener};
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let extra = extra.to_owned();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            read_request(&mut socket).await;
            if socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/zinc\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n{extra}\r\n").as_bytes()).await.is_err() { return; }
            let mut position = 0;
            let mut index = 0;
            while position < bytes.len() {
                let size = chunks[index % chunks.len()].min(bytes.len() - position);
                if socket
                    .write_all(format!("{size:x}\r\n").as_bytes())
                    .await
                    .is_err()
                {
                    return;
                }
                if socket
                    .write_all(&bytes[position..position + size])
                    .await
                    .is_err()
                {
                    return;
                }
                if socket.write_all(b"\r\n").await.is_err() {
                    return;
                }
                position += size;
                index += 1;
                tokio::task::yield_now().await;
            }
            let _ = socket.write_all(b"0\r\n\r\n").await;
        });
        Self {
            url,
            task: Some(task),
        }
    }
    pub async fn finish(mut self) {
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.task.as_mut().unwrap(),
        )
        .await
        .unwrap()
        .as_ref()
        .unwrap();
        self.task = None;
    }
}
pub async fn client(
    url: &str,
) -> haystack_client::HaystackClient<haystack_client::transport::http::HttpTransport> {
    haystack_client::HaystackClient::connect_with_config(url, "user", "password", &config())
        .await
        .unwrap()
}
pub async fn collect(
    bytes: Vec<u8>,
    chunks: Vec<usize>,
    options: haystack_client::history::HistoryStreamOptions,
) -> Result<
    (
        HistoryReadHeader,
        Vec<HistorySample>,
        haystack_client::history::HistoryStreamEnd,
        haystack_client::history::HistoryStreamStats,
    ),
    haystack_client::history::HistoryStreamError,
> {
    let server = Server::bytes(bytes, chunks, "").await;
    let client = client(&server.url).await;
    let result = async {
        let mut session = client.his_read_stream(&request(), options).await?;
        let header = session.header().clone();
        let mut samples = vec![];
        let end = loop {
            let batch = session.next().await;
            samples.extend(batch.samples);
            if let Some(end) = batch.end {
                break end;
            }
        };
        let stats = session.stats();
        assert_eq!(session.close().await, end);
        Ok((header, samples, end, stats))
    }
    .await;
    server.finish().await;
    result
}
