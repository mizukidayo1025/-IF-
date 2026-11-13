//! A realized record in a binary form (#252, PR 6-2): what the log of a Run in the browser holds
//! instead of the record's canonical JSON. A record of 7 days of 100 people takes 156 bytes on
//! average here and about 1.1 KB as JSON.
//!
//! The form holds the record's `EventRow` and its outcome (`realized_minute`, `status`, `detail`)
//! as they are, so `EventRow::record_json` of what `unpack` gives writes the bytes the record had:
//! the canonical JSON stays the one form records are compared, hashed and exchanged in.
//!
//! - A string is a number in the packer's table (`Packer::strings`), whose text is held once. A
//!   string of a prefix and a decimal number (`action-lifecycle-136663`: no leading zero, below
//!   2^64) is the prefix's number and the number, so the IDs of a Run take a few bytes each and add
//!   nothing to the table. Map keys and the text of a `Big` are held whole.
//! - A map is the number of its keys, in their order (a shape, `Packer::shapes`), then its values in
//!   that order: the order a map keeps is part of what it writes.
//! - `Int`, `Big`, `Float`, `Bool` and `Null` stay apart, as `json.dumps` tells them apart. An int
//!   is a zigzag varint; a float is its 8 bytes, little-endian, so every bit comes back.
//!
//! The tables grow as records are packed and never change what they hold, so a record packed
//! earlier unpacks the same later. `unpack` reads untrusted bytes: anything that is not a record of
//! this packer's tables is an error, never a panic.
//!
//! Layout of a record (`u` a varint, `i` a zigzag varint, `s` a string, `o` an optional string):
//!
//! ```text
//! head: u8 = status * 4 + kind   realized_minute: u   detail: o
//! event_id: s   due_minute: u   priority: i   sequence: u   event_type: s
//! source_id: o   causation_id: o   payload: value   provenance: value
//! s: u = text * 2, or u = prefix * 2 + 1 then u = number      o: 0, or 1 then s
//! value: tag u8, then 0 null | 1 false | 2 true | 3 i | 4 f64 | 5 s | 6 u count, values
//!                     | 7 u shape, values | 8 u text (Big)
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use indexmap::IndexMap;

use super::{EventKind, EventRow, EventStatus};
use crate::error::{Error, Result};
use crate::value::{MAX_DEPTH, V};

const NULL: u8 = 0;
const FALSE: u8 = 1;
const TRUE: u8 = 2;
const INT: u8 = 3;
const FLOAT: u8 = 4;
const STR: u8 = 5;
const LIST: u8 = 6;
const MAP: u8 = 7;
const BIG: u8 = 8;

/// The digits of a number a string may end in: below 2^64, with no leading zero.
const MAX_DIGITS: usize = 20;

fn corrupt(what: &str) -> Error {
    Error::Value(format!("not a packed record: {what}"))
}

fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn put_zigzag(out: &mut Vec<u8>, value: i64) {
    put_varint(out, ((value << 1) ^ (value >> 63)) as u64);
}

/// `text` as a prefix and the number it ends in, when the number is written as `u64` writes it.
fn split_number(text: &str) -> Option<(&str, u64)> {
    let digits = text.bytes().rev().take_while(u8::is_ascii_digit).count();
    if digits == 0 || digits == text.len() || digits > MAX_DIGITS {
        return None;
    }
    let (prefix, number) = text.split_at(text.len() - digits);
    if number.len() > 1 && number.starts_with('0') {
        return None;
    }
    number.parse().ok().map(|number| (prefix, number))
}

/// The tables records are packed against, and the packing itself.
#[derive(Clone, Debug, Default)]
pub struct Packer {
    texts: Vec<Arc<str>>,
    text_ids: HashMap<Arc<str>, u32>,
    shapes: Vec<Box<[u32]>>,
    shape_ids: HashMap<Box<[u32]>, u32>,
    /// A shape's keys, made once.
    shape_keys: Vec<Box<[Arc<str>]>>,
}

impl Packer {
    pub fn new() -> Self {
        Self::default()
    }

    /// The texts of the string table, in the order they were added.
    pub fn strings(&self) -> &[Arc<str>] {
        &self.texts
    }

    /// The shapes, each its keys' numbers in the string table, in the order they were added.
    pub fn shapes(&self) -> &[Box<[u32]>] {
        &self.shapes
    }

    fn text_id(&mut self, text: &str) -> Result<u32> {
        if let Some(&id) = self.text_ids.get(text) {
            return Ok(id);
        }
        let id = u32::try_from(self.texts.len()).map_err(|_| Error::unsupported("more than 2^32 distinct strings"))?;
        let text: Arc<str> = Arc::from(text);
        self.texts.push(text.clone());
        self.text_ids.insert(text, id);
        Ok(id)
    }

    fn put_text(&mut self, out: &mut Vec<u8>, text: &str) -> Result<()> {
        match split_number(text) {
            Some((prefix, number)) => {
                let prefix = self.text_id(prefix)?;
                put_varint(out, u64::from(prefix) * 2 + 1);
                put_varint(out, number);
            }
            None => {
                let id = self.text_id(text)?;
                put_varint(out, u64::from(id) * 2);
            }
        }
        Ok(())
    }

    fn put_optional(&mut self, out: &mut Vec<u8>, text: Option<&str>) -> Result<()> {
        match text {
            None => out.push(0),
            Some(text) => {
                out.push(1);
                self.put_text(out, text)?;
            }
        }
        Ok(())
    }

    fn put_value(&mut self, out: &mut Vec<u8>, value: &V, depth: usize) -> Result<()> {
        if depth > MAX_DEPTH {
            return Err(Error::unsupported("a value nested too deep to pack"));
        }
        match value {
            V::Null => out.push(NULL),
            V::Bool(false) => out.push(FALSE),
            V::Bool(true) => out.push(TRUE),
            V::Int(number) => {
                out.push(INT);
                put_zigzag(out, *number);
            }
            V::Float(number) => {
                out.push(FLOAT);
                out.extend_from_slice(&number.to_bits().to_le_bytes());
            }
            V::Str(text) => {
                out.push(STR);
                self.put_text(out, text)?;
            }
            V::Big(text) => {
                out.push(BIG);
                let id = self.text_id(text)?;
                put_varint(out, u64::from(id));
            }
            V::List(items) => {
                out.push(LIST);
                put_varint(out, items.len() as u64);
                for item in items {
                    self.put_value(out, item, depth + 1)?;
                }
            }
            V::Map(map) => {
                let keys = map.keys().map(|key| self.text_id(key)).collect::<Result<Box<[u32]>>>()?;
                let shape = match self.shape_ids.get(&keys) {
                    Some(&shape) => shape,
                    None => {
                        let shape = u32::try_from(self.shapes.len()).map_err(|_| Error::unsupported("more than 2^32 shapes"))?;
                        self.shape_keys.push(map.keys().cloned().collect());
                        self.shapes.push(keys.clone());
                        self.shape_ids.insert(keys, shape);
                        shape
                    }
                };
                out.push(MAP);
                put_varint(out, u64::from(shape));
                for item in map.values() {
                    self.put_value(out, item, depth + 1)?;
                }
            }
        }
        Ok(())
    }

    /// Appends the packed form of the record of `event`'s outcome to `out`. Nothing is appended for
    /// an error (a table past 2^32 entries, a value nested past `MAX_DEPTH`), though the tables may
    /// have grown.
    pub fn pack(&mut self, out: &mut Vec<u8>, event: &EventRow, realized_minute: i64, status: EventStatus, detail: Option<&str>) -> Result<()> {
        let mut record = Vec::with_capacity(160);
        let unsigned = |value: i64, what: &str| u64::try_from(value).map_err(|_| Error::unsupported(format!("a negative {what}")));
        let status_index = EventStatus::ALL.iter().position(|each| *each == status).unwrap_or_default();
        let kind_index = EventKind::ALL.iter().position(|each| *each == event.kind).unwrap_or_default();
        record.push((status_index * 4 + kind_index) as u8);
        put_varint(&mut record, unsigned(realized_minute, "realized minute")?);
        self.put_optional(&mut record, detail)?;
        self.put_text(&mut record, &event.event_id)?;
        put_varint(&mut record, unsigned(event.due_minute, "due minute")?);
        put_zigzag(&mut record, event.priority);
        put_varint(&mut record, unsigned(event.sequence, "sequence")?);
        self.put_text(&mut record, &event.event_type)?;
        self.put_optional(&mut record, event.source_id.as_deref())?;
        self.put_optional(&mut record, event.causation_id.as_deref())?;
        self.put_value(&mut record, &event.payload, 0)?;
        self.put_value(&mut record, &event.provenance, 0)?;
        out.extend_from_slice(&record);
        Ok(())
    }

    /// The record at the start of `bytes`, and how many bytes it took.
    pub fn unpack(&self, bytes: &[u8]) -> Result<(Unpacked, usize)> {
        let mut reader = Reader { bytes, at: 0, packer: self };
        let head = reader.byte()?;
        let status = *EventStatus::ALL.get(usize::from(head / 4)).ok_or_else(|| corrupt("a status"))?;
        let kind = *EventKind::ALL.get(usize::from(head % 4)).ok_or_else(|| corrupt("a kind"))?;
        let realized_minute = reader.signed_from_unsigned()?;
        let detail = reader.optional()?;
        let event_id = reader.text()?;
        let due_minute = reader.signed_from_unsigned()?;
        let priority = reader.zigzag()?;
        let sequence = reader.signed_from_unsigned()?;
        let event_type = reader.text()?;
        let source_id = reader.optional()?;
        let causation_id = reader.optional()?;
        let payload = reader.value(0)?;
        let provenance = reader.value(0)?;
        let event = EventRow { event_id, kind, due_minute, priority, sequence, event_type, payload, source_id, causation_id, provenance };
        Ok((Unpacked { event, realized_minute, status, detail }, reader.at))
    }
}

/// A record `Packer::unpack` read.
#[derive(Clone, Debug, PartialEq)]
pub struct Unpacked {
    pub event: EventRow,
    pub realized_minute: i64,
    pub status: EventStatus,
    pub detail: Option<Arc<str>>,
}

impl Unpacked {
    /// The record's canonical JSON (`EventRow::record_json`).
    pub fn record_json(&self) -> Result<Box<[u8]>> {
        self.event.record_json(self.realized_minute, self.status, self.detail.as_deref())
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
    packer: &'a Packer,
}

impl<'a> Reader<'a> {
    fn byte(&mut self) -> Result<u8> {
        let byte = *self.bytes.get(self.at).ok_or_else(|| corrupt("it ends early"))?;
        self.at += 1;
        Ok(byte)
    }

    fn varint(&mut self) -> Result<u64> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = self.byte()?;
            let part = u64::from(byte & 0x7f);
            if shift == 63 && part > 1 {
                return Err(corrupt("a varint past 64 bits"));
            }
            value |= part << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(corrupt("a varint past 64 bits"))
    }

    fn zigzag(&mut self) -> Result<i64> {
        let value = self.varint()?;
        Ok(((value >> 1) as i64) ^ -((value & 1) as i64))
    }

    fn signed_from_unsigned(&mut self) -> Result<i64> {
        i64::try_from(self.varint()?).map_err(|_| corrupt("a number past i64"))
    }

    fn table_text(&self, id: u64) -> Result<&'a Arc<str>> {
        let packer: &'a Packer = self.packer;
        usize::try_from(id).ok().and_then(|id| packer.texts.get(id)).ok_or_else(|| corrupt("a string the table does not hold"))
    }

    fn text(&mut self) -> Result<Arc<str>> {
        let reference = self.varint()?;
        let text = self.table_text(reference / 2)?;
        if reference % 2 == 0 {
            return Ok(text.clone());
        }
        let number = self.varint()?;
        let mut joined = String::with_capacity(text.len() + MAX_DIGITS);
        joined.push_str(text);
        joined.push_str(itoa::Buffer::new().format(number));
        Ok(Arc::from(joined))
    }

    fn optional(&mut self) -> Result<Option<Arc<str>>> {
        match self.byte()? {
            0 => Ok(None),
            1 => self.text().map(Some),
            _ => Err(corrupt("an optional string")),
        }
    }

    fn value(&mut self, depth: usize) -> Result<V> {
        if depth > MAX_DEPTH {
            return Err(corrupt("a value nested too deep"));
        }
        Ok(match self.byte()? {
            NULL => V::Null,
            FALSE => V::Bool(false),
            TRUE => V::Bool(true),
            INT => V::Int(self.zigzag()?),
            FLOAT => {
                let end = self.at.checked_add(8).filter(|end| *end <= self.bytes.len()).ok_or_else(|| corrupt("it ends early"))?;
                let mut bits = [0u8; 8];
                bits.copy_from_slice(&self.bytes[self.at..end]);
                self.at = end;
                V::Float(f64::from_bits(u64::from_le_bytes(bits)))
            }
            STR => V::Str(self.text()?),
            BIG => {
                let id = self.varint()?;
                V::Big(Box::from(&**self.table_text(id)?))
            }
            LIST => {
                let count = self.varint()?;
                // Each item takes at least a byte: a count past what is left is not a list.
                let count = usize::try_from(count).ok().filter(|count| *count <= self.bytes.len() - self.at).ok_or_else(|| corrupt("a list's length"))?;
                let mut items = Vec::with_capacity(count);
                for _ in 0..count {
                    items.push(self.value(depth + 1)?);
                }
                V::List(items)
            }
            MAP => {
                let shape = self.varint()?;
                let packer: &'a Packer = self.packer;
                let keys = usize::try_from(shape).ok().and_then(|shape| packer.shape_keys.get(shape)).ok_or_else(|| corrupt("a shape the table does not hold"))?;
                let mut map = IndexMap::with_capacity(keys.len());
                for key in keys.iter() {
                    map.insert(key.clone(), self.value(depth + 1)?);
                }
                V::Map(map)
            }
            _ => return Err(corrupt("a value's tag")),
        })
    }
}
