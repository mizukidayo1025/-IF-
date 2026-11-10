//! `V`: a JSON value owned by Rust, for state without typed rows. It holds exactly what
//! `json.dumps` tells apart: `1`, `1.0` and `True` differ, and a map keeps Python's insertion
//! order, so a value read from Python writes the same bytes and goes back the same. The PyO3 crate
//! reads it from Python and makes it into Python (`native/src/core/value.rs`).

use std::sync::Arc;

use indexmap::IndexMap;

use crate::error::Result;
use crate::json::Encoder;

#[derive(Clone, Debug, PartialEq)]
pub enum V {
    Null,
    Bool(bool),
    Int(i64),
    /// An int outside i64, as `int.__repr__` writes it.
    Big(Box<str>),
    Float(f64),
    Str(Arc<str>),
    List(Vec<V>),
    Map(IndexMap<Arc<str>, V>),
}

// Rows and values must be usable without the GIL.
const _: fn() = || {
    fn send_and_sync<T: Send + Sync>() {}
    send_and_sync::<V>();
};

impl V {
    /// `bool(value)`.
    pub fn is_truthy(&self) -> bool {
        match self {
            V::Null => false,
            V::Bool(flag) => *flag,
            V::Int(number) => *number != 0,
            V::Big(_) => true,
            V::Float(number) => *number != 0.0,
            V::Str(text) => !text.is_empty(),
            V::List(items) => !items.is_empty(),
            V::Map(items) => !items.is_empty(),
        }
    }

    /// The canonical JSON of the value (`canonical_json` of the PyO3 crate), keys sorted by bytes.
    /// NaN and infinity are `Unsupported`, as there.
    pub fn write_json(&self, encoder: &mut Encoder) -> Result<()> {
        match self {
            V::Null => encoder.null(),
            V::Bool(flag) => encoder.bool(*flag),
            V::Int(number) => encoder.int(*number),
            V::Big(text) => encoder.out.extend_from_slice(text.as_bytes()),
            V::Float(number) => encoder.try_float(*number)?,
            V::Str(text) => encoder.string(text),
            V::List(items) => {
                encoder.out.push(b'[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        encoder.out.push(b',');
                    }
                    item.write_json(encoder)?;
                }
                encoder.out.push(b']');
            }
            V::Map(map) => {
                let mut entries: Vec<(&Arc<str>, &V)> = map.iter().collect();
                entries.sort_unstable_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
                encoder.out.push(b'{');
                for (index, (key, item)) in entries.into_iter().enumerate() {
                    if index > 0 {
                        encoder.out.push(b',');
                    }
                    encoder.string(key);
                    encoder.out.push(b':');
                    item.write_json(encoder)?;
                }
                encoder.out.push(b'}');
            }
        }
        Ok(())
    }

    /// The canonical JSON bytes of the value.
    pub fn to_json(&self) -> Result<Vec<u8>> {
        let mut encoder = Encoder::with_capacity(256);
        self.write_json(&mut encoder)?;
        Ok(encoder.out)
    }

    /// The bytes the value holds on the heap, as allocated (capacities, not lengths), without the
    /// allocator's own overhead: for measuring what a namespace costs (lane 5-3).
    pub fn heap_bytes(&self) -> usize {
        match self {
            V::Null | V::Bool(_) | V::Int(_) | V::Float(_) => 0,
            V::Big(text) => text.len(),
            // An `Arc<str>` holds its two counts next to the text.
            V::Str(text) => text.len() + 2 * std::mem::size_of::<usize>(),
            V::List(items) => {
                items.capacity() * std::mem::size_of::<V>() + items.iter().map(V::heap_bytes).sum::<usize>()
            }
            V::Map(map) => {
                // An entry is its hash, key and value; the index table holds one usize per entry.
                let entry = std::mem::size_of::<(u64, Arc<str>, V)>() + std::mem::size_of::<usize>();
                map.capacity() * entry
                    + map.iter().map(|(key, item)| key.len() + 2 * std::mem::size_of::<usize>() + item.heap_bytes()).sum::<usize>()
            }
        }
    }
}
