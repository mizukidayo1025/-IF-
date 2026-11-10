//! Where each `Unsupported` was raised and who caught it, for benchmarks/rule_shadow/refusal_census.py.
//!
//! Off until that tool turns it on. Nothing here runs unless a kernel refuses, and then only a load
//! of one flag on the way to building the exception, so a run without the tool pays nothing a refusal
//! didn't already pay. The tool reads the environment; the kernels never do.

use std::collections::BTreeMap;
use std::panic::Location;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;


type Site = (&'static str, u32);

static ON: AtomicBool = AtomicBool::new(false);
static COUNTS: Mutex<Counts> = Mutex::new(Counts {
    raised: BTreeMap::new(),
    caught: BTreeMap::new(),
    last: None,
    moved: BTreeMap::new(),
});

struct Counts {
    raised: BTreeMap<(Site, String), u64>,
    /// By `(who caught it, the site if it was the last raised, reason)`.
    caught: BTreeMap<(String, Option<Site>, String), u64>,
    last: Option<(Site, String)>,
    /// The errors of `core/errors.rs` raised, by site and text.
    moved: BTreeMap<(Site, String), u64>,
}

/// A refusal being built at the caller's line.
#[cold]
#[inline(never)]
#[track_caller]
pub fn raised(reason: &str) {
    if !ON.load(Ordering::Relaxed) {
        return;
    }
    let location = Location::caller();
    let site = (location.file(), location.line());
    if let Ok(mut counts) = COUNTS.lock() {
        *counts.raised.entry((site, reason.to_owned())).or_default() += 1;
        counts.last = Some((site, reason.to_owned()));
    }
}

/// An error of `core/errors.rs` being built at the caller's caller's line.
#[cold]
#[inline(never)]
#[track_caller]
pub fn moved(text: &str) {
    if !ON.load(Ordering::Relaxed) {
        return;
    }
    let location = Location::caller();
    if let Ok(mut counts) = COUNTS.lock() {
        *counts.moved.entry(((location.file(), location.line()), text.to_owned())).or_default() += 1;
    }
}

fn note_caught(by: &str, reason: String) {
    if let Ok(mut counts) = COUNTS.lock() {
        let site = counts.last.as_ref().filter(|(_, last)| *last == reason).map(|(site, _)| *site);
        *counts.caught.entry((by.to_owned(), site, reason)).or_default() += 1;
    }
}

/// A refusal caught in Rust, with the text of its exception: the driver's exit of a kernel's
/// refusal (`rules/tick.rs`).
#[cold]
#[inline(never)]
pub fn caught(by: &str, reason: String) {
    if ON.load(Ordering::Relaxed) {
        note_caught(by, reason);
    }
}

pub fn is_on() -> bool {
    ON.load(Ordering::Relaxed)
}

/// Turn the census on or off for this process.
pub fn switch(on: bool) {
    ON.store(on, Ordering::Relaxed);
}

/// A refusal that reached a Python entry of a kernel.
pub fn caught_at_entry(by: &str, reason: String) {
    note_caught(by, reason);
}

/// Rows of `(file, line, reason, n)`.
pub type RaisedRows = Vec<(&'static str, u32, String, u64)>;
/// Rows of `(by, file, line, reason, n)`.
pub type CaughtRows = Vec<(String, &'static str, u32, String, u64)>;

/// Take what was counted: `(raised, caught)`, and start again. The moved errors wait for
/// `take_errors`.
pub fn take() -> (RaisedRows, CaughtRows) {
    let (raised, caught) = match COUNTS.lock() {
        Ok(mut counts) => (std::mem::take(&mut counts.raised), std::mem::take(&mut counts.caught)),
        Err(_) => Default::default(),
    };
    let raised_rows = raised.into_iter().map(|(((file, line), reason), n)| (file, line, reason, n)).collect();
    let caught_rows = caught
        .into_iter()
        .map(|((by, site, reason), n)| {
            let (file, line) = site.unwrap_or(("", 0));
            (by, file, line, reason, n)
        })
        .collect();
    (raised_rows, caught_rows)
}

/// Take the moved errors, rows of `(file, line, text, n)`, and start again.
pub fn take_errors() -> RaisedRows {
    let moved = match COUNTS.lock() {
        Ok(mut counts) => std::mem::take(&mut counts.moved),
        Err(_) => Default::default(),
    };
    moved.into_iter().map(|(((file, line), text), n)| (file, line, text, n)).collect()
}
