mod history_stream_support;
use haystack_client::HaystackClient;
use history_stream_support::*;
use std::time::Duration;
use tokio::{io::AsyncWriteExt, net::TcpListener, sync::oneshot};

#[tokio::test]
async fn first_batch_is_available_before_remote_tail_is_sent() {
    let expected = result(4, 4);
    let bytes = encoded(&expected);
    let split = bytes
        .iter()
        .enumerate()
        .filter(|(_, c)| **c == b'\n')
        .nth(3)
        .unwrap()
        .0
        + 1;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (release, wait) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_request(&mut socket).await;
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/zinc\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",bytes.len()).as_bytes()).await.unwrap();
        socket.write_all(&bytes[..split]).await.unwrap();
        let _ = wait.await;
        let _ = socket.write_all(&bytes[split..]).await;
    });
    let client = HaystackClient::connect_with_config(&url, "user", "password", &config())
        .await
        .unwrap();
    let first = tokio::time::timeout(Duration::from_millis(200), async {
        let options = haystack_client::history::HistoryStreamOptions {
            limits: haystack_client::history::HistoryStreamLimits {
                batch_rows: 2,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut session = client.his_read_stream(&request(), options).await.unwrap();
        assert_eq!(session.header().metadata, expected.metadata);
        let batch = session.next().await;
        assert_eq!(batch.samples, expected.samples[..2]);
        assert!(batch.end.is_none());
        session
    })
    .await;
    let _ = release.send(());
    server.await.unwrap();
    let mut session =
        first.expect("a remote prefix must be consumable before the peer sends the tail");
    assert_eq!(session.next().await.samples, expected.samples[2..]);
    assert_eq!(
        session.next().await.end,
        Some(haystack_client::history::HistoryStreamEnd::Remote(
            expected.terminal
        ))
    );
    session.close().await;
}

use haystack_client::history::{
    HistoryStreamEnd, HistoryStreamError, HistoryStreamLimit, HistoryStreamLimits,
    HistoryStreamOptions,
};
use haystack_core::{
    codecs::{Codec, history::*, typed, zinc::ZincCodec},
    kinds::{Kind, Number},
};

#[tokio::test]
async fn chunks_and_remote_terminals_preserve_independent_metadata_and_scalars() {
    for chunks in [vec![1], vec![2, 3, 7, 1, 31, 5, 129], vec![65536]] {
        for terminal in [
            HistoryTerminal::Complete,
            HistoryTerminal::Limited(HistoryReason::Rows),
            HistoryTerminal::Interrupted(HistoryReason::GenerationChanged),
            HistoryTerminal::Failed(HistoryReason::Provider),
        ] {
            let mut expected = result(7, 3);
            expected.terminal = terminal;
            expected.samples[2].val = Kind::NA;
            let (header, actual, end, _) = collect(
                encoded(&expected),
                chunks.clone(),
                HistoryStreamOptions {
                    limits: HistoryStreamLimits {
                        batch_rows: 2,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            assert_eq!(header.metadata, expected.metadata);
            assert_eq!(header.terminal, terminal);
            assert_eq!(header.returned_count, 7);
            assert_eq!(actual, expected.samples);
            assert_eq!(end, HistoryStreamEnd::Remote(terminal));
        }
    }
}
#[tokio::test]
async fn independent_zinc_numeric_bool_and_minute_leap_rows_are_exact() {
    // Rows below are literal wire inputs, independently expected from the
    // documented H4 representation; the product encoder only builds metadata.
    for (kind, unit, rows, values) in [
        (
            HistoryKind::Number,
            Some("°F"),
            vec![
                "-0fahrenheit",
                "INF°F",
                "-INF",
                "NaN",
                "NA",
                "1.0000000000000002°F",
                "1.2e-3°F",
            ],
            vec![
                Kind::Number(Number::new(-0.0, Some("fahrenheit".into()))),
                Kind::Number(Number::new(f64::INFINITY, Some("°F".into()))),
                Kind::Number(Number::unitless(f64::NEG_INFINITY)),
                Kind::Number(Number::unitless(f64::NAN)),
                Kind::NA,
                Kind::Number(Number::new(1.0000000000000002, Some("°F".into()))),
                Kind::Number(Number::new(0.0012, Some("°F".into()))),
            ],
        ),
        (
            HistoryKind::Bool,
            None,
            vec!["T", "F", "NA"],
            vec![Kind::Bool(true), Kind::Bool(false), Kind::NA],
        ),
    ] {
        let mut expected = result(rows.len(), 0);
        expected.metadata.schema.kind = kind;
        expected.metadata.schema.unit = unit.map(str::to_owned);
        for (sample, value) in expected.samples.iter_mut().zip(&values) {
            sample.val = value.clone();
        }
        let encoded = encoded(&expected);
        let text = std::str::from_utf8(&encoded).unwrap();
        let mut lines = text.lines();
        let mut wire = format!("{}\n{}\n", lines.next().unwrap(), lines.next().unwrap());
        for (i, value) in rows.iter().enumerate() {
            wire.push_str(&format!("2024-06-01T00:00:{i:02}Z UTC,{value}\n"));
        }
        let (_, actual, end, _) = collect(wire.into_bytes(), vec![1], Default::default())
            .await
            .unwrap();
        assert_eq!(end, HistoryStreamEnd::Remote(HistoryTerminal::Complete));
        for (sample, value) in actual.iter().zip(values) {
            match (&sample.val, value) {
                (Kind::Number(a), Kind::Number(b)) => {
                    assert_eq!(a.val.to_bits(), b.val.to_bits());
                    assert_eq!(a.unit, b.unit);
                }
                (a, b) => assert_eq!(*a, b),
            }
        }
    }
    let mut expected = result(3, 0);
    expected.metadata.requested_range = "2016-12-31".into();
    expected.metadata.start = date("2016-12-31T00:00:00Z UTC");
    expected.metadata.end = date("2017-01-01T00:00:00Z UTC");
    expected.metadata.evaluated_at = expected.metadata.start.clone();
    for (sample, ts) in expected.samples.iter_mut().zip([
        "2016-12-31T23:59:59.999999999Z UTC",
        "2016-12-31T23:59:60.000000001Z UTC",
        "2016-12-31T23:59:60.999999999Z UTC",
    ]) {
        sample.ts = date(ts);
    }
    expected.metadata.coverage.retained_start = Some(expected.samples[0].ts.clone());
    expected.metadata.coverage.retained_end = Some(expected.samples[2].ts.clone());
    expected.metadata.coverage.evicted_through = None;
    let server = Server::bytes(encoded(&expected), vec![1], "").await;
    let client = client(&server.url).await;
    let req = HistoryReadRequest {
        id: "point".into(),
        range: "2016-12-31".into(),
    };
    let mut session = client
        .his_read_stream(&req, Default::default())
        .await
        .unwrap();
    let batch = session.next().await;
    assert_eq!(batch.samples, expected.samples);
    assert_eq!(
        batch.end,
        Some(HistoryStreamEnd::Remote(HistoryTerminal::Complete))
    );
    session.close().await;
    server.finish().await;
}
fn mutate_control(bytes: &[u8], change: impl FnOnce(&mut haystack_core::data::HDict)) -> Vec<u8> {
    let mut grid = ZincCodec
        .decode_grid(std::str::from_utf8(bytes).unwrap())
        .unwrap();
    let Some(Kind::Str(control)) = grid.meta.get("history") else {
        panic!()
    };
    let Kind::Dict(mut control) = typed::decode(control.as_bytes()).unwrap() else {
        panic!()
    };
    change(&mut control);
    grid.meta.set(
        "history",
        Kind::Str(String::from_utf8(typed::encode(&Kind::Dict(control)).unwrap()).unwrap()),
    );
    ZincCodec.encode_grid(&grid).unwrap().into_bytes()
}
#[tokio::test]
async fn late_protocol_failures_never_turn_prefixes_into_complete() {
    let original = encoded(&result(4, 0));
    let text = String::from_utf8(original.clone()).unwrap();
    let lines: Vec<_> = text.lines().collect();
    let prefix = format!("{}\n{}\n{}\n", lines[0], lines[1], lines[2]);
    let mut cases = vec![
        prefix.as_bytes().to_vec(),
        format!("{prefix}{}", lines[3]).into_bytes(),
        format!("{prefix}{}\n{}\n{}\n", lines[3], lines[3], lines[5]).into_bytes(),
        format!("{prefix}{}\n{}\n{}\n", lines[4], lines[3], lines[5]).into_bytes(),
        format!("{prefix}2024-06-02T00:00:00Z UTC,\"bad\"\n").into_bytes(),
        format!("{text}garbage\n").into_bytes(),
        format!("{text}2024-06-01T00:00:05Z UTC,\"extra\"\n").into_bytes(),
        format!("{prefix}2024-06-01T00:00:01Z UTC,\"unterminated\n").into_bytes(),
        format!("{prefix}2024-06-01T00:00:01Z UTC,{{evil:[[[1]]]}}\n").into_bytes(),
        format!("{prefix}2024-06-01T00:00:01Z UTC,\"\\uD800\"\n").into_bytes(),
        format!("{prefix}2024-06-01T00:00:01Z UTC,\"x\",\"surplus\"\n").into_bytes(),
    ];
    cases.push(mutate_control(&original, |c| {
        c.set("returnedCount", Kind::Str("3".into()))
    }));
    cases.push(mutate_control(&original, |c| {
        c.set("retainedCount", Kind::Str("5".into()))
    }));
    for (index, bytes) in cases.into_iter().enumerate() {
        let (_, samples, end, _) = collect(
            bytes,
            vec![7, 1, 3],
            HistoryStreamOptions {
                limits: HistoryStreamLimits {
                    batch_rows: 1,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(
            samples.first(),
            Some(&result(4, 0).samples[0]),
            "case {index}"
        );
        assert_eq!(
            end,
            HistoryStreamEnd::Local(HistoryStreamError::Protocol),
            "case {index}"
        );
    }
}
#[tokio::test]
async fn header_identity_format_and_structure_fail_before_sample_delivery() {
    let original = encoded(&result(4, 0));
    let text = String::from_utf8(original.clone()).unwrap();
    for bytes in [
        text.replacen("ver:\"3.0\"", "ver:\"2.0\"", 1).into_bytes(),
        text.replacen("ts,val", "ts,val,other", 1).into_bytes(),
        text.replacen(" id:@point", " id:@foreign", 1).into_bytes(),
        text.replacen("\nts,val", " id:@point\nts,val", 1)
            .into_bytes(),
        text.replacen(" history:", " mystery:", 1).into_bytes(),
        mutate_control(&original, |c| {
            c.set("requestedRange", Kind::Str("today".into()))
        }),
        mutate_control(&original, |c| {
            c.set("returnedCount", Kind::Str("10001".into()))
        }),
    ] {
        assert!(matches!(
            collect(bytes, vec![1, 17], Default::default()).await,
            Err(HistoryStreamError::Protocol)
        ));
    }
}
#[tokio::test]
async fn local_limits_stop_bounded_growth_with_typed_partial_outcomes() {
    let original = encoded(&result(10, 20));
    for (limits, expected) in [
        (
            HistoryStreamLimits {
                header_bytes: 100,
                ..Default::default()
            },
            HistoryStreamLimit::HeaderBytes,
        ),
        (
            HistoryStreamLimits {
                row_bytes: 80,
                field_bytes: 80,
                ..Default::default()
            },
            HistoryStreamLimit::RowBytes,
        ),
        (
            HistoryStreamLimits {
                field_bytes: 10,
                ..Default::default()
            },
            HistoryStreamLimit::FieldBytes,
        ),
        (
            HistoryStreamLimits {
                batch_bytes: 600,
                ..Default::default()
            },
            HistoryStreamLimit::BatchBytes,
        ),
        (
            HistoryStreamLimits {
                rows: 2,
                batch_rows: 1,
                ..Default::default()
            },
            HistoryStreamLimit::Rows,
        ),
        (
            HistoryStreamLimits {
                source_bytes: 1000,
                batch_bytes: 1000,
                ..Default::default()
            },
            HistoryStreamLimit::SourceBytes,
        ),
        (
            HistoryStreamLimits {
                response_bytes: 1000,
                ..Default::default()
            },
            HistoryStreamLimit::ResponseBytes,
        ),
        (
            HistoryStreamLimits {
                work: 1000,
                ..Default::default()
            },
            HistoryStreamLimit::Work,
        ),
        (
            HistoryStreamLimits {
                frame_bytes: 1,
                ..Default::default()
            },
            HistoryStreamLimit::FrameBytes,
        ),
    ] {
        let outcome = collect(
            original.clone(),
            vec![5],
            HistoryStreamOptions {
                limits,
                ..Default::default()
            },
        )
        .await;
        let error = match outcome {
            Err(error) => error,
            Ok((_, _, HistoryStreamEnd::Local(error), _)) => error,
            Ok(other) => panic!("unexpected success: {other:?}"),
        };
        assert_eq!(error, HistoryStreamError::Limit(expected));
    }
}

#[tokio::test]
async fn escaped_linefeed_is_distinct_from_literal_escape_and_malformed_grammar_is_rejected() {
    let mut expected = result(2, 0);
    expected.samples[0].val = Kind::Str("a\nb".into());
    expected.samples[1].val = Kind::Str("a\\nb".into());
    let (_, actual, end, _) = collect(encoded(&expected), vec![1], Default::default())
        .await
        .unwrap();
    assert_eq!(actual, expected.samples);
    assert_eq!(end, HistoryStreamEnd::Remote(HistoryTerminal::Complete));
    let original = String::from_utf8(encoded(&result(2, 0))).unwrap();
    let lines: Vec<_> = original.lines().collect();
    let head = format!("{}\n{}\n", lines[0], lines[1]);
    for bad in [
        "\"a\nb\"",
        "\"\\u雪雪\"",
        "NA,",
        "T junk",
        "N",
        "",
        "[NA]",
        "{x:NA}",
        "NaN°F",
    ] {
        let bytes = format!("{head}2024-06-01T00:00:00Z UTC,{bad}\n").into_bytes();
        let (_, _, end, _) = collect(bytes, vec![1], Default::default()).await.unwrap();
        assert_eq!(
            end,
            HistoryStreamEnd::Local(HistoryStreamError::Protocol),
            "{bad}"
        );
    }
    for columns in ["ts,,val", "val,ts", "ts x:M,val", "ts,val,"] {
        assert!(matches!(
            collect(
                original.replacen("ts,val", columns, 1).into_bytes(),
                vec![17],
                Default::default()
            )
            .await,
            Err(HistoryStreamError::Protocol)
        ));
    }
    let duplicate = original.replacen("\nts,val", " history:\"duplicate\"\nts,val", 1);
    assert!(matches!(
        collect(duplicate.into_bytes(), vec![17], Default::default()).await,
        Err(HistoryStreamError::Protocol)
    ));
}

// Literal hostile inputs, independent of the product encoder. Exercise each
// calendar/time/seconds/offset region that precedes generic scalar decoding.
const MALFORMED_UNICODE_DATETIMES: [&str; 7] = [
    "2024-雪-01T00:00:02Z UTC",
    "2024-06-01T雪雪Z UTC",
    "2024-06-01T00:雪雪Z UTC",
    "2024-06-01T00:00:雪雪Z UTC",
    "2024-06-01T00:00:02.雪Z UTC",
    "2024-06-01T00:00:02+雪雪 UTC",
    "2024-06-01T00:00:02+00:雪雪 UTC",
];

fn literal_chunks(bytes: &[u8], literal: &str, split: bool) -> Vec<usize> {
    if !split {
        return vec![65536];
    }
    let start = bytes
        .windows(literal.len())
        .position(|window| window == literal.as_bytes())
        .expect("fixture must contain the literal to split");
    let mut chunks = vec![start];
    // Deliberately split every byte of the hostile field, including each UTF-8
    // code point. Valid surrounding metadata/prefix rows need no fragmentation
    // in these cases and otherwise add thousands of unrelated fixture writes.
    chunks.extend(std::iter::repeat_n(1, literal.len()));
    chunks.push(bytes.len() - start - literal.len());
    assert!(chunks.iter().all(|size| *size > 0));
    eprintln!(
        "targeted literal chunks: before={} after={} literal_bytes={}",
        bytes.len(),
        chunks.len(),
        literal.len()
    );
    chunks
}

#[tokio::test]
async fn malformed_unicode_header_datetimes_return_protocol_without_unwinding() {
    let original = String::from_utf8(encoded(&result(3, 0))).unwrap();
    for split in [false, true] {
        for (field, valid) in [
            ("hisStart", "2024-06-01T00:00:00+00:00 UTC"),
            ("hisEnd", "2024-06-02T00:00:00+00:00 UTC"),
        ] {
            for malformed in MALFORMED_UNICODE_DATETIMES {
                let bytes = original.replacen(
                    &format!("{field}:{valid}"),
                    &format!("{field}:{malformed}"),
                    1,
                );
                assert_ne!(bytes, original, "fixture must replace {field}");
                let chunks = literal_chunks(bytes.as_bytes(), malformed, split);
                assert!(
                    matches!(
                        collect(bytes.into_bytes(), chunks.clone(), Default::default()).await,
                        Err(HistoryStreamError::Protocol)
                    ),
                    "{field}: {malformed}, chunks {chunks:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn malformed_unicode_request_ranges_fail_before_dispatch_and_binding() {
    for range in [
        "2024-06-01T雪雪Z UTC",
        "2024-06-01T00:00:雪雪Z UTC,2024-06-02T00:00:00Z UTC",
        "2024-06-01T00:00:00Z UTC,2024-06-02T00:00:00+00:雪雪 UTC",
    ] {
        let mut expected = result(3, 0);
        expected.metadata.requested_range = range.into();
        let bytes = encoded(&expected);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (stop, stopped) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = tokio::select! {
                connection = listener.accept() => connection.unwrap(),
                _ = stopped => return false,
            };
            read_request(&mut socket).await;
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/zinc\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len()).as_bytes()).await.unwrap();
            socket.write_all(&bytes).await.unwrap();
            true
        });
        let client = client(&url).await;
        let mut request = request();
        request.range = range.into();
        // A separate task lets this regression retain evidence of an opening
        // panic while still stopping and joining the optional response fixture.
        let outcome =
            tokio::spawn(async move { client.his_read_stream(&request, Default::default()).await })
                .await;
        let _ = stop.send(());
        let dispatched = server.await.unwrap();
        assert!(
            matches!(outcome, Ok(Err(HistoryStreamError::Protocol))),
            "malformed range must return Protocol without unwinding: {range}"
        );
        assert!(!dispatched, "malformed range must not dispatch: {range}");
    }
}

#[tokio::test]
async fn malformed_unicode_sample_datetimes_preserve_the_current_batch_prefix() {
    let expected = result(3, 0);
    let original = String::from_utf8(encoded(&expected)).unwrap();
    let lines: Vec<_> = original.lines().collect();
    let prefix = format!("{}\n{}\n{}\n{}\n", lines[0], lines[1], lines[2], lines[3]);
    for split in [false, true] {
        for malformed in MALFORMED_UNICODE_DATETIMES {
            let bytes = format!("{prefix}{malformed},\"bad\"\n").into_bytes();
            let chunks = literal_chunks(&bytes, malformed, split);
            let (_, samples, end, stats) = collect(bytes, chunks.clone(), Default::default())
                .await
                .unwrap();
            assert_eq!(
                end,
                HistoryStreamEnd::Local(HistoryStreamError::Protocol),
                "{malformed}, chunks {chunks:?}"
            );
            assert_eq!(samples, expected.samples[..2]);
            assert_eq!(stats.admitted_rows, 2);
        }
    }
}

#[tokio::test]
async fn malformed_unicode_temporal_values_preserve_the_current_batch_prefix() {
    let expected = result(3, 0);
    let original = String::from_utf8(encoded(&expected)).unwrap();
    let lines: Vec<_> = original.lines().collect();
    let prefix = format!("{}\n{}\n{}\n{}\n", lines[0], lines[1], lines[2], lines[3]);
    for split in [false, true] {
        // Even a value of the wrong kind must fail as a protocol error before
        // any unchecked DateTime/Time parser path can lose the admitted rows.
        for malformed in MALFORMED_UNICODE_DATETIMES
            .into_iter()
            .chain(["00:00:雪雪"])
        {
            let bytes = format!("{prefix}2024-06-01T00:00:02Z UTC,{malformed}\n").into_bytes();
            let chunks = literal_chunks(&bytes, malformed, split);
            let (_, samples, end, stats) = collect(bytes, chunks.clone(), Default::default())
                .await
                .unwrap();
            assert_eq!(
                end,
                HistoryStreamEnd::Local(HistoryStreamError::Protocol),
                "{malformed}, chunks {chunks:?}"
            );
            assert_eq!(samples, expected.samples[..2]);
            assert_eq!(stats.admitted_rows, 2);
        }
    }
}

#[tokio::test]
async fn raw_carriage_return_cells_fail_with_the_current_batch_prefix() {
    let expected = result(3, 0);
    let original = String::from_utf8(encoded(&expected)).unwrap();
    let lines: Vec<_> = original.lines().collect();
    let prefix = format!("{}\n{}\n{}\n{}\n", lines[0], lines[1], lines[2], lines[3]);
    for split in [false, true] {
        let bytes = format!("{prefix}2024-06-01T00:00:02Z UTC,\"a\rb\"\n").into_bytes();
        let chunks = literal_chunks(&bytes, "\"a\rb\"", split);
        let (_, samples, end, stats) = collect(bytes, chunks, Default::default()).await.unwrap();
        assert_eq!(end, HistoryStreamEnd::Local(HistoryStreamError::Protocol));
        assert_eq!(samples, expected.samples[..2]);
        assert_eq!(stats.admitted_rows, 2);
    }
}

#[tokio::test]
async fn escaped_carriage_return_and_crlf_records_keep_exact_values() {
    let mut expected = result(3, 0);
    expected.samples[0].val = Kind::Str("a\rb".into());
    expected.samples[1].val = Kind::Str("a\\rb".into());
    expected.samples[2].val = Kind::Str("雪\r\nb".into());
    let original = String::from_utf8(encoded(&expected)).unwrap();
    let lines: Vec<_> = original.lines().collect();
    // Build scalar rows literally; the encoder supplies only the envelope.
    let bytes = format!(
        "{}\r\n{}\r\n2024-06-01T00:00:00Z UTC,\"a\\rb\"\r\n\
         2024-06-01T00:00:01Z UTC,\"a\\\\rb\"\r\n\
         2024-06-01T00:00:02Z UTC,\"雪\\r\\nb\"\r\n",
        lines[0], lines[1]
    )
    .into_bytes();
    for chunks in [vec![65536], vec![1]] {
        let (_, samples, end, _) = collect(bytes.clone(), chunks, Default::default())
            .await
            .unwrap();
        assert_eq!(samples, expected.samples);
        assert_eq!(end, HistoryStreamEnd::Remote(HistoryTerminal::Complete));
    }
}

#[tokio::test]
async fn dst_fallback_orders_instants_and_exact_count_does_not_prove_complete_coverage() {
    let mut expected = result(2, 0);
    expected.metadata.requested_range = "2024-11-03".into();
    expected.metadata.schema.timezone = "New_York".into();
    expected.metadata.start = date("2024-11-03T00:00:00-04:00 New_York");
    expected.metadata.end = date("2024-11-04T00:00:00-05:00 New_York");
    expected.metadata.evaluated_at = expected.metadata.start.clone();
    expected.samples[0].ts = date("2024-11-03T01:45:00-04:00 New_York");
    expected.samples[1].ts = date("2024-11-03T01:15:00-05:00 New_York");
    expected.metadata.coverage.retained_start = Some(expected.samples[0].ts.clone());
    expected.metadata.coverage.retained_end = Some(expected.samples[1].ts.clone());
    expected.metadata.coverage.evicted_through = None;
    let server = Server::bytes(encoded(&expected), vec![1], "").await;
    let client = client(&server.url).await;
    let req = HistoryReadRequest {
        id: "point".into(),
        range: "2024-11-03".into(),
    };
    let mut session = client
        .his_read_stream(&req, Default::default())
        .await
        .unwrap();
    let batch = session.next().await;
    assert_eq!(batch.samples, expected.samples);
    assert_eq!(
        batch.end,
        Some(HistoryStreamEnd::Remote(HistoryTerminal::Complete))
    );
    session.close().await;
    server.finish().await;
    let original = encoded(&result(3, 0));
    let changed = mutate_control(&original, |c| c.set("returnedCount", Kind::Str("2".into())));
    let text = String::from_utf8(changed).unwrap();
    let lines: Vec<_> = text.lines().collect();
    let missing_middle = format!("{}\n{}\n{}\n{}\n", lines[0], lines[1], lines[2], lines[4]);
    let (_, samples, end, _) = collect(missing_middle.into_bytes(), vec![1], Default::default())
        .await
        .unwrap();
    assert_eq!(samples.len(), 2);
    assert_eq!(end, HistoryStreamEnd::Local(HistoryStreamError::Protocol));
}

#[tokio::test]
async fn selected_zinc_header_rejects_native_only_timestamp_projection() {
    let native=typed::decode(br#"{"version":1,"value":{"kind":"dateTime","seconds":"253402300800","nanos":0,"offset":0,"timezone":"UTC"}}"#).unwrap();
    let bytes = mutate_control(&encoded(&result(2, 0)), |c| c.set("evaluatedAt", native));
    assert!(matches!(
        collect(bytes, vec![1, 19], Default::default()).await,
        Err(HistoryStreamError::Protocol)
    ));
}
