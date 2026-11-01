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
/// The request a `value` answers.
pub fn request_for(value: &HistoryReadResult) -> HistoryReadRequest {
    HistoryReadRequest {
        id: value.metadata.id.clone(),
        range: value.metadata.requested_range.clone(),
    }
}
/// The header a `value` is served with.
pub fn header(value: &HistoryReadResult) -> HistoryReadHeader {
    HistoryReadHeader {
        metadata: value.metadata.clone(),
        terminal: value.terminal,
        returned_count: value.samples.len() as u64,
    }
}
/// Bytes the manifest spends on `header`: the Zinc envelope as a JSON string,
/// so both escaping layers (control JSON inside a Zinc string, then that Zinc
/// text inside the manifest JSON) are counted.
pub fn header_json_bytes(header: &HistoryReadHeader) -> usize {
    let text = ZincCodec
        .encode_grid(&result_header_grid(header).unwrap())
        .unwrap();
    serde_json::to_string(&text).unwrap().len()
}
/// Every character `str::trim` strips. `validate_range` trims the requested
/// range, so a range may be padded with any of them up to its 1,024 bytes.
/// The HTTP client further refuses non-ASCII and raw CR/LF ranges before
/// dispatch, leaving tab, VT, FF and space for a real session.
pub fn trim_whitespace() -> Vec<char> {
    (0..=u32::from(char::MAX))
        .filter_map(char::from_u32)
        .filter(|c| c.is_whitespace())
        .collect()
}
/// Admitted `rows`-row results whose recorded header is as large as the
/// bounded-history codec allows (but for `returnedCount`, which is `rows`),
/// one per [`trim_whitespace`] padding character. Built by construction
/// rather than by example:
///
/// - the 256-byte id cap; the 1,024-byte range cap spent on padding around
///   `today` (the shortest range the codec resolves), any odd remainder
///   padded with VT, which escapes widest;
/// - the zone and unit whose recorded header is longest, chosen by measuring
///   every admitted candidate (each field is escaped independently, so the
///   choices are additive);
/// - every counter at `u64::MAX` and every bound at its codec cap;
/// - year-9999 timestamps with full nanoseconds, and retained coverage that
///   brackets the window so `retainedCount` is unconstrained.
pub fn worst_case_results(rows: usize) -> Vec<HistoryReadResult> {
    use chrono::{DateTime, FixedOffset, NaiveDate, TimeDelta};
    use haystack_core::kinds::{Number, offset_at, resolve_local_offset, tz_for, tz_map};
    const CORE: &str = "today";
    let at = |zone: &str, instant: DateTime<FixedOffset>| -> Option<HDateTime> {
        Some(HDateTime::new(
            instant.with_timezone(&offset_at(zone, instant)?),
            zone,
        ))
    };
    let build = |zone: &str, unit: &str, range: String| -> Option<HistoryReadResult> {
        let midnight = |day| {
            let local = NaiveDate::from_ymd_opt(9999, 12, day)?.and_hms_opt(0, 0, 0)?;
            let offset = resolve_local_offset(zone, local)?.single()?;
            local.and_local_timezone(offset).single()
        };
        let (start, end) = (midnight(30)?, midnight(31)?);
        let ns = TimeDelta::nanoseconds;
        let mut value = result(rows, 0);
        let m = &mut value.metadata;
        m.id = "p".repeat(256);
        m.requested_range = range;
        m.start = at(zone, start)?;
        m.end = at(zone, end)?;
        m.evaluated_at = at(zone, end - ns(1))?;
        m.schema.kind = HistoryKind::Number;
        m.schema.unit = Some(unit.into());
        m.schema.timezone = zone.into();
        m.bounds = HistoryBounds {
            batch_rows: MAX_ROWS as u64,
            batch_bytes: MAX_GRID_BYTES as u64,
            total_rows: MAX_ROWS as u64,
            total_bytes: MAX_GRID_BYTES as u64,
            total_work: 256 * 1024 * 1024,
            response_bytes: MAX_GRID_BYTES as u64,
        };
        m.graph.revision = u64::MAX;
        m.graph.catalog_generation = u64::MAX;
        m.history.generation = u64::MAX;
        m.coverage = HistoryCoverage {
            retained_start: Some(at(zone, start - ns(1))?),
            retained_end: Some(at(zone, end + ns(999_999_999))?),
            retained_count: u64::MAX,
            evicted_through: Some(at(zone, start - ns(2))?),
        };
        for (i, sample) in value.samples.iter_mut().enumerate() {
            sample.ts = at(zone, start + TimeDelta::seconds(i as i64))?;
            sample.val = Kind::Number(Number::new(f64::MAX, Some(unit.into())));
        }
        validate_for_request(&value, &request_for(&value)).ok()?;
        Some(value)
    };
    let longest = |candidates: Vec<String>, make: &dyn Fn(&str) -> Option<HistoryReadResult>| {
        candidates
            .iter()
            .filter_map(|c| Some((header_json_bytes(&header(&make(c)?)), c.clone())))
            .max()
            .unwrap()
            .1
    };
    let zones = tz_map()
        .keys()
        .filter(|z| z.len() <= 128 && tz_for(z).is_some())
        .cloned()
        .collect();
    let zone = longest(zones, &|z| build(z, "%", CORE.into()));
    let units = haystack_core::kinds::units_by_name()
        .keys()
        .chain(haystack_core::kinds::units_by_symbol().keys())
        .filter(|u| u.len() <= 128)
        .cloned()
        .collect();
    let unit = longest(units, &|u| build(&zone, u, CORE.into()));
    trim_whitespace()
        .into_iter()
        .map(|pad| {
            let room = 1024 - CORE.len();
            let mut range = pad.to_string().repeat(room / pad.len_utf8());
            range.push_str(CORE);
            range.push_str(&"\u{0b}".repeat(room % pad.len_utf8()));
            build(&zone, &unit, range).unwrap()
        })
        .collect()
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
/// `HistorySample ==` compares `HDateTime` instants only, so a lost offset or
/// zone name would still compare equal. Check those components explicitly.
pub fn assert_same_samples(actual: &[HistorySample], expected: &[HistorySample]) {
    assert_eq!(actual.len(), expected.len());
    assert_eq!(actual, expected);
    for (index, (a, b)) in actual.iter().zip(expected).enumerate() {
        assert_eq!(a.ts.dt.timestamp(), b.ts.dt.timestamp(), "row {index}");
        assert_eq!(
            a.ts.dt.timestamp_subsec_nanos(),
            b.ts.dt.timestamp_subsec_nanos(),
            "row {index}"
        );
        assert_eq!(
            a.ts.dt.offset().local_minus_utc(),
            b.ts.dt.offset().local_minus_utc(),
            "row {index}"
        );
        assert_eq!(a.ts.tz_name, b.ts.tz_name, "row {index}");
    }
}
