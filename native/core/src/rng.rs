//! `CounterRNG` (`core/rng.py`). Every draw's key is written by `Key` and starts with one of
//! the labels in `label`.
//!
//! A key holds the rule's label, the minute and stable domain IDs (people, messages, sources,
//! facilities, queues), never an ID made from a counter: an event's ID holds its sequence
//! number, which depends on the order the events were made in. Model version 2 took the event
//! IDs out of the information events' keys (docs/rust-port/step4-plan.md, D5). SHA-256 and the
//! seed's `str(seed)` are as `CounterRNG` has them, which the Scenario generators still use.

use std::cell::RefCell;
use std::sync::atomic::{AtomicUsize, Ordering};

use sha2::{Digest, Sha256};

use crate::json::write_json_string;

/// How many threads keep their draws, for the tests and benchmarks/rule_shadow. Python sees the
/// draws its own rules make; this shows those made in Rust, to compare each with its key and its
/// bits. While a thread keeps them, the per-person compute stays on it (core/parallel.rs).
static DRAW_LOGS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static DRAW_LOG: RefCell<Option<Vec<(String, f64)>>> = const { RefCell::new(None) };
}

pub fn draw_log_is_on() -> bool {
    DRAW_LOGS.load(Ordering::Relaxed) != 0
}

/// Keep the draws this thread makes from now on, until `take_draw_log`.
pub fn start_draw_log() {
    let before = DRAW_LOG.with(|log| log.borrow_mut().replace(Vec::new()));
    if before.is_none() {
        DRAW_LOGS.fetch_add(1, Ordering::Relaxed);
    }
}

/// The (key as JSON, value) of each draw this thread made since `start_draw_log`, in order.
pub fn take_draw_log() -> Vec<(String, f64)> {
    let taken = DRAW_LOG.with(|log| log.borrow_mut().take());
    if taken.is_some() {
        DRAW_LOGS.fetch_sub(1, Ordering::Relaxed);
    }
    taken.unwrap_or_default()
}

fn log_draw(key: &[u8], value: f64) {
    DRAW_LOG.with(|log| {
        if let Some(log) = log.borrow_mut().as_mut() {
            log.push((String::from_utf8_lossy(key).into_owned(), value));
        }
    });
}

/// The first part of each draw's key, one per kind of draw.
pub mod label {
    /// `propose`: the action a person chooses.
    pub const BEHAVIOR: &str = "behavior";
    /// The minutes a toilet visit takes.
    pub const TOILET_SERVICE: &str = "toilet-service";
    /// Whether a person forgets a message.
    pub const INFORMATION_FORGETTING: &str = "information-forgetting-v1";
    /// Whom a person talks to about information.
    pub const INFORMATION_CONTACT_PARTNER: &str = "information-contact-partner-v1";
    /// Whether a contact passes a message on.
    pub const INFORMATION_CONTACT_TRANSFER: &str = "information-contact-transfer-v1";
    /// Whom a person meets for the social propagation.
    pub const SOCIAL_CONTACT_BUCKET: &str = "social-contact-bucket-v1";
    /// Whether a person believes a claim they hear.
    pub const RUMOR_BELIEF: &str = "rumor-belief-acceptance-v1";
    /// Whether a person accepts an announced correction.
    pub const CORRECTION_ACCEPTANCE: &str = "correction-acceptance-v1";
    /// Whether a person accepts a correction another person tells them.
    pub const CONTACT_CORRECTION_ACCEPTANCE: &str = "contact-correction-acceptance-v1";
}

/// `CounterRNG.random`: SHA-256 of the seed and the JSON key, the first eight bytes as a
/// fraction of 2**64.
pub struct KeyedDraws {
    seeded: Sha256,
}

impl KeyedDraws {
    /// `seed` is `str(seed)` of a non-negative int.
    pub fn new(seed: &str) -> Self {
        let mut seeded = Sha256::new();
        seeded.update(b"sheltersim:sha256-keyed-v1\0");
        seeded.update(seed.as_bytes());
        seeded.update(b"\0");
        Self { seeded }
    }

    fn draw(&self, key: &[u8]) -> f64 {
        let digest = self.seeded.clone().chain_update(key).finalize();
        let mut first = [0u8; 8];
        first.copy_from_slice(&digest[..8]);
        // u64 to f64 and int / int both round to nearest, ties to even, and the division by a
        // power of two is exact.
        let value = u64::from_be_bytes(first) as f64 / 18_446_744_073_709_551_616.0;
        if draw_log_is_on() {
            log_draw(key, value);
        }
        value
    }
}

/// The key parts of a draw, written as `json.dumps(list(parts), ensure_ascii=False,
/// separators=(",", ":"))`. Rules that draw in a loop keep a `mark` of the common parts and
/// `rewind` to it, which avoids writing them again.
#[derive(Clone)]
pub struct Key {
    bytes: Vec<u8>,
}

impl Key {
    pub fn new() -> Self {
        let mut bytes = Vec::with_capacity(64);
        bytes.push(b'[');
        Self { bytes }
    }

    /// A key that starts with the rule's label.
    pub fn labelled(label: &str) -> Self {
        let mut key = Self::new();
        key.text(label);
        key
    }

    fn separate(&mut self) {
        if self.bytes.len() > 1 {
            self.bytes.push(b',');
        }
    }

    pub fn text(&mut self, part: &str) -> &mut Self {
        self.separate();
        write_json_string(&mut self.bytes, part);
        self
    }

    /// A part already written as a JSON string, kept per person by the callers.
    pub fn quoted(&mut self, part: &[u8]) -> &mut Self {
        self.separate();
        self.bytes.extend_from_slice(part);
        self
    }

    pub fn int(&mut self, part: i64) -> &mut Self {
        self.separate();
        self.bytes
            .extend_from_slice(itoa::Buffer::new().format(part).as_bytes());
        self
    }

    pub fn mark(&self) -> usize {
        self.bytes.len()
    }

    pub fn rewind(&mut self, mark: usize) {
        self.bytes.truncate(mark);
    }

    /// The draw for these parts. The key is unchanged, so more parts can follow.
    pub fn draw(&mut self, draws: &KeyedDraws) -> f64 {
        self.bytes.push(b']');
        let value = draws.draw(&self.bytes);
        self.bytes.pop();
        value
    }
}
