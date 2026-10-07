//! Bounded streaming for live frames, encoded chunks and non-seekable byte input.
//!
//! A channel has one producer and one consumer. `try_send` / `try_next` never
//! wait for data or capacity; `send` / `next` suspend through task wakers.
//! A full queue applies backpressure, with no implicit frame dropping. Finish
//! drains queued items before EOF. Failure or cancellation discards the queue
//! and delivers one error followed by EOF. Dropping the sender finishes the
//! stream; dropping the receiver cancels it and wakes a waiting sender.
//!
//! Both item count and payload bytes are bounded. Limits account for each queued
//! reference separately, even when several frames share storage. They do not
//! bound data already handed to a consumer or retained by a caller. Byte chunks
//! need not correspond to container packets, and no source must support seek.

use std::collections::VecDeque;
use std::future::poll_fn;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use crate::{AudioData, EncodedChunk, Error, ErrorKind, Result, VideoFrame};

/// The immutable payload size used to account for queue capacity.
/// Implementations must return a stable size while an item is queued.
pub trait Payload {
    fn payload_bytes(&self) -> usize;
}

impl Payload for AudioData {
    fn payload_bytes(&self) -> usize {
        self.bytes().len()
    }
}
impl Payload for VideoFrame {
    fn payload_bytes(&self) -> usize {
        self.bytes().len()
    }
}
impl Payload for EncodedChunk {
    fn payload_bytes(&self) -> usize {
        self.bytes().len()
    }
}
impl Payload for [u8] {
    fn payload_bytes(&self) -> usize {
        self.len()
    }
}
impl Payload for Vec<u8> {
    fn payload_bytes(&self) -> usize {
        self.len()
    }
}
impl<T: Payload + ?Sized> Payload for Arc<T> {
    fn payload_bytes(&self) -> usize {
        (**self).payload_bytes()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_items: usize,
    pub max_bytes: usize,
}

#[derive(Debug)]
pub enum Read<T> {
    Item(T),
    Pending,
    End,
}

/// A rejected send returns ownership so a producer can retry after backpressure.
#[derive(Debug)]
pub struct SendError<T> {
    pub error: Error,
    pub value: T,
}

enum Terminal {
    Open,
    Finished,
    Failed(Error),
}

struct State<T> {
    queue: VecDeque<(T, usize)>,
    bytes: usize,
    terminal: Terminal,
    error_delivered: bool,
    reader_ended: bool,
    reader: Option<Waker>,
    writer: Option<Waker>,
}

struct Shared<T> {
    limits: Limits,
    state: Mutex<State<T>>,
}

pub struct Sender<T: Payload> {
    shared: Arc<Shared<T>>,
}
pub struct Receiver<T: Payload> {
    shared: Arc<Shared<T>>,
}

pub fn channel<T: Payload>(limits: Limits) -> Result<(Sender<T>, Receiver<T>)> {
    if limits.max_items == 0 || limits.max_bytes == 0 {
        return Err(Error::invalid(
            "stream item and byte limits must be positive",
        ));
    }
    let shared = Arc::new(Shared {
        limits,
        state: Mutex::new(State {
            queue: VecDeque::new(),
            bytes: 0,
            terminal: Terminal::Open,
            error_delivered: false,
            reader_ended: false,
            reader: None,
            writer: None,
        }),
    });
    Ok((
        Sender {
            shared: shared.clone(),
        },
        Receiver { shared },
    ))
}

fn poisoned() -> Error {
    Error::new(ErrorKind::InvalidState, "stream state lock is poisoned")
}

fn remember(slot: &mut Option<Waker>, cx: Option<&Context<'_>>) {
    if let Some(cx) = cx
        && slot.as_ref().is_none_or(|w| !w.will_wake(cx.waker()))
    {
        *slot = Some(cx.waker().clone());
    }
}

impl<T: Payload> Sender<T> {
    pub fn try_send(&mut self, value: T) -> std::result::Result<(), SendError<T>> {
        let bytes = value.payload_bytes();
        let mut pending = Some(value);
        match self.enqueue(&mut pending, bytes, None) {
            Poll::Ready(Ok(())) => Ok(()),
            Poll::Ready(Err(error)) => Err(SendError {
                error,
                value: pending.unwrap(),
            }),
            Poll::Pending => Err(SendError {
                error: Error::new(ErrorKind::WouldBlock, "stream queue is full"),
                value: pending.unwrap(),
            }),
        }
    }

    /// Waits for capacity without blocking the executor. Cancelling this future
    /// drops its unsent value; it never inserts a partial payload.
    pub async fn send(&mut self, value: T) -> std::result::Result<(), SendError<T>> {
        let bytes = value.payload_bytes();
        let mut pending = Some(value);
        poll_fn(|cx| self.enqueue(&mut pending, bytes, Some(cx)))
            .await
            .map_err(|error| SendError {
                error,
                value: pending.unwrap(),
            })
    }

    /// End input after the queued items. Repeated finish is harmless.
    pub fn finish(&mut self) -> Result<()> {
        self.shared.terminate(Terminal::Finished, false)
    }

    /// A fatal source error: queued output is discarded, and the reader wakes.
    pub fn fail(&mut self, error: Error) -> Result<()> {
        self.shared.terminate(Terminal::Failed(error), true)
    }

    fn enqueue(
        &self,
        value: &mut Option<T>,
        bytes: usize,
        cx: Option<&Context<'_>>,
    ) -> Poll<Result<()>> {
        let mut state = match self.shared.state.lock() {
            Ok(state) => state,
            Err(_) => return Poll::Ready(Err(poisoned())),
        };
        match &state.terminal {
            Terminal::Finished => {
                return Poll::Ready(Err(Error::new(
                    ErrorKind::InvalidState,
                    "stream is finished",
                )));
            }
            Terminal::Failed(error) => return Poll::Ready(Err(error.clone())),
            Terminal::Open => {}
        }
        if bytes > self.shared.limits.max_bytes {
            return Poll::Ready(Err(Error::invalid("payload exceeds the stream byte limit")));
        }
        if state.queue.len() >= self.shared.limits.max_items
            || bytes > self.shared.limits.max_bytes - state.bytes
        {
            remember(&mut state.writer, cx);
            return Poll::Pending;
        }
        if state.queue.try_reserve(1).is_err() {
            return Poll::Ready(Err(Error::exhausted()));
        }
        state
            .queue
            .push_back((value.take().expect("enqueue owns an unsent payload"), bytes));
        state.bytes += bytes;
        let reader = state.reader.take();
        drop(state);
        if let Some(waker) = reader {
            waker.wake();
        }
        Poll::Ready(Ok(()))
    }
}

impl<T: Payload> Receiver<T> {
    pub fn try_next(&mut self) -> Result<Read<T>> {
        self.dequeue(None)
    }

    pub fn poll_next(&mut self, cx: &mut Context<'_>) -> Poll<Option<Result<T>>> {
        match self.dequeue(Some(cx)) {
            Ok(Read::Item(value)) => Poll::Ready(Some(Ok(value))),
            Ok(Read::Pending) => Poll::Pending,
            Ok(Read::End) => Poll::Ready(None),
            Err(error) => Poll::Ready(Some(Err(error))),
        }
    }

    pub async fn next(&mut self) -> Option<Result<T>> {
        poll_fn(|cx| self.poll_next(cx)).await
    }

    pub fn cancel(&mut self) -> Result<()> {
        self.shared.terminate(
            Terminal::Failed(Error::new(
                ErrorKind::Cancelled,
                "stream consumer cancelled",
            )),
            true,
        )
    }

    fn dequeue(&self, cx: Option<&Context<'_>>) -> Result<Read<T>> {
        let mut state = self.shared.state.lock().map_err(|_| poisoned())?;
        if let Some((value, bytes)) = state.queue.pop_front() {
            state.bytes -= bytes;
            let writer = state.writer.take();
            drop(state);
            if let Some(waker) = writer {
                waker.wake();
            }
            return Ok(Read::Item(value));
        }
        match &state.terminal {
            Terminal::Open => {
                remember(&mut state.reader, cx);
                Ok(Read::Pending)
            }
            Terminal::Failed(error) if !state.error_delivered => {
                let error = error.clone();
                state.error_delivered = true;
                Err(error)
            }
            _ => {
                state.reader_ended = true;
                Ok(Read::End)
            }
        }
    }
}

impl<T> Shared<T> {
    fn terminate(&self, terminal: Terminal, discard: bool) -> Result<()> {
        let mut state = self.state.lock().map_err(|_| poisoned())?;
        // Cancellation can discard a gracefully finishing queue. The first
        // fatal error is retained; finish never overwrites an error.
        if state.reader_ended
            || matches!(state.terminal, Terminal::Failed(_))
            || (matches!(state.terminal, Terminal::Finished) && !discard)
        {
            return Ok(());
        }
        state.terminal = terminal;
        let removed = if discard {
            state.bytes = 0;
            std::mem::take(&mut state.queue)
        } else {
            VecDeque::new()
        };
        let reader = state.reader.take();
        let writer = state.writer.take();
        drop(state);
        // Never invoke destructors or executor wake callbacks under the lock.
        drop(removed);
        if let Some(waker) = reader {
            waker.wake();
        }
        if let Some(waker) = writer {
            waker.wake();
        }
        Ok(())
    }
}

impl<T: Payload> Drop for Sender<T> {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}
impl<T: Payload> Drop for Receiver<T> {
    fn drop(&mut self) {
        let _ = self.cancel();
    }
}
