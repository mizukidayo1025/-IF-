//! What a Run in the browser stores, and the readers that check it (#252, PR 6-5).
//!
//! The browser keeps a Run's log and metric samples in the binary form (`events::packed`): blocks
//! of packed records and samples, and the tables (strings and shapes) they are packed against. It
//! stores them as they are, a sealed block at a time, with the tables' new entries before the
//! blocks that use them (`Packer::write_tables`, `Blocks::sealed`); and beside them the world at
//! its last minute without its log (`world_parts`). The page writes them, compressed, to its
//! storage (OPFS) with a journal of where each piece is.
//!
//! The readers here take those pieces back, in the order they were written, and give the digests
//! a Run gives (`canonical::digests`, `PlainHost::metrics_digest`): `LogReader` the `event_log`
//! part's, written again from the packed records; `SampleReader` the metric lines'; and
//! `world_part_digests` each other part's. A stored Run whose digests are the Run's holds the Run.
//! The readers read untrusted bytes: anything else is an error, never a panic.

use sha2::{Digest, Sha256};

use crate::canonical;
use crate::checkpoint::{hex, sha256_hex};
use crate::engine;
use crate::error::{Error, Result};
use crate::events::packed::{put_varint, Cursor, Packer};
use crate::plain::unpack_sample;
use crate::world::World;

fn corrupt(what: &str) -> Error {
    Error::Value(format!("not a stored Run: {what}"))
}

/// The world's parts but `event_log` (`canonical::parts_without_log`), each its name and its
/// canonical JSON: the count, then each part's name and bytes, each a varint length and the bytes.
pub fn world_parts(world: &World) -> Result<Vec<u8>> {
    let parts = canonical::parts_without_log(world)?;
    let mut out = Vec::with_capacity(parts.iter().map(|(name, raw)| name.len() + raw.len() + 8).sum());
    put_varint(&mut out, parts.len() as u64);
    for (name, raw) in parts {
        put_varint(&mut out, name.len() as u64);
        out.extend_from_slice(name.as_bytes());
        put_varint(&mut out, raw.len() as u64);
        out.extend_from_slice(&raw);
    }
    Ok(out)
}

/// The sha256 of each part `world_parts` wrote, in hex, by its name, in its order.
pub fn world_part_digests(bytes: &[u8]) -> Result<Vec<(String, String)>> {
    let mut cursor = Cursor::new(bytes);
    let length = |cursor: &mut Cursor<'_>| usize::try_from(cursor.varint()?).map_err(|_| corrupt("a length"));
    let count = length(&mut cursor)?;
    let mut digests = Vec::with_capacity(count.min(64));
    for _ in 0..count {
        let name_length = length(&mut cursor)?;
        let name = std::str::from_utf8(cursor.take(name_length)?).map_err(|_| corrupt("a part's name"))?.to_owned();
        let raw_length = length(&mut cursor)?;
        digests.push((name, sha256_hex(cursor.take(raw_length)?)));
    }
    if !cursor.rest().is_empty() {
        return Err(corrupt("bytes after the parts"));
    }
    Ok(digests)
}

/// Reads a stored log back: its tables and its blocks, in the order they were written. The digest
/// is the `event_log` part's (`canonical::log_digest`) of the records read so far.
#[derive(Clone, Debug)]
pub struct LogReader {
    packer: Packer,
    hasher: Sha256,
    records: u64,
    record: Vec<u8>,
}

impl Default for LogReader {
    fn default() -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"[");
        Self { packer: Packer::new(), hasher, records: 0, record: Vec::new() }
    }
}

impl LogReader {
    pub fn new() -> Self {
        Self::default()
    }

    /// The tables' new entries (`Packer::write_tables`).
    pub fn read_tables(&mut self, bytes: &[u8]) -> Result<()> {
        self.packer.read_tables(bytes)
    }

    /// The records of a block, one after another: how many it held. On an error, the records read
    /// before it in the block are counted and digested.
    pub fn read_block(&mut self, mut bytes: &[u8]) -> Result<usize> {
        let mut read = 0;
        while !bytes.is_empty() {
            let (record, used) = self.packer.unpack(bytes)?;
            if used == 0 {
                return Err(corrupt("an empty record"));
            }
            self.record.clear();
            self.record.extend_from_slice(&record.record_json()?);
            if self.records > 0 {
                self.hasher.update(b",");
            }
            self.hasher.update(&self.record);
            self.records += 1;
            read += 1;
            bytes = &bytes[used..];
        }
        Ok(read)
    }

    /// How many records were read.
    pub fn records(&self) -> u64 {
        self.records
    }

    /// The digest of the `event_log` part of the records read, in hex.
    pub fn digest(&self) -> String {
        let mut hasher = self.hasher.clone();
        hasher.update(b"]");
        hex(&hasher.finalize())
    }
}

/// Reads stored metric samples back: their tables and their blocks, in the order they were
/// written. The digest is the one of their lines (`PlainHost::metrics_digest`).
#[derive(Clone, Debug, Default)]
pub struct SampleReader {
    packer: Packer,
    hasher: Sha256,
    samples: u64,
}

impl SampleReader {
    pub fn new() -> Self {
        Self::default()
    }

    /// The tables' new entries (`Packer::write_tables`).
    pub fn read_tables(&mut self, bytes: &[u8]) -> Result<()> {
        self.packer.read_tables(bytes)
    }

    /// The samples of a block, one after another: how many it held.
    pub fn read_block(&mut self, mut bytes: &[u8]) -> Result<usize> {
        let mut read = 0;
        while !bytes.is_empty() {
            let (sample, minute, used) = unpack_sample(&self.packer, bytes)?;
            if used == 0 {
                return Err(corrupt("an empty sample"));
            }
            if self.samples > 0 {
                self.hasher.update(b"\n");
            }
            self.hasher.update(engine::metric_line(&sample, minute)?);
            self.samples += 1;
            read += 1;
            bytes = &bytes[used..];
        }
        Ok(read)
    }

    /// How many samples were read.
    pub fn samples(&self) -> u64 {
        self.samples
    }

    /// The digest of the lines of the samples read, joined by `\n`, in hex.
    pub fn digest(&self) -> String {
        hex(&self.hasher.clone().finalize())
    }
}
