//! What a new Run of a Scenario records besides its world (#301): its `model_versions`
//! (`model_signatures(scenario.model_configs)`) and the evidence route's answer
//! (`evidence_context(scenario)` with the catalogs `web/project_services.py` adds, in a Project
//! without datasets). Both come from the table's templates of the Scenario's fingerprint, with what
//! depends on the Scenario's own data filled in: the signature of the floor plan's service points,
//! and the Scenario's sources, assumptions and research limits as `safe_document` keeps them.
//!
//! `safe_document` redacts what looks like a secret, an e-mail address or a phone number in a
//! string and refuses a key that names private data. The core does not redact: a string where
//! Python's patterns could match, or such a key, is `Unsupported` (the Scenario screen's builder
//! writes neither).

use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::Value;

use super::{bad, canonical, entry_of, map_of, text, vmap};
use crate::checkpoint::sha256_hex;
use crate::error::{Error, Result};
use crate::json::write_float;
use crate::text::casefold;

/// A superset of `\\d` (Unicode decimal digits): any numeric character.
fn re_decimal(char: char) -> bool {
    char.is_numeric()
}
use crate::value::V;

/// The signature of the floor plan's service points (`simulator_signatures`).
const SERVICE_POINTS: &str = "operator:facility_service_points";

/// `json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False)`, with the default
/// `ensure_ascii=True`: every character past ASCII as `\uXXXX`.
fn ascii_json(value: &V, out: &mut Vec<u8>) -> Result<()> {
    match value {
        V::Null => out.extend_from_slice(b"null"),
        V::Bool(flag) => out.extend_from_slice(if *flag { b"true" } else { b"false" }),
        V::Int(number) => out.extend_from_slice(number.to_string().as_bytes()),
        V::Big(digits) => out.extend_from_slice(digits.as_bytes()),
        V::Float(number) => write_float(out, *number).map_err(|_| Error::unsupported("NaN and infinity are not JSON"))?,
        V::Str(text) => ascii_string(text, out),
        V::List(items) => {
            out.push(b'[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                ascii_json(item, out)?;
            }
            out.push(b']');
        }
        V::Map(map) => {
            let mut entries: Vec<(&Arc<str>, &V)> = map.iter().collect();
            entries.sort_by(|first, second| first.0.as_bytes().cmp(second.0.as_bytes()));
            out.push(b'{');
            for (index, (key, item)) in entries.into_iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                ascii_string(key, out);
                out.push(b':');
                ascii_json(item, out)?;
            }
            out.push(b'}');
        }
    }
    Ok(())
}

fn ascii_string(text: &str, out: &mut Vec<u8>) {
    out.push(b'"');
    for char in text.chars() {
        match char {
            '"' => out.extend_from_slice(b"\\\""),
            '\\' => out.extend_from_slice(b"\\\\"),
            '\n' => out.extend_from_slice(b"\\n"),
            '\r' => out.extend_from_slice(b"\\r"),
            '\t' => out.extend_from_slice(b"\\t"),
            '\u{8}' => out.extend_from_slice(b"\\b"),
            '\u{c}' => out.extend_from_slice(b"\\f"),
            ' '..='~' => out.push(char as u8),
            _ => {
                let mut units = [0u16; 2];
                for unit in char.encode_utf16(&mut units) {
                    out.extend_from_slice(format!("\\u{unit:04x}").as_bytes());
                }
            }
        }
    }
    out.push(b'"');
}

/// The Scenario's configs and provenance.
fn read(scenario: &[u8]) -> Result<(IndexMap<Arc<str>, V>, V)> {
    let json: Value = serde_json::from_slice(scenario).map_err(bad)?;
    if !json.is_object() {
        return Err(bad("the document is not a dict"));
    }
    let configs = map_of(&json["model_configs"], "model_configs")?;
    let provenance = V::Map(map_of(&json["provenance"], "provenance")?);
    Ok((configs, provenance))
}

fn signatures(configs: &IndexMap<Arc<str>, V>, template: &IndexMap<Arc<str>, V>) -> Result<IndexMap<Arc<str>, V>> {
    let mut signatures = template.clone();
    if let Some(V::Map(geometry)) = configs.get("facility_geometry") {
        if let Some(points) = geometry.get("service_points") {
            if points.is_truthy() {
                let mut out = Vec::new();
                ascii_json(points, &mut out)?;
                signatures.insert(text(SERVICE_POINTS), V::Str(Arc::from(format!("facility-service-points@1:{}", sha256_hex(&out)).as_str())));
            }
        }
    }
    Ok(signatures)
}

/// `model_signatures(scenario.model_configs)` of a Scenario's canonical JSON, in the table's
/// order with the service points' signature last (Python's order differs; a dict's order is not
/// written to the records).
pub fn model_versions(scenario: &[u8]) -> Result<V> {
    let (configs, _) = read(scenario)?;
    let entry = entry_of(&configs)?;
    Ok(V::Map(signatures(&configs, &entry.model_signatures)?))
}

// ---- safe_document --------------------------------------------------------------------------------

/// Whether `_PRIVATE` (`api.?key|secret|password|credential|authorization|raw.?prompt|
/// full.?name|email|phone|address`, ignoring case) could match a key: one of its words, or both
/// words of a pair, in the folded key (`ı` read as `i`, as `re.IGNORECASE` matches it).
fn private_key(key: &str) -> bool {
    let folded = casefold(key).replace('ı', "i");
    let both = |first: &str, second: &str| folded.contains(first) && folded.contains(second);
    ["secret", "password", "credential", "authorization", "email", "phone", "address"].iter().any(|word| folded.contains(word))
        || both("api", "key")
        || both("raw", "prompt")
        || both("full", "name")
}

/// `re.IGNORECASE` of an ASCII letter (lower case) against a character, with Python's extras.
fn ci(char: char, letter: char) -> bool {
    (char.is_ascii() && char.to_ascii_lowercase() == letter)
        || matches!((letter, char), ('i', '\u{130}' | '\u{131}') | ('k', '\u{212a}') | ('s', '\u{17f}'))
}

/// Whether `_redact` could change the text: an `@`; `sk-` with 12 or more of `[A-Za-z0-9_-]`
/// after it, or `bearer`, white space and something more (both ignoring case); or three groups of
/// 2 to 4 digits joined by `-` or a space.
fn may_redact(value: &str) -> bool {
    if value.contains('@') {
        return true;
    }
    let chars: Vec<char> = value.chars().collect();
    let word = |start: usize, letters: &str| letters.chars().enumerate().all(|(offset, letter)| chars.get(start + offset).is_some_and(|char| if letter.is_ascii_alphabetic() { ci(*char, letter) } else { *char == letter }));
    let key_char = |char: char| char.is_ascii_alphanumeric() || char == '_' || char == '-' || matches!(char, '\u{130}' | '\u{131}' | '\u{17f}' | '\u{212a}');
    for start in 0..chars.len() {
        if word(start, "sk-") && chars[start + 3..].iter().take_while(|char| key_char(**char)).count() >= 12 {
            return true;
        }
        if word(start, "bearer") {
            let spaces = chars[start + 6..].iter().take_while(|char| char.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(*char)).count();
            if spaces > 0 && chars.get(start + 6 + spaces).is_some() {
                return true;
            }
        }
    }
    let digits = |start: usize| chars[start..].iter().take_while(|char| re_decimal(**char)).count();
    let joined = |char: char| char == '-' || char == ' ';
    for start in 0..chars.len() {
        let first = digits(start);
        if first < 2 || (start > 0 && re_decimal(chars[start - 1])) {
            continue;
        }
        // Any split of a run into groups: a run of 2 to 4 digits, a joint, then again, then 3+.
        let mut at = start + first;
        let second_start = at + 1;
        if at >= chars.len() || !joined(chars[at]) {
            continue;
        }
        let second = digits(second_start);
        if second < 2 {
            continue;
        }
        at = second_start + second;
        if at < chars.len() && joined(chars[at]) && digits(at + 1) >= 3 {
            return true;
        }
    }
    false
}

/// `safe_document` of a value the Scenario holds: the value itself, or `Unsupported` where Python
/// would redact or refuse something.
fn safe(value: &V) -> Result<V> {
    match value {
        V::Str(text) if may_redact(text) => Err(Error::unsupported("a text that safe_document may redact")),
        V::Float(number) if !number.is_finite() => Err(Error::unsupported("a float that is not finite")),
        V::Map(map) => {
            if map.keys().any(|key| private_key(key)) {
                return Err(Error::unsupported("a key that names private data"));
            }
            map.values().try_for_each(|item| safe(item).map(drop))?;
            Ok(value.clone())
        }
        V::List(items) => {
            items.iter().try_for_each(|item| safe(item).map(drop))?;
            Ok(value.clone())
        }
        _ => Ok(value.clone()),
    }
}

fn member<'a>(value: &'a V, key: &str) -> Option<&'a V> {
    match value {
        V::Map(map) => map.get(key),
        _ => None,
    }
}

/// The evidence route's answer for a new Run of the Scenario (canonical JSON): the table's
/// template with the model versions (`model_versions`), `scenario_sources`,
/// `scenario_assumptions` and `region_limits` filled in.
pub fn evidence(scenario: &[u8]) -> Result<Vec<u8>> {
    let (configs, provenance) = read(scenario)?;
    let entry = entry_of(&configs)?;
    let mut evidence = entry.evidence.clone();
    let list = |key: &str| member(&provenance, key).cloned().unwrap_or(V::List(Vec::new()));
    let research = member(&provenance, "region_research").cloned().unwrap_or(V::Map(IndexMap::new()));
    let limits = vmap([
        ("unknowns", member(&research, "unknowns").cloned().unwrap_or(V::List(Vec::new()))),
        ("warnings", member(&research, "warnings").cloned().unwrap_or(V::List(Vec::new()))),
    ]);
    for (key, value) in [
        ("model_versions", V::Map(signatures(&configs, &entry.model_signatures)?)),
        ("scenario_sources", safe(&list("sources"))?),
        ("scenario_assumptions", safe(&list("assumptions"))?),
        ("region_limits", safe(&limits)?),
    ] {
        if !evidence.contains_key(key) {
            return Err(Error::Value(format!("the table's evidence has no {key}")));
        }
        evidence.insert(text(key), value);
    }
    canonical(&V::Map(evidence))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_text_python_may_redact_is_found() {
        for text in ["a@b.co", "SK-abcdefghijkl", "x bearer  y", "03-1234-5678", "+81 90 1234 5678", "１２-３４-５６７"] {
            assert!(may_redact(text), "{text}");
        }
        for text in ["2026-11-01", "12-34-56", "task-1", "ASM-STANDARD-PROVISIONING-V1", "100 200", "SK-abc", "bearer"] {
            assert!(!may_redact(text), "{text}");
        }
    }

    #[test]
    fn a_key_naming_private_data_is_found() {
        for key in ["address", "API_KEY", "full name", "raw-prompt", "ſecret", "Phone"] {
            assert!(private_key(key), "{key}");
        }
        for key in ["id", "title", "source_ids", "name", "key"] {
            assert!(!private_key(key), "{key}");
        }
    }

    #[test]
    fn ensure_ascii_escapes_past_ascii() {
        let mut out = Vec::new();
        ascii_json(&vmap([("b", V::Str(Arc::from("墨\u{1f600}\"\n"))), ("a", V::Float(1.0))]), &mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "{\"a\":1.0,\"b\":\"\\u58a8\\ud83d\\ude00\\\"\\n\"}");
    }
}
