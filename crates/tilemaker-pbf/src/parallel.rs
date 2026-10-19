//! Order-preserving parallel map with bounded memory.
//!
//! Items are produced on a feeder thread (where sequential I/O belongs),
//! transformed on a pool of workers, and handed back in their original
//! order. At most `window` items are in flight or waiting to be consumed, so
//! a slow consumer slows the producer instead of buffering the whole input.
//!
//! A panic in `f` is caught on the worker and re-raised on the consumer's
//! thread when its turn comes, rather than leaving a gap that would hang
//! or silently end the iteration.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use crossbeam_channel::{bounded, unbounded, Receiver, Sender};

/// The default number of worker threads: the machine's parallelism.
pub fn default_threads() -> usize {
    thread::available_parallelism().map(|n| n.get()).unwrap_or(4)
}

/// An iterator over results in input order (see the module docs).
type Outcome<R> = std::thread::Result<R>;

pub struct OrderedMap<R> {
    results: Receiver<(usize, Outcome<R>)>,
    pending: BTreeMap<usize, Outcome<R>>,
    next: usize,
    permits: Option<Receiver<()>>,
    cancel: Arc<AtomicBool>,
}

/// Map `items` through `f` on `threads` workers, keeping input order.
pub fn ordered_map<T, R, I, F>(items: I, threads: usize, window: usize, f: F) -> OrderedMap<R>
where
    T: Send + 'static,
    R: Send + 'static,
    I: IntoIterator<Item = T>,
    I::IntoIter: Send + 'static,
    F: Fn(T) -> R + Send + Sync + 'static,
{
    let threads = threads.max(1);
    let window = window.max(threads);
    let (permit_tx, permit_rx) = bounded::<()>(window);
    let (work_tx, work_rx) = unbounded::<(usize, T)>();
    let (res_tx, res_rx) = unbounded::<(usize, Outcome<R>)>();
    let cancel = Arc::new(AtomicBool::new(false));

    let items = items.into_iter();
    {
        let cancel = cancel.clone();
        thread::Builder::new()
            .name("pbf-feeder".into())
            .spawn(move || feed(items, permit_tx, work_tx, cancel))
            .expect("spawn feeder thread");
    }
    let f = Arc::new(f);
    for i in 0..threads {
        let work_rx = work_rx.clone();
        let res_tx = res_tx.clone();
        let f = f.clone();
        let cancel = cancel.clone();
        thread::Builder::new()
            .name(format!("pbf-worker-{i}"))
            .spawn(move || {
                while let Ok((seq, item)) = work_rx.recv() {
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(item)));
                    if res_tx.send((seq, outcome)).is_err() {
                        break;
                    }
                }
            })
            .expect("spawn worker thread");
    }
    OrderedMap {
        results: res_rx,
        pending: BTreeMap::new(),
        next: 0,
        permits: Some(permit_rx),
        cancel,
    }
}

fn feed<T>(items: impl Iterator<Item = T>, permits: Sender<()>, work: Sender<(usize, T)>, cancel: Arc<AtomicBool>) {
    for (seq, item) in items.enumerate() {
        if cancel.load(Ordering::Relaxed) || permits.send(()).is_err() {
            return;
        }
        if work.send((seq, item)).is_err() {
            return;
        }
    }
}

impl<R> Iterator for OrderedMap<R> {
    type Item = R;

    fn next(&mut self) -> Option<R> {
        loop {
            if let Some(r) = self.pending.remove(&self.next) {
                self.next += 1;
                if let Some(p) = &self.permits {
                    let _ = p.try_recv();
                }
                match r {
                    Ok(r) => return Some(r),
                    Err(payload) => std::panic::resume_unwind(payload),
                }
            }
            match self.results.recv() {
                Ok((seq, r)) => {
                    self.pending.insert(seq, r);
                }
                Err(_) => return None,
            }
        }
    }
}

impl<R> Drop for OrderedMap<R> {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        // Unblock a feeder waiting for a permit.
        self.permits.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn keeps_order() {
        let out: Vec<u64> = ordered_map(0..1000u64, 8, 16, |i| {
            if i % 7 == 0 {
                thread::sleep(Duration::from_micros(200));
            }
            i * 2
        })
        .collect();
        assert_eq!(out, (0..1000).map(|i| i * 2).collect::<Vec<_>>());
    }

    #[test]
    fn bounded_window() {
        use std::sync::atomic::AtomicUsize;
        let produced = Arc::new(AtomicUsize::new(0));
        let p = produced.clone();
        let iter = (0..10_000).inspect(move |_| {
            p.fetch_add(1, Ordering::SeqCst);
        });
        let mut m = ordered_map(iter, 2, 8, |i| i);
        assert_eq!(m.next(), Some(0));
        thread::sleep(Duration::from_millis(50));
        // The feeder can only be a window (plus one waiting item) ahead.
        assert!(
            produced.load(Ordering::SeqCst) <= 10,
            "{}",
            produced.load(Ordering::SeqCst)
        );
    }

    #[test]
    fn worker_panics_reach_the_consumer() {
        for (n, window) in [(10u64, 32usize), (1000, 8)] {
            let r = std::panic::catch_unwind(|| {
                ordered_map(0..n, 2, window, |i| {
                    if i == 3 {
                        panic!("boom");
                    }
                    i
                })
                .collect::<Vec<_>>()
            });
            let err = r.unwrap_err();
            assert_eq!(err.downcast_ref::<&str>(), Some(&"boom"));
        }
    }

    #[test]
    fn drop_early_stops() {
        let m = ordered_map(0..u64::MAX, 4, 8, |i| i);
        let first: Vec<u64> = m.take(5).collect();
        assert_eq!(first, vec![0, 1, 2, 3, 4]);
    }
}
