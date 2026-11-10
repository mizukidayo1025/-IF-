//! The per-person compute of a tick on more than one thread (lane L12 of docs/rust-port/step4-plan.md).
//!
//! A kernel that splits into a compute over rows and a serial apply gives each person's compute to
//! `in_order`, which returns the results in the people's order, and then applies them in that
//! order. The compute reads only Rust data: no Python, no GIL, no HashMap iteration. Draws are keyed
//! SHA-256 and take no order; event sequence numbers and every write stay in the serial apply. So a
//! tick computes the same bytes with any number of threads, as with one.
//!
//! The calling thread works too, and keeps the GIL, so no other Python thread runs between the
//! compute and the apply. Helpers from a small pool of the tick's own take runs of people as they
//! come; the parse of saved records (checkpoint.rs, eventlog.rs) keeps rayon's global pool, so a
//! tick never waits behind it. Waking a helper costs tens of microseconds, so a compute of few
//! people stays on the calling thread (docs/rust-port.md has the measurements).
//!
//! The pool has at most `MAX_HELPERS` threads, and one fewer than rayon's global pool:
//! `RAYON_NUM_THREADS=1` is no helper, and `4` is the calling thread and three helpers.

use std::ops::Range;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

use rayon::{ThreadPool, ThreadPoolBuilder};

use crate::{census, rng};

/// More helpers cost more to wake than they save at the sizes of a shelter (measured on 16 cores).
const MAX_HELPERS: usize = 3;
/// A helper takes this many people at a time.
const PEOPLE_PER_PART: usize = 32;
/// One helper for each this many people: below it, the calling thread does all the work.
const PEOPLE_PER_HELPER: usize = 256;

static PER_HELPER: AtomicUsize = AtomicUsize::new(PEOPLE_PER_HELPER);
static POOL: OnceLock<Option<ThreadPool>> = OnceLock::new();

fn per_helper() -> usize {
    PER_HELPER.load(Ordering::Relaxed).max(1)
}

fn pool() -> Option<&'static ThreadPool> {
    POOL.get_or_init(|| {
        let helpers = rayon::current_num_threads().saturating_sub(1).min(MAX_HELPERS);
        if helpers == 0 {
            return None;
        }
        // A pool that cannot start leaves the work to the calling thread, which gives the same.
        ThreadPoolBuilder::new()
            .num_threads(helpers)
            .thread_name(|index| format!("sheltersim-tick-{index}"))
            .build()
            .ok()
    })
    .as_ref()
}

/// The pool and how many of its threads help with `count` people, or None for the calling thread
/// alone. The census and the draw log count what this thread does, in its order, so with either on
/// the work stays here.
fn helpers_for(count: usize) -> Option<(&'static ThreadPool, usize)> {
    if census::is_on() || rng::draw_log_is_on() {
        return None;
    }
    let pool = pool()?;
    let helpers = (count / per_helper()).min(pool.current_num_threads());
    (helpers > 0).then_some((pool, helpers))
}

/// `part(0..parts)`, by the calling thread and `helpers` threads of `pool`, each part once, in order.
fn run_parts<R, F>(pool: &ThreadPool, helpers: usize, parts: usize, part: F) -> Vec<R>
where
    R: Send,
    F: Fn(usize) -> R + Sync,
{
    let next = AtomicUsize::new(0);
    let slots: Vec<Mutex<Option<R>>> = (0..parts).map(|_| Mutex::new(None)).collect();
    let take_parts = || loop {
        let index = next.fetch_add(1, Ordering::Relaxed);
        if index >= parts {
            return;
        }
        let result = part(index);
        *slots[index].lock().unwrap_or_else(PoisonError::into_inner) = Some(result);
    };
    pool.in_place_scope(|scope| {
        for _ in 0..helpers {
            scope.spawn(|_| take_parts());
        }
        take_parts();
    });
    slots
        .into_iter()
        .map(|slot| {
            slot.into_inner()
                .unwrap_or_else(PoisonError::into_inner)
                .expect("every part ran before the scope ended")
        })
        .collect()
}

/// `work(0..count)`, each on any thread, in the order of the indexes.
pub fn in_order<R, F>(count: usize, work: F) -> Vec<R>
where
    R: Send,
    F: Fn(usize) -> R + Sync,
{
    let Some((pool, helpers)) = helpers_for(count) else {
        return (0..count).map(work).collect();
    };
    let per_part = PEOPLE_PER_PART.min(per_helper());
    let parts = run_parts(pool, helpers, count.div_ceil(per_part), |index| {
        let start = index * per_part;
        (start..count.min(start + per_part)).map(&work).collect::<Vec<R>>()
    });
    parts.into_iter().flatten().collect()
}

/// `work` of each run of at most `per_part` indexes of `0..count`, each on any thread, in order.
pub fn parts_in_order<R, F>(count: usize, per_part: usize, work: F) -> Vec<R>
where
    R: Send,
    F: Fn(Range<usize>) -> R + Sync,
{
    let per_part = per_part.min(per_helper()).max(1);
    let part = |index: usize| work(index * per_part..count.min((index + 1) * per_part));
    let parts = count.div_ceil(per_part);
    match helpers_for(count) {
        Some((pool, helpers)) if parts > 1 => run_parts(pool, helpers, parts, part),
        _ => (0..parts).map(part).collect(),
    }
}

/// For the tests and the gate: one helper for each `people` people from now on (at least 1), and
/// the number before. A small number sends a small Scenario's compute to the helpers, in runs of
/// at most that many people.
pub fn set_people_per_helper(people: usize) -> usize {
    PER_HELPER.swap(people.max(1), Ordering::Relaxed)
}

/// For the tests: `(helper threads, helpers for this many people)`.
pub fn parallel_state(people: usize) -> (usize, usize) {
    let threads = pool().map_or(0, ThreadPool::current_num_threads);
    (threads, helpers_for(people).map_or(0, |(_, helpers)| helpers))
}
