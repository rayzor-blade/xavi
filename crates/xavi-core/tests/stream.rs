use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll, Wake, Waker};

use xavi_core::stream::{Limits, Read, channel};
use xavi_core::{Error, ErrorKind};

#[derive(Default)]
struct CountWake(AtomicUsize);
impl Wake for CountWake {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn waker() -> (Arc<CountWake>, Waker) {
    let counter = Arc::new(CountWake::default());
    (counter.clone(), Waker::from(counter))
}

#[test]
fn backpressure_preserves_unsent_data_and_eof_is_not_pending() {
    let (mut tx, mut rx) = channel(Limits {
        max_items: 2,
        max_bytes: 3,
    })
    .unwrap();
    assert!(matches!(rx.try_next().unwrap(), Read::Pending));
    tx.try_send(vec![1, 2]).unwrap();
    let full = tx.try_send(vec![3, 4]).unwrap_err();
    assert_eq!(full.error.kind, ErrorKind::WouldBlock);
    assert_eq!(full.value, [3, 4]);
    assert!(matches!(rx.try_next().unwrap(), Read::Item(v) if v == [1, 2]));
    tx.try_send(full.value).unwrap();
    tx.finish().unwrap();
    assert!(matches!(rx.try_next().unwrap(), Read::Item(v) if v == [3, 4]));
    assert!(matches!(rx.try_next().unwrap(), Read::End));
    assert!(matches!(rx.try_next().unwrap(), Read::End));
    assert_eq!(
        tx.try_send(vec![5]).unwrap_err().error.kind,
        ErrorKind::InvalidState
    );
}

#[test]
fn async_producer_is_woken_when_space_is_available() {
    let (mut tx, mut rx) = channel(Limits {
        max_items: 1,
        max_bytes: 4,
    })
    .unwrap();
    tx.try_send(vec![1]).unwrap();
    let (wake_count, waker) = waker();
    let mut cx = Context::from_waker(&waker);
    let mut sending = Box::pin(tx.send(vec![2]));
    assert!(sending.as_mut().poll(&mut cx).is_pending());
    assert!(matches!(rx.try_next().unwrap(), Read::Item(v) if v == [1]));
    assert_eq!(wake_count.0.load(Ordering::SeqCst), 1);
    assert!(matches!(
        sending.as_mut().poll(&mut cx),
        Poll::Ready(Ok(()))
    ));
    drop(sending);
    drop(tx);
    assert!(matches!(rx.try_next().unwrap(), Read::Item(v) if v == [2]));
    assert!(matches!(rx.try_next().unwrap(), Read::End));
}

#[test]
fn async_consumer_is_woken_by_input_and_sender_shutdown() {
    let (mut tx, mut rx) = channel(Limits {
        max_items: 1,
        max_bytes: 4,
    })
    .unwrap();
    let (wake_count, waker) = waker();
    let mut cx = Context::from_waker(&waker);
    assert!(rx.poll_next(&mut cx).is_pending());
    tx.try_send(vec![9]).unwrap();
    assert_eq!(wake_count.0.load(Ordering::SeqCst), 1);
    assert!(matches!(rx.poll_next(&mut cx), Poll::Ready(Some(Ok(v))) if v == [9]));
    assert!(rx.poll_next(&mut cx).is_pending());
    drop(tx);
    assert_eq!(wake_count.0.load(Ordering::SeqCst), 2);
    assert!(matches!(rx.poll_next(&mut cx), Poll::Ready(None)));
}

#[test]
fn cancellation_wakes_a_blocked_producer_and_releases_queued_resources() {
    let (mut tx, rx) = channel(Limits {
        max_items: 1,
        max_bytes: 4,
    })
    .unwrap();
    let bytes: Arc<[u8]> = Arc::from([1, 2]);
    tx.try_send(bytes.clone()).unwrap();
    let (wake_count, waker) = waker();
    let mut cx = Context::from_waker(&waker);
    let mut sending = Box::pin(tx.send(Arc::<[u8]>::from([3])));
    assert!(sending.as_mut().poll(&mut cx).is_pending());
    drop(rx);
    assert_eq!(Arc::strong_count(&bytes), 1);
    assert_eq!(wake_count.0.load(Ordering::SeqCst), 1);
    let Poll::Ready(Err(error)) = sending.as_mut().poll(&mut cx) else {
        panic!("send must fail after cancellation")
    };
    assert_eq!(error.error.kind, ErrorKind::Cancelled);
    assert_eq!(&*error.value, &[3]);
}

#[test]
fn failure_discards_output_and_delivers_one_error_then_eof() {
    let (mut tx, mut rx) = channel(Limits {
        max_items: 2,
        max_bytes: 4,
    })
    .unwrap();
    tx.try_send(vec![1]).unwrap();
    let error = Error::unsupported("source protocol failed");
    tx.fail(error.clone()).unwrap();
    assert_eq!(rx.try_next().unwrap_err(), error);
    assert!(matches!(rx.try_next().unwrap(), Read::End));
    assert_eq!(tx.try_send(vec![]).unwrap_err().error, error);
}

#[test]
fn limits_reject_oversized_items_and_bound_empty_chunks() {
    assert!(
        channel::<Vec<u8>>(Limits {
            max_items: 0,
            max_bytes: 1
        })
        .is_err()
    );
    assert!(
        channel::<Vec<u8>>(Limits {
            max_items: 1,
            max_bytes: 0
        })
        .is_err()
    );
    let (mut tx, mut rx) = channel(Limits {
        max_items: 1,
        max_bytes: 2,
    })
    .unwrap();
    assert_eq!(
        tx.try_send(vec![0; 3]).unwrap_err().error.kind,
        ErrorKind::InvalidArgument
    );
    tx.try_send(vec![]).unwrap();
    assert_eq!(
        tx.try_send(vec![]).unwrap_err().error.kind,
        ErrorKind::WouldBlock
    );
    assert!(matches!(rx.try_next().unwrap(), Read::Item(v) if v.is_empty()));
}

#[test]
fn eof_is_permanent_even_if_an_endpoint_later_cancels_or_fails() {
    let (mut tx, mut rx) = channel::<Vec<u8>>(Limits {
        max_items: 1,
        max_bytes: 1,
    })
    .unwrap();
    tx.finish().unwrap();
    assert!(matches!(rx.try_next().unwrap(), Read::End));
    tx.fail(Error::unsupported("late failure")).unwrap();
    rx.cancel().unwrap();
    assert!(matches!(rx.try_next().unwrap(), Read::End));
}

#[test]
fn cancelling_a_pending_send_does_not_enqueue_it_later() {
    let (mut tx, mut rx) = channel(Limits {
        max_items: 1,
        max_bytes: 2,
    })
    .unwrap();
    tx.try_send(vec![1]).unwrap();
    let (_, waker) = waker();
    let mut cx = Context::from_waker(&waker);
    let mut sending = Box::pin(tx.send(vec![2]));
    assert!(sending.as_mut().poll(&mut cx).is_pending());
    drop(sending);
    assert!(matches!(rx.try_next().unwrap(), Read::Item(v) if v == [1]));
    assert!(matches!(rx.try_next().unwrap(), Read::Pending));
    tx.try_send(vec![3]).unwrap();
    assert!(matches!(rx.try_next().unwrap(), Read::Item(v) if v == [3]));
}
