use super::*;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy)]
enum Stop {
    Deadline,
    Request,
    Owner,
}

/// Hold an actual codec charge, then change the original clock/cancellation
/// authority before releasing it. No timing threshold or large input is used.
fn stops_at_held_codec_checkpoint(encode: bool, stop: Stop) {
    let now = Instant::now();
    let deadline = now + Duration::from_secs(1);
    let clock = Arc::new(Mutex::new(now));
    let cancelled = CancellationToken::new();
    let closing = CancellationToken::new();
    let mut control = ReadControl::new(deadline, cancelled.clone(), closing.clone());
    control.clock = Some(clock.clone());
    let count = Arc::new(AtomicUsize::new(0));
    let hook_count = count.clone();
    let held = AtomicBool::new(false);
    let (reached_tx, reached_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let resume_rx = Mutex::new(resume_rx);
    control.codec_checkpoint = Some(Arc::new(move |charge| {
        let count = hook_count.fetch_add(1, Ordering::SeqCst) + 1;
        if matches!(charge, jeto::Charge::Nodes(_)) && !held.swap(true, Ordering::SeqCst) {
            reached_tx.send(count).unwrap();
            resume_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
        }
    }));
    let worker = std::thread::spawn(move || {
        let request = ReadRequest::ById {
            id: Some(HRef::from_val("rich")),
            checked: true,
        };
        let mut meter = control.meter().unwrap();
        if encode {
            request.encode(&control, &mut meter).map(|_| ())
        } else {
            request.decode(200, br#"{"id":{"spec":"sys::Ref","val":"rich"},"list":[{"spec":"sys::Int","val":"9223372036854775807"}]}"#,
                &control, &mut meter).map(|_| ())
        }
    });
    let held_count = reached_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("codec checkpoint reached");
    match stop {
        Stop::Deadline => *clock.lock().unwrap() = deadline,
        Stop::Request => cancelled.cancel(),
        Stop::Owner => closing.cancel(),
    }
    resume_tx.send(()).unwrap();
    let result = worker.join().unwrap();
    match stop {
        Stop::Deadline => assert!(matches!(result, Err(ClientError::Timeout(_))), "{result:?}"),
        Stop::Request | Stop::Owner => assert!(
            matches!(result, Err(ClientError::ConnectionClosed)),
            "{result:?}"
        ),
    }
    // A final-only check can return the right error after doing all the work.
    // This assertion requires interruption at the held charge itself.
    assert_eq!(
        count.load(Ordering::SeqCst),
        held_count,
        "codec continued after stop"
    );
}

#[test]
fn encode_deadline_interrupts_the_held_codec_charge() {
    stops_at_held_codec_checkpoint(true, Stop::Deadline);
}
#[test]
fn decode_deadline_interrupts_the_held_codec_charge() {
    stops_at_held_codec_checkpoint(false, Stop::Deadline);
}
#[test]
fn encode_request_cancel_interrupts_the_held_codec_charge() {
    stops_at_held_codec_checkpoint(true, Stop::Request);
}
#[test]
fn decode_request_cancel_interrupts_the_held_codec_charge() {
    stops_at_held_codec_checkpoint(false, Stop::Request);
}
#[test]
fn encode_owner_close_interrupts_the_held_codec_charge() {
    stops_at_held_codec_checkpoint(true, Stop::Owner);
}
#[test]
fn decode_owner_close_interrupts_the_held_codec_charge() {
    stops_at_held_codec_checkpoint(false, Stop::Owner);
}
