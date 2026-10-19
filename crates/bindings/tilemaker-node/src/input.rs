//! Bytes pushed from JS (a download, a file stream, stdin) read by the
//! export thread through [`std::io::Read`].
//!
//! Flow control on this side too: `push` reports when more than
//! [`HIGH_WATER`] bytes wait unread, and a `drain` event fires once the
//! reader has brought that below [`LOW_WATER`].

use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};
use napi::threadsafe_function::ThreadsafeFunctionCallMode;

use crate::stream::{Callback, StreamEvent};

/// Unread bytes above which `push` asks JS to wait for `drain`.
pub(crate) const HIGH_WATER: usize = 8 << 20;
/// Unread bytes below which a waiting JS producer is told to continue.
pub(crate) const LOW_WATER: usize = 2 << 20;

pub(crate) struct Shared {
    buffered: AtomicUsize,
    waiting: AtomicBool,
}

/// What `push` tells JS.
pub(crate) enum PushStatus {
    /// Keep pushing.
    Ok = 0,
    /// Wait for the `drain` event.
    Full = 1,
    /// The export is over; stop.
    Closed = 2,
}

/// The JS end.
pub(crate) struct InputSender {
    tx: Sender<Vec<u8>>,
    shared: Arc<Shared>,
}

impl InputSender {
    pub(crate) fn push(&self, chunk: Vec<u8>) -> PushStatus {
        let n = chunk.len();
        let total = self.shared.buffered.fetch_add(n, Ordering::SeqCst) + n;
        if self.tx.send(chunk).is_err() {
            return PushStatus::Closed;
        }
        if total > HIGH_WATER {
            self.shared.waiting.store(true, Ordering::SeqCst);
            // The reader may have drained everything in between.
            if self.shared.buffered.load(Ordering::SeqCst) <= LOW_WATER {
                self.shared.waiting.store(false, Ordering::SeqCst);
                return PushStatus::Ok;
            }
            return PushStatus::Full;
        }
        PushStatus::Ok
    }
}

/// The export thread's end.
pub(crate) struct ChannelReader {
    rx: Receiver<Vec<u8>>,
    current: Vec<u8>,
    pos: usize,
    shared: Arc<Shared>,
    cb: Callback,
}

impl Read for ChannelReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        while self.pos >= self.current.len() {
            match self.rx.recv() {
                Ok(chunk) => {
                    let n = chunk.len();
                    self.current = chunk;
                    self.pos = 0;
                    let left = self.shared.buffered.fetch_sub(n, Ordering::SeqCst) - n;
                    if left <= LOW_WATER && self.shared.waiting.swap(false, Ordering::SeqCst) {
                        self.cb
                            .call(StreamEvent::Drain, ThreadsafeFunctionCallMode::NonBlocking);
                    }
                }
                // The sender is gone: end of input.
                Err(_) => return Ok(0),
            }
        }
        let n = buf.len().min(self.current.len() - self.pos);
        buf[..n].copy_from_slice(&self.current[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// A connected pair.
pub(crate) fn channel(cb: Callback) -> (InputSender, ChannelReader) {
    let (tx, rx) = crossbeam_channel::unbounded();
    let shared = Arc::new(Shared {
        buffered: AtomicUsize::new(0),
        waiting: AtomicBool::new(false),
    });
    (
        InputSender {
            tx,
            shared: shared.clone(),
        },
        ChannelReader {
            rx,
            current: Vec::new(),
            pos: 0,
            shared,
            cb,
        },
    )
}
