//! The text of Python's `json.dumps`, for the digests the pytest files took of it (Issue #225).
//!
//! Several pytest files fixed a sha256 of `json.dumps(value)` with Python's default options (the
//! separators `", "` and `": "`, `ensure_ascii=True`, the dict's order), some with
//! `ensure_ascii=False`, `sort_keys=True` or the separators `","` and `":"`. A test of the core
//! writes the same text with `Dumps`, so it can keep those digests. Floats are `float.__repr__`,
//! with `NaN`, `Infinity` and `-Infinity` as `allow_nan=True` writes them; a map keeps its order
//! (a `V` keeps Python's), or is sorted by key with `sort_keys`.

use sheltersim_core::json::python_float_repr;
use sheltersim_core::value::V;

/// The options of `json.dumps` these tests use; `Dumps::default()` is Python's default.
#[derive(Clone, Copy, Debug)]
pub struct Dumps {
    pub ensure_ascii: bool,
    pub sort_keys: bool,
    pub item_separator: &'static str,
    pub key_separator: &'static str,
}

impl Default for Dumps {
    fn default() -> Self {
        Self { ensure_ascii: true, sort_keys: false, item_separator: ", ", key_separator: ": " }
    }
}

impl Dumps {
    /// `separators=(",", ":")`.
    pub fn compact(self) -> Self {
        Self { item_separator: ",", key_separator: ":", ..self }
    }

    /// `ensure_ascii=False`.
    pub fn unicode(self) -> Self {
        Self { ensure_ascii: false, ..self }
    }

    /// `sort_keys=True`.
    pub fn sorted(self) -> Self {
        Self { sort_keys: true, ..self }
    }

    /// `json.dumps(value, ...)` with these options.
    pub fn text(&self, value: &V) -> String {
        let mut out = String::new();
        self.write(&mut out, value);
        out
    }

    fn write(&self, out: &mut String, value: &V) {
        match value {
            V::Null => out.push_str("null"),
            V::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
            V::Int(number) => out.push_str(&number.to_string()),
            V::Big(text) => out.push_str(text),
            V::Float(number) if number.is_nan() => out.push_str("NaN"),
            V::Float(number) if number.is_infinite() => out.push_str(if *number > 0.0 { "Infinity" } else { "-Infinity" }),
            V::Float(number) => out.push_str(&python_float_repr(*number)),
            V::Str(text) => self.string(out, text),
            V::List(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push_str(self.item_separator);
                    }
                    self.write(out, item);
                }
                out.push(']');
            }
            V::Map(items) => {
                let mut entries: Vec<_> = items.iter().collect();
                if self.sort_keys {
                    // Python orders str by code point, which is the order of their UTF-8 bytes.
                    entries.sort_by(|first, second| first.0.as_bytes().cmp(second.0.as_bytes()));
                }
                out.push('{');
                for (index, (key, item)) in entries.into_iter().enumerate() {
                    if index > 0 {
                        out.push_str(self.item_separator);
                    }
                    self.string(out, key);
                    out.push_str(self.key_separator);
                    self.write(out, item);
                }
                out.push('}');
            }
        }
    }

    /// `py_encode_basestring_ascii`, or `py_encode_basestring` without `ensure_ascii`.
    fn string(&self, out: &mut String, text: &str) {
        out.push('"');
        for character in text.chars() {
            match character {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\u{8}' => out.push_str("\\b"),
                '\u{c}' => out.push_str("\\f"),
                ' '..='~' => out.push(character),
                _ if !self.ensure_ascii && character >= ' ' => out.push(character),
                _ => {
                    let mut units = [0u16; 2];
                    for unit in character.encode_utf16(&mut units) {
                        out.push_str(&format!("\\u{unit:04x}"));
                    }
                }
            }
        }
        out.push('"');
    }
}

/// `json.dumps(value)`.
pub fn dumps(value: &V) -> String {
    Dumps::default().text(value)
}
