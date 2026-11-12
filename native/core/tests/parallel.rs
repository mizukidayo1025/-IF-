//! The work the core splits over threads gives the same bytes on the helper threads (the default
//! feature `parallel`) as on the calling thread alone (`--no-default-features`, the wasm32 build).
//!
//! CI runs this file in both builds. Each build compares its own split with a plain loop over the
//! same input, and both compare their bytes with the same frozen digest, so the two builds agree.
//! One test changes the global people per helper, so this file holds one test.

use std::ops::Range;

use rustc_hash::FxHashSet;
use sha2::{Digest, Sha256};
use sheltersim_core::checkpoint::{candidates, chunk_hash, chunk_hashes, hex, sensitive_candidates, Candidates};
use sheltersim_core::parallel::{in_order, parallel_state, parts_in_order, set_people_per_helper};

const PEOPLE: usize = 1000;
/// Small enough that the parallel build sends 1,000 people to its helpers.
const PER_HELPER: usize = 8;
const PER_PART: usize = 64;

/// The digest of everything below, frozen from the build with the `parallel` feature.
const FROZEN: &str = "97d12c344c4ac193714d4edc40325b7b014553ca1ef93db157979cd205f5704a";

/// A person's compute: bytes that depend on the index alone.
fn person(index: usize) -> Vec<u8> {
    Sha256::digest(format!("person-{index}").as_bytes()).to_vec()
}

/// A part's compute: an order-dependent fold over its people, and its range.
fn part(run: Range<usize>) -> (Range<usize>, f64) {
    let mut sum = 0.0;
    for index in run.clone() {
        sum = sum * 0.5 + (index as f64).sqrt();
    }
    (run, sum)
}

fn json_texts() -> Vec<String> {
    (0..40)
        .map(|index| {
            format!(
                r#"{{"k{index}":"plain-{index}","mail":"user{index}@example.org","tok":"sk-{index:0>14}","phone":"03-{index:0>4}-0000","list":["x","Bearer y{}"]}}"#,
                index % 7
            )
        })
        .collect()
}

fn sorted(set: FxHashSet<String>) -> Vec<String> {
    let mut items: Vec<String> = set.into_iter().collect();
    items.sort();
    items
}

#[test]
fn the_split_work_gives_the_bytes_of_a_plain_loop() {
    let before = set_people_per_helper(PER_HELPER);
    let mut digest = Sha256::new();

    // The people, in their order.
    let people = in_order(PEOPLE, person);
    assert_eq!(people, (0..PEOPLE).map(person).collect::<Vec<_>>());
    for bytes in &people {
        digest.update(bytes);
    }

    // The parts: the same runs, in order, each folded in its order.
    let parts = parts_in_order(PEOPLE, PER_PART, part);
    let per_part = PER_PART.min(PER_HELPER);
    let plain: Vec<_> = (0..PEOPLE.div_ceil(per_part))
        .map(|index| part(index * per_part..PEOPLE.min((index + 1) * per_part)))
        .collect();
    assert_eq!(parts, plain);
    for (run, sum) in &parts {
        digest.update(format!("{}..{}:", run.start, run.end).as_bytes());
        digest.update(sum.to_bits().to_le_bytes());
    }

    // The Checkpoint kernels: the chunks' hashes in the chunks' order.
    let texts = json_texts();
    let raws: Vec<&[u8]> = texts.iter().map(String::as_bytes).collect();
    let chunks: Vec<Vec<&[u8]>> = raws.chunks(3).map(<[&[u8]]>::to_vec).collect();
    let hashes = chunk_hashes(&chunks);
    assert_eq!(hashes, chunks.iter().map(|chunk| chunk_hash(chunk)).collect::<Vec<_>>());
    for hash in &hashes {
        digest.update(hash.as_bytes());
    }

    // The privacy candidates: the same sets as one text after another.
    let (keys, strings) = sensitive_candidates(&raws).unwrap();
    let mut one_by_one = Candidates::default();
    for raw in &raws {
        let (more_keys, more_strings) = candidates(raw).unwrap();
        one_by_one.0.extend(more_keys);
        one_by_one.1.extend(more_strings);
    }
    let (keys, strings) = (sorted(keys), sorted(strings));
    assert_eq!(keys, sorted(one_by_one.0));
    assert_eq!(strings, sorted(one_by_one.1));
    assert!(!strings.is_empty());
    for text in keys.iter().chain(&strings) {
        digest.update(text.as_bytes());
        digest.update(b"\n");
    }

    // A text that can not be read: the same error.
    let mut broken = raws.clone();
    broken.insert(5, br#"{"a":"unterminated"#);
    assert_eq!(sensitive_candidates(&broken).unwrap_err(), "unterminated JSON string");

    // The parallel build did use its helpers (unless this machine has one core); the other has none.
    let (threads, helpers) = parallel_state(PEOPLE);
    if cfg!(feature = "parallel") {
        assert_eq!(helpers, threads.min(PEOPLE / PER_HELPER));
    } else {
        assert_eq!((threads, helpers), (0, 0));
    }

    // With the default of one helper for 256 people, 255 stay on the calling thread and 1,000 take
    // at most the pool's three helpers (`test_few_people_stay_on_the_calling_thread` of
    // `tests/integration/test_parallel_tick.py` before #225).
    set_people_per_helper(256);
    let (threads, _) = parallel_state(0);
    assert_eq!(parallel_state(255).1, 0);
    assert_eq!(parallel_state(1000).1, threads.min(3));

    set_people_per_helper(before);
    assert_eq!(hex(&digest.finalize()), FROZEN);
}
