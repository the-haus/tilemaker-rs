//! A running export: a producer thread, a threadsafe function that hands
//! events to JS, and the credits that bound how far it runs ahead.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use napi::threadsafe_function::{ErrorStrategy, ThreadSafeCallContext, ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi::{Env, JsFunction, JsObject, JsUnknown, Result};
use napi_derive::napi;
use tilemaker_export::{Event, ExportStats, Flow, Progress};

use crate::batch::{ElementBatch, FeatureBatch};
use crate::credits::Credits;

/// What the producer sends to JS.
pub(crate) enum StreamEvent {
    Features(Box<FeatureBatch>),
    Elements(Box<ElementBatch>),
    Tiles(crate::tiles::TileBatch),
    Progress(Progress),
    TileProgress(tilemaker_core::Progress),
    /// Input bytes were consumed; JS may push more.
    Drain,
    End(serde_json::Value),
    Error(String),
}

impl StreamEvent {
    fn into_js(self, env: &Env) -> Result<JsObject> {
        let mut o = env.create_object()?;
        match self {
            StreamEvent::Features(b) => {
                o.set("type", "batch")?;
                o.set("batch", b.into_js(env)?)?;
            }
            StreamEvent::Elements(b) => {
                o.set("type", "batch")?;
                o.set("batch", b.into_js(env)?)?;
            }
            StreamEvent::Tiles(b) => {
                o.set("type", "batch")?;
                o.set("batch", b.into_js(env)?)?;
            }
            StreamEvent::TileProgress(p) => {
                o.set("type", "progress")?;
                match p {
                    tilemaker_core::Progress::Read {
                        phase,
                        blocks_done,
                        blocks_total,
                    } => {
                        o.set("phase", phase.as_str())?;
                        o.set("blocksDone", blocks_done as u32)?;
                        o.set("blocksTotal", blocks_total as u32)?;
                    }
                    tilemaker_core::Progress::Tiles { zoom, done, total } => {
                        o.set("phase", "tiles")?;
                        o.set("zoom", zoom as u32)?;
                        o.set("tilesDone", done as u32)?;
                        o.set("tilesTotal", total as u32)?;
                    }
                }
            }
            StreamEvent::Progress(p) => {
                o.set("type", "progress")?;
                o.set("phase", p.phase.as_str())?;
                o.set("blocksDone", p.blocks_done as u32)?;
                o.set("blocksTotal", p.blocks_total as u32)?;
            }
            StreamEvent::Drain => {
                o.set("type", "drain")?;
            }
            StreamEvent::End(stats) => {
                o.set("type", "end")?;
                o.set("stats", env.to_js_value(&as_doubles(stats))?)?;
            }
            StreamEvent::Error(message) => {
                o.set("type", "error")?;
                o.set("message", message)?;
            }
        }
        Ok(o)
    }
}

/// Integers as doubles: napi turns those beyond 32 bits into BigInts,
/// while JS wants plain numbers (ids and byte counts fit in 53 bits).
fn as_doubles(v: serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match v {
        Value::Number(n) if !n.is_f64() => n
            .as_f64()
            .and_then(serde_json::Number::from_f64)
            .map_or(Value::Number(n), Value::Number),
        Value::Array(a) => Value::Array(a.into_iter().map(as_doubles).collect()),
        Value::Object(o) => Value::Object(o.into_iter().map(|(k, v)| (k, as_doubles(v))).collect()),
        other => other,
    }
}

pub(crate) type Callback = ThreadsafeFunction<StreamEvent, ErrorStrategy::Fatal>;

pub(crate) fn callback(f: &JsFunction) -> Result<Callback> {
    f.create_threadsafe_function(0, |ctx: ThreadSafeCallContext<StreamEvent>| {
        let v: JsUnknown = ctx.value.into_js(&ctx.env)?.into_unknown();
        Ok(vec![v])
    })
}

/// The handle JS holds for a running export.
///
/// It keeps a handle on the threadsafe function so JS can decide when the
/// stream keeps the event loop alive: only while it awaits a batch (or the
/// end of a closed stream). An abandoned, half-read stream then does not
/// keep the process running.
#[napi]
pub struct NativeStream {
    pub(crate) credits: Arc<Credits>,
    pub(crate) callback: std::sync::Mutex<Option<Callback>>,
    /// For streams over pushed bytes: where `push` sends them.
    pub(crate) input: std::sync::Mutex<Option<crate::input::InputSender>>,
}

impl Drop for NativeStream {
    // Collected without being finished (JS dropped the stream): stop the
    // producer so it does not wait for credits forever.
    fn drop(&mut self) {
        self.credits.close();
        self.input.lock().unwrap().take();
    }
}

impl NativeStream {
    pub(crate) fn new(credits: Arc<Credits>, callback: Callback) -> Self {
        Self {
            credits,
            callback: std::sync::Mutex::new(Some(callback)),
            input: std::sync::Mutex::new(None),
        }
    }
}

#[napi]
impl NativeStream {
    /// Whether the stream keeps the event loop alive.
    #[napi]
    pub fn set_ref(&self, env: Env, referenced: bool) -> napi::Result<()> {
        if let Some(cb) = self.callback.lock().unwrap().as_mut() {
            if referenced {
                cb.refer(&env)?;
            } else {
                cb.unref(&env)?;
            }
        }
        Ok(())
    }

    /// Drop this handle's reference to the callback (after the end).
    #[napi]
    pub fn release(&self) {
        self.callback.lock().unwrap().take();
    }

    /// Allow `n` more batches to be sent.
    #[napi]
    pub fn request(&self, n: u32) {
        self.credits.grant(n);
    }

    /// Stop the export; no more batches are sent, then `end` arrives.
    #[napi]
    pub fn close(&self) {
        self.credits.close();
        // A reader waiting for input sees the end of it.
        self.input.lock().unwrap().take();
    }

    /// Push input bytes (streams over pushed bytes only): 0 keep going,
    /// 1 wait for the `drain` event, 2 the export is over.
    #[napi]
    pub fn push(&self, chunk: napi::bindgen_prelude::Buffer) -> u32 {
        match self.input.lock().unwrap().as_ref() {
            Some(i) => i.push(chunk.to_vec()) as u32,
            None => crate::input::PushStatus::Closed as u32,
        }
    }

    /// No more input.
    #[napi]
    pub fn end_input(&self) {
        self.input.lock().unwrap().take();
    }
}

/// Accumulates records into batches and sends them under flow control.
pub(crate) struct Sender<B> {
    cb: Callback,
    credits: Arc<Credits>,
    batch: B,
    batch_size: usize,
    wrap: fn(B) -> StreamEvent,
    last_progress: Option<(Instant, tilemaker_export::Phase)>,
}

pub(crate) trait Batch: Default {
    type Item;
    fn push(&mut self, item: Self::Item);
    fn len(&self) -> usize;
    fn coords_len(&self) -> usize;
}

impl Batch for FeatureBatch {
    type Item = tilemaker_export::Feature;
    fn push(&mut self, item: Self::Item) {
        FeatureBatch::push(self, item)
    }
    fn len(&self) -> usize {
        FeatureBatch::len(self)
    }
    fn coords_len(&self) -> usize {
        FeatureBatch::coords_len(self)
    }
}

impl Batch for ElementBatch {
    type Item = tilemaker_export::Element;
    fn push(&mut self, item: Self::Item) {
        ElementBatch::push(self, item)
    }
    fn len(&self) -> usize {
        ElementBatch::len(self)
    }
    fn coords_len(&self) -> usize {
        ElementBatch::coords_len(self)
    }
}

/// Batches also flush at this many coordinates, so one huge geometry
/// (a country) does not wait for thousands of others.
const MAX_BATCH_COORDS: usize = 4 << 20;

impl<B: Batch> Sender<B> {
    fn flush(&mut self) -> Flow {
        if self.batch.len() == 0 {
            return Flow::Continue;
        }
        if !self.credits.acquire() {
            return Flow::Stop;
        }
        let b = std::mem::take(&mut self.batch);
        self.cb.call((self.wrap)(b), ThreadsafeFunctionCallMode::Blocking);
        Flow::Continue
    }

    pub(crate) fn on_event(&mut self, e: Event<B::Item>) -> Flow {
        if self.credits.is_closed() {
            return Flow::Stop;
        }
        match e {
            Event::Batch(items) => {
                for item in items {
                    self.batch.push(item);
                    if (self.batch.len() >= self.batch_size || self.batch.coords_len() >= MAX_BATCH_COORDS)
                        && self.flush() == Flow::Stop
                    {
                        return Flow::Stop;
                    }
                }
                Flow::Continue
            }
            Event::Progress(p) => {
                let now = Instant::now();
                let due = match self.last_progress {
                    None => true,
                    Some((t, phase)) => {
                        phase != p.phase || p.blocks_done == p.blocks_total || now - t >= Duration::from_millis(200)
                    }
                };
                if due {
                    self.last_progress = Some((now, p.phase));
                    self.cb
                        .call(StreamEvent::Progress(p), ThreadsafeFunctionCallMode::NonBlocking);
                }
                Flow::Continue
            }
        }
    }
}

/// Run `job` on its own thread, streaming to `cb`. Returns the JS handle.
pub(crate) fn spawn<B, F>(
    cb: Callback,
    batch_size: usize,
    read_ahead: u32,
    wrap: fn(B) -> StreamEvent,
    job: F,
) -> NativeStream
where
    B: Batch + Send + 'static,
    F: FnOnce(&mut dyn FnMut(Event<B::Item>) -> Flow) -> std::result::Result<ExportStats, String> + Send + 'static,
{
    let credits = Arc::new(Credits::default());
    credits.grant(read_ahead.max(1));
    let handle = NativeStream::new(credits.clone(), cb.clone());
    thread::Builder::new()
        .name("tilemaker-export".into())
        .spawn(move || {
            let mut sender = Sender {
                cb: cb.clone(),
                credits,
                batch: B::default(),
                batch_size: batch_size.max(1),
                wrap,
                last_progress: None,
            };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(&mut |e| sender.on_event(e))))
                .unwrap_or_else(|p| Err(format!("internal error: {}", panic_message(&p))));
            let result = result.map(|mut stats| {
                if sender.flush() == Flow::Stop {
                    stats.stopped = true;
                }
                stats
            });
            let event = match result.and_then(|s| serde_json::to_value(&s).map_err(|e| e.to_string())) {
                Ok(stats) => StreamEvent::End(stats),
                Err(e) => StreamEvent::Error(e),
            };
            cb.call(event, ThreadsafeFunctionCallMode::Blocking);
        })
        .expect("spawn export thread");
    handle
}

/// Run `job` (which emits no records) on its own thread, reporting its
/// progress (throttled) and then its end or error to `cb`. The job's
/// progress callback says `Stop` once JS closes the stream.
pub(crate) fn spawn_task<F>(cb: Callback, job: F) -> NativeStream
where
    F: FnOnce(&mut dyn FnMut(Progress) -> Flow) -> std::result::Result<serde_json::Value, String> + Send + 'static,
{
    let credits = Arc::new(Credits::default());
    let handle = NativeStream::new(credits.clone(), cb.clone());
    thread::Builder::new()
        .name("tilemaker-task".into())
        .spawn(move || {
            let mut last: Option<(Instant, tilemaker_export::Phase)> = None;
            let mut progress = |p: Progress| {
                if credits.is_closed() {
                    return Flow::Stop;
                }
                let now = Instant::now();
                let due = match last {
                    None => true,
                    Some((t, phase)) => {
                        phase != p.phase || p.blocks_done == p.blocks_total || now - t >= Duration::from_millis(200)
                    }
                };
                if due {
                    last = Some((now, p.phase));
                    cb.call(StreamEvent::Progress(p), ThreadsafeFunctionCallMode::NonBlocking);
                }
                Flow::Continue
            };
            let event = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(&mut progress)))
                .unwrap_or_else(|p| Err(format!("internal error: {}", panic_message(&p))))
                .map_or_else(StreamEvent::Error, StreamEvent::End);
            cb.call(event, ThreadsafeFunctionCallMode::Blocking);
        })
        .expect("spawn task thread");
    handle
}

/// The message of a caught panic.
pub(crate) fn panic_message(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a worker thread panicked".into())
}
