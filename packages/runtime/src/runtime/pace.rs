//! Where an async call starts: on the caller's thread, or on a worker.
//!
//! A call that finishes without waiting costs no thread when it runs on the
//! caller's thread, but that thread runs the host's event loop: long work
//! there stops the loop, and the calls of one loop run one after another on
//! one core. Each async export keeps a [`Pace`]: how long its calls work,
//! which each timed call updates, and how many of them run on workers.
//! A call whose export works much longer than a hand-off to a worker costs
//! starts on a worker, and so does one that works longer than the hand-off
//! while other calls of its export run on workers.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::Instant;

/// What handing a call to a worker and its result back costs, in
/// nanoseconds: two wake-ups of a thread, which the crossing lab measures at
/// 10 to 30 µs.
pub(crate) const HAND_OFF: u64 = 25_000;

/// The most work that a call does on the caller's thread while no other call
/// of its export runs on a worker, so that the hand-off adds at most 40% to
/// the work it moves.
pub(crate) const INLINE: u64 = HAND_OFF * 5 / 2;

/// While an export works for less than a quarter of the hand-off, one call
/// in this many times itself, since timing a call costs two reads of the
/// clock.
const SAMPLE: u32 = 8;

/// How the calls of one async export work, which decides where the next one
/// starts. The macros keep one in a static for each export.
#[doc(hidden)]
#[derive(Debug, Default)]
pub struct Pace {
    /// A running average of the nanoseconds that a call spends in its polls,
    /// or 0 before the export has learned anything.
    work: AtomicU64,
    /// The nanoseconds of the last timed call.
    last: AtomicU64,
    /// The calls of the export that run on workers because it works long.
    handed: AtomicU32,
    /// The calls since the last timed one, while the export works briefly.
    untimed: AtomicU32,
}

impl Pace {
    /// A pace with no timing yet, so the first call starts on the caller's
    /// thread.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            work: AtomicU64::new(0),
            last: AtomicU64::new(0),
            handed: AtomicU32::new(0),
            untimed: AtomicU32::new(0),
        }
    }

    /// Whether the next call starts on a worker.
    fn hands_off(&self) -> bool {
        let work = self.work.load(Ordering::Relaxed);
        work > INLINE || (work > HAND_OFF && self.handed.load(Ordering::Relaxed) > 0)
    }

    /// Whether to time the next call: every call of an export that works for
    /// a quarter of the hand-off or more, or whose last timed call did, and
    /// one in [`SAMPLE`] of the rest.
    fn times(&self) -> bool {
        self.work.load(Ordering::Relaxed) >= HAND_OFF / 4
            || self.last.load(Ordering::Relaxed) >= HAND_OFF / 4
            || self
                .untimed
                .fetch_add(1, Ordering::Relaxed)
                .is_multiple_of(SAMPLE)
    }

    /// Learn from a call that spent `work` nanoseconds in its polls.
    ///
    /// A call counts as the lesser of its time and that of the last timed
    /// call, so that a call that the scheduler stalled, or that met an
    /// unusual input, moves no later call to a worker. Each count moves the
    /// average a quarter of the way, so calls of more than four times
    /// [`INLINE`] move their export after two in a row, and shorter long calls
    /// after a few more. Calls that finish together race to update it, and a
    /// lost sample costs nothing.
    fn learn(&self, work: u64) {
        let work = work.min(self.last.swap(work, Ordering::Relaxed));
        let average = self.work.load(Ordering::Relaxed);
        let next = if average == 0 {
            work
        } else {
            average - average / 4 + work / 4
        };
        self.work.store(next, Ordering::Relaxed);
    }
}

/// An async call's future, which times its polls for its export's [`Pace`],
/// and counts itself among the export's calls on workers while it runs on
/// one.
pub(crate) struct Paced<F> {
    future: Pin<Box<F>>,
    pace: Option<&'static Pace>,
    /// The nanoseconds that the call has spent in its polls, or `None` when
    /// the call does not time itself.
    work: Option<u64>,
    /// Whether the call counts among its export's calls on workers.
    handed: bool,
}

impl<F: Future> Paced<F> {
    /// Time `future` for `pace`, if it has one.
    pub(crate) fn new(future: F, pace: Option<&'static Pace>) -> Self {
        Self {
            future: Box::pin(future),
            pace,
            work: pace.filter(|pace| pace.times()).map(|_| 0),
            handed: false,
        }
    }

    /// Whether the call should start on a worker, which then counts it among
    /// its export's calls on workers until it finishes or drops.
    pub(crate) fn hands_off(&mut self) -> bool {
        let Some(pace) = self.pace else {
            return false;
        };
        if !pace.hands_off() {
            return false;
        }
        pace.handed.fetch_add(1, Ordering::Relaxed);
        self.handed = true;
        true
    }

    /// Stop counting the call among its export's calls on workers.
    fn settle(&mut self) {
        if self.handed
            && let Some(pace) = self.pace
        {
            pace.handed.fetch_sub(1, Ordering::Relaxed);
            self.handed = false;
        }
    }
}

impl<F: Future> Future for Paced<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<F::Output> {
        let Some(spent) = self.work else {
            let poll = self.future.as_mut().poll(context);
            if poll.is_ready() {
                self.settle();
            }
            return poll;
        };
        let started = Instant::now();
        let poll = self.future.as_mut().poll(context);
        let elapsed = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let spent = spent.saturating_add(elapsed);
        self.work = Some(spent);
        if poll.is_ready() {
            if let Some(pace) = self.pace {
                pace.learn(spent);
            }
            self.settle();
        }
        poll
    }
}

impl<F> Drop for Paced<F> {
    fn drop(&mut self) {
        if self.handed
            && let Some(pace) = self.pace
        {
            pace.handed.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::task::Waker;

    use super::*;

    /// Guards the decision: an export that works briefly stays on the
    /// caller's thread, one that works long goes to a worker, and one that
    /// works longer than a hand-off goes there too while other calls of it
    /// run on workers.
    #[test]
    fn a_call_starts_where_its_export_works_best() {
        let pace = Pace::new();
        assert!(
            !pace.hands_off(),
            "the first call starts on the caller's thread"
        );
        pace.learn(INLINE / 2);
        pace.learn(INLINE / 2);
        assert!(!pace.hands_off(), "a lone call that works briefly stays");
        pace.handed.store(1, Ordering::Relaxed);
        assert!(
            pace.hands_off(),
            "with calls on workers, it works longer than a hand-off"
        );
        pace.handed.store(0, Ordering::Relaxed);
        pace.learn(INLINE * 10);
        assert!(!pace.hands_off(), "one long call moves no later call");
        assert!(pace.times(), "the call after a long one times itself");
        pace.learn(INLINE * 10);
        assert!(
            pace.hands_off(),
            "an export that works long goes to a worker"
        );
        for _ in 0..20 {
            pace.learn(0);
        }
        assert!(
            !pace.hands_off(),
            "an export that works briefly again comes back"
        );
    }

    /// Guards the count of calls on workers: a call that goes to a worker
    /// counts until it finishes, and one that drops unfinished stops counting
    /// too.
    #[test]
    fn calls_on_workers_count_until_they_finish_or_drop() {
        static PACE: Pace = Pace::new();
        PACE.learn(INLINE * 10);
        PACE.learn(INLINE * 10);
        let mut finished = Paced::new(std::future::ready(1), Some(&PACE));
        assert!(finished.hands_off());
        let mut dropped = Paced::new(std::future::pending::<()>(), Some(&PACE));
        assert!(dropped.hands_off());
        assert_eq!(PACE.handed.load(Ordering::Relaxed), 2);
        let mut context = Context::from_waker(Waker::noop());
        assert_eq!(Pin::new(&mut finished).poll(&mut context), Poll::Ready(1));
        assert_eq!(PACE.handed.load(Ordering::Relaxed), 1);
        drop(dropped);
        assert_eq!(PACE.handed.load(Ordering::Relaxed), 0);
        drop(finished);
        assert_eq!(PACE.handed.load(Ordering::Relaxed), 0);
    }
}
