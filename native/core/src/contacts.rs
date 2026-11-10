//! What the per-tick contact models share: sampling keyed partners and the keys of pairs.
//! The models are in `rules/social_propagation.rs` and `rules/information.rs` of the PyO3
//! crate, which also reads the exact Python numbers (`native/src/contacts.rs`). Each returns the
//! same floats, draws and JSON keys as the Python code: every float operation keeps its order.

use std::collections::HashMap;

use crate::error::{Error, Result};
use crate::json::write_json_string;
use crate::rng::{Key, KeyedDraws};

#[derive(Clone, Copy)]
pub enum Basis {
    Family,
    SameZone,
    Nearby,
}

pub struct Bucket<'a> {
    pub basis: Basis,
    pub members: &'a [usize],
    pub includes_self: bool,
}

impl Bucket<'_> {
    fn eligible(&self) -> i64 {
        self.members.len() as i64 - i64::from(self.includes_self)
    }
}

/// Sorted person indexes of each zone (people are indexed in ID order).
pub fn zone_members<'a>(zones: &[&'a str]) -> HashMap<&'a str, Vec<usize>> {
    let mut members: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, &zone) in zones.iter().enumerate() {
        members.entry(zone).or_default().push(index);
    }
    members
}

/// The adjacent zones of `zone` that have people, in sorted order.
pub fn nearby_buckets<'a>(
    zone: &str,
    neighbors: &'a HashMap<String, Vec<String>>,
    members: &'a HashMap<&str, Vec<usize>>,
) -> Vec<&'a [usize]> {
    let mut names: Vec<&str> = neighbors
        .get(zone)
        .map(|names| names.iter().map(String::as_str).collect())
        .unwrap_or_default();
    names.sort_unstable();
    names
        .into_iter()
        .filter_map(|name| members.get(name))
        .filter(|people| !people.is_empty())
        .map(Vec::as_slice)
        .collect()
}

/// `_select_contacts`: at most `maximum` partners of `me`, with the draws of
/// `{key}` written as `["<label>",minute,"<me>",slot,attempt]`. The social model
/// stops at a slot that used all its attempts; the information model goes on.
pub fn select_contacts(
    draws: &KeyedDraws,
    key: &mut Key,
    me: usize,
    buckets: &[Bucket<'_>],
    maximum: i64,
    stop_at_exhausted_slot: bool,
    selected: &mut Vec<(usize, Basis)>,
) {
    selected.clear();
    if maximum == 0 || buckets.is_empty() {
        return;
    }
    let total: i64 = buckets.iter().map(Bucket::eligible).sum();
    if total <= 0 {
        return;
    }
    let head = key.mark();
    for slot in 0..maximum {
        let mut exhausted = true;
        for attempt in 0..maximum * 4 {
            key.rewind(head);
            let draw = key.int(slot).int(attempt).draw(draws);
            let mut offset = (total - 1).min((draw * total as f64) as i64);
            let mut chosen = None;
            for bucket in buckets {
                if offset < bucket.eligible() {
                    chosen = Some(bucket);
                    break;
                }
                offset -= bucket.eligible();
            }
            let Some(bucket) = chosen else {
                exhausted = false;
                break;
            };
            let mut local = offset as usize;
            if bucket.includes_self && local >= bucket.members.partition_point(|&other| other < me)
            {
                local += 1;
            }
            let source = bucket.members[local];
            if source == me || selected.iter().any(|&(other, _)| other == source) {
                continue;
            }
            selected.push((source, bucket.basis));
            exhausted = false;
            break;
        }
        if exhausted && stop_at_exhausted_slot {
            break;
        }
    }
}

/// `"<id>"` as JSON for each person.
pub fn quoted_ids(ids: &[&str]) -> Vec<Vec<u8>> {
    ids.iter()
        .map(|id| {
            let mut quoted = Vec::with_capacity(id.len() + 2);
            write_json_string(&mut quoted, id);
            quoted
        })
        .collect()
}

/// `[first,second]`; Unsupported for bytes that are not UTF-8.
pub fn pair_key(first: &[u8], second: &[u8]) -> Result<String> {
    let mut key = String::with_capacity(first.len() + second.len() + 3);
    key.push('[');
    key.push_str(std::str::from_utf8(first).map_err(|_| Error::unsupported("not UTF-8"))?);
    key.push(',');
    key.push_str(std::str::from_utf8(second).map_err(|_| Error::unsupported("not UTF-8"))?);
    key.push(']');
    Ok(key)
}

pub fn key_head(label: &str, minute: i64, quoted: &[u8]) -> Key {
    let mut head = Key::labelled(label);
    head.int(minute).quoted(quoted);
    head
}
