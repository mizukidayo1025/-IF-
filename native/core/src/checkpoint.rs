//! Checkpoint save kernels over bytes (docs/rust-port.md, stage B3), without Python: the
//! privacy check's candidates, the content-defined chunk starts, the chunk hashes, the length
//! `json.dumps` gives, and the items of a saved JSON array. The PyO3 crate's `checkpoint.rs` reads
//! the Python bytes and calls these, in parallel without the GIL where it did.
//!
//! `candidates` reads a chunk's canonical JSON bytes and returns what the privacy check of
//! `ProjectStore` must look at: the distinct object keys, and the distinct string values that its
//! patterns could match. The patterns stay in Python; a string left out here can not match them,
//! for these reasons:
//!
//! - `_SECRET`'s address alternative needs "@".
//! - `sk-(?:ant-)?[A-Za-z0-9_-]{12,}` needs "sk-" and 12 more such characters.
//! - `Bearer\s+\S+` needs "bearer", in any case.
//! - `PHONE_NUMBER` needs two separators, each between digits.
//!
//! The patterns ignore case, under which U+017F matches "s" and U+212A matches "k", and `\d`
//! matches every Unicode decimal digit; a string with one of those is returned whatever else it
//! holds (`char::is_numeric` covers more than `\d`).

use rayon::prelude::*;
use rustc_hash::FxHashSet;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// The keys and the strings of `candidates`.
pub type Candidates = (FxHashSet<String>, FxHashSet<String>);

const SK_RUN: usize = 12;

fn token_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'
}

fn starts_ignoring_case(bytes: &[u8], lower: &[u8]) -> bool {
    bytes.len() >= lower.len() && bytes[..lower.len()].eq_ignore_ascii_case(lower)
}

/// Whether the patterns could match text, the UTF-8 of one decoded JSON string.
fn could_match(text: &[u8]) -> bool {
    let mut separators = 0;
    for (index, &byte) in text.iter().enumerate() {
        match byte {
            b'@' => return true,
            b's' | b'S' if starts_ignoring_case(&text[index..], b"sk-") => {
                let run = text[index + 3..].iter().take(SK_RUN).take_while(|b| token_char(**b));
                if run.count() == SK_RUN {
                    return true;
                }
            }
            b'b' | b'B' if starts_ignoring_case(&text[index..], b"bearer") => return true,
            b'-' | b' ' => {
                let digit_before = index > 0 && text[index - 1].is_ascii_digit();
                let digit_after = text.get(index + 1).is_some_and(u8::is_ascii_digit);
                if digit_before && digit_after {
                    separators += 1;
                    if separators == 2 {
                        return true;
                    }
                }
            }
            _ => {}
        }
    }
    if text.is_ascii() {
        return false;
    }
    match std::str::from_utf8(text) {
        Ok(decoded) => decoded
            .chars()
            .any(|c| c == '\u{17f}' || c == '\u{212a}' || (!c.is_ascii() && c.is_numeric())),
        Err(_) => true,
    }
}

fn decode(raw: &[u8]) -> std::result::Result<String, String> {
    serde_json::from_slice(raw).map_err(|error| error.to_string())
}

/// decode of a quoted string with no escape or control character in it.
fn decode_plain(quoted: &[u8]) -> std::result::Result<String, String> {
    std::str::from_utf8(&quoted[1..quoted.len() - 1])
        .map(str::to_owned)
        .map_err(|error| error.to_string())
}

/// The distinct keys and the distinct string values to check in raw, one JSON text.
pub fn candidates(raw: &[u8]) -> std::result::Result<Candidates, String> {
    let mut keys: FxHashSet<String> = FxHashSet::default();
    let mut strings: FxHashSet<String> = FxHashSet::default();
    let mut seen_keys: FxHashSet<&[u8]> = FxHashSet::default();
    let mut seen_strings: FxHashSet<&[u8]> = FxHashSet::default();
    let mut index = 0;
    let length = raw.len();
    while index < length {
        if raw[index] != b'"' {
            index += 1;
            continue;
        }
        let start = index;
        index += 1;
        let mut escaped = false;
        while index < length && raw[index] != b'"' {
            if raw[index] == b'\\' {
                escaped = true;
                index += 1;
            } else if raw[index] < 0x20 {
                // serde_json says what is wrong with it.
                escaped = true;
            }
            index += 1;
        }
        if index >= length {
            return Err("unterminated JSON string".to_owned());
        }
        index += 1;
        let quoted = &raw[start..index];
        // In JSON only an object key is followed by ":".
        if raw.get(index) == Some(&b':') {
            if seen_keys.insert(quoted) {
                keys.insert(if escaped { decode(quoted)? } else { decode_plain(quoted)? });
            }
        } else if seen_strings.insert(quoted) {
            if escaped {
                let text = decode(quoted)?;
                if could_match(text.as_bytes()) {
                    strings.insert(text);
                }
            } else if could_match(&quoted[1..quoted.len() - 1]) {
                strings.insert(decode_plain(quoted)?);
            }
        }
    }
    Ok((keys, strings))
}

/// The `candidates` of several JSON texts together, read in parallel.
pub fn sensitive_candidates(raws: &[&[u8]]) -> std::result::Result<Candidates, String> {
    raws.par_iter().map(|raw| candidates(raw)).try_reduce(
        || (FxHashSet::default(), FxHashSet::default()),
        |(mut keys, mut strings), (more_keys, more_strings)| {
            keys.extend(more_keys);
            strings.extend(more_strings);
            Ok((keys, strings))
        },
    )
}

/// A 64-bit hash of one item's bytes that decides content-defined chunk ends. It is part of
/// how the archive lays out chunks, not of any Checkpoint hash: eight bytes at a time,
/// little-endian, then the length, then a final mix (splitmix64), the same on every OS.
fn item_cut_hash(item: &[u8]) -> u64 {
    let mut hash: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut words = item.chunks_exact(8);
    for word in &mut words {
        let value = u64::from_le_bytes(word.try_into().expect("eight bytes"));
        hash = (hash.rotate_left(5) ^ value).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
    let mut tail = [0u8; 8];
    let rest = words.remainder();
    tail[..rest.len()].copy_from_slice(rest);
    hash = (hash.rotate_left(5) ^ u64::from_le_bytes(tail)).wrapping_mul(0x517c_c1b7_2722_0a95);
    hash ^= item.len() as u64;
    hash = (hash ^ (hash >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    hash = (hash ^ (hash >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    hash ^ (hash >> 31)
}

/// `ValueError` unless both chunk sizes of `content_chunk_starts` are positive.
pub fn chunk_sizes_valid(average: u64, maximum: usize) -> Result<()> {
    if average == 0 || maximum == 0 {
        return Err(Error::Value("chunk sizes must be positive".into()));
    }
    Ok(())
}

/// Where each chunk of items starts when a chunk ends after an item whose hash is
/// `average - 1` modulo `average`, or once it holds `maximum` items. The ends depend only
/// on the items themselves, so dropping items from the front or putting one in the middle
/// moves only the chunks around the change (#138).
pub fn content_chunk_starts(items: &[&[u8]], average: u64, maximum: usize) -> Result<Vec<usize>> {
    chunk_sizes_valid(average, maximum)?;
    let mut starts = Vec::new();
    let mut size = 0;
    for (index, item) in items.iter().enumerate() {
        if size == 0 {
            starts.push(index);
        }
        size += 1;
        if size == maximum || item_cut_hash(item) % average == average - 1 {
            size = 0;
        }
    }
    Ok(starts)
}

/// The SHA-256 hex digest of `[item,item,...]` for each chunk of items, computed in parallel
/// without joining the items.
pub fn chunk_hashes(chunks: &[Vec<&[u8]>]) -> Vec<String> {
    chunks
        .par_iter()
        .map(|chunk| {
            let mut hasher = Sha256::new();
            hasher.update(b"[");
            for (index, item) in chunk.iter().enumerate() {
                if index > 0 {
                    hasher.update(b",");
                }
                hasher.update(item);
            }
            hasher.update(b"]");
            hex(&hasher.finalize())
        })
        .collect()
}

/// Lowercase hex digits of bytes, as `hashlib`'s `hexdigest` writes them.
pub fn hex(digest: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(digest.len() * 2);
    for byte in digest {
        text.push(DIGITS[usize::from(byte >> 4)] as char);
        text.push(DIGITS[usize::from(byte & 15)] as char);
    }
    text
}

/// `hashlib.sha256(raw).hexdigest()`.
pub fn sha256_hex(raw: &[u8]) -> String {
    hex(&Sha256::digest(raw))
}

/// `len(json.dumps(value, ensure_ascii=False).encode())` of a value whose canonical JSON is `raw`:
/// one more byte for each separator, which only json.dumps' default separators have a space in.
pub fn dumps_length(raw: &[u8]) -> usize {
    let mut length = raw.len();
    let mut in_text = false;
    let mut index = 0;
    while index < raw.len() {
        match raw[index] {
            b'\\' if in_text => index += 1,
            b'"' => in_text = !in_text,
            b',' | b':' if !in_text => length += 1,
            _ => {}
        }
        index += 1;
    }
    length
}

/// The bytes of each item of a JSON array, split where JSON does: at commas outside strings and
/// nested values. raw must be one canonical JSON array; `ValueError` otherwise.
pub fn split_json_array(raw: &[u8]) -> Result<Vec<&[u8]>> {
    let bad = || Error::Value("not a JSON array".into());
    let length = raw.len();
    if length < 2 || raw[0] != b'[' || raw[length - 1] != b']' {
        return Err(bad());
    }
    let mut items = Vec::new();
    let (mut depth, mut in_string, mut escaped) = (0usize, false, false);
    let mut start = 1;
    for (index, &byte) in raw.iter().enumerate().take(length - 1).skip(1) {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'[' | b'{' => depth += 1,
            b']' | b'}' => depth = depth.checked_sub(1).ok_or_else(bad)?,
            b',' if depth == 0 => {
                items.push(&raw[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    if in_string || depth != 0 {
        return Err(bad());
    }
    if start < length - 1 {
        items.push(&raw[start..length - 1]);
    }
    Ok(items)
}
