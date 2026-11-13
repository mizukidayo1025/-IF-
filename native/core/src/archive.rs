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
//!
//! A Run also stores saved points (#251): the world at a minute without its log (`world_parts`),
//! with how many records the log and how many samples the Run held then, and what else the Run
//! needs to go on from there (`SavedPoint`). The log and the samples are only appended to, so a
//! saved point holds no copy of them: `Restore` reads the stored log and samples back, a block at a
//! time, and keeps their first records and samples up to the saved point's counts; then
//! `PlainRun::restore` builds the Run at the saved point's minute, which goes on as the Run did.

use std::collections::BTreeMap;

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::canonical;
use crate::checkpoint::{hex, sha256_hex};
use crate::engine;
use crate::error::{Error, Result};
use crate::events::log::EventLog;
use crate::events::packed::{put_varint, put_zigzag, Blocks, Cursor, Packer};
use crate::plain::{unpack_sample, MetricSamples};
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
            let unpacked = unpack_sample(&self.packer, bytes)?;
            let (sample, minute, used) = (unpacked.sample, unpacked.minute, unpacked.used);
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

// ---- Saved points (#251) ---------------------------------------------------------------------------

/// The first bytes of a saved point.
const SAVED_POINT_MAGIC: &[u8] = b"SHSP";

/// The version of a saved point's bytes: a reader takes this version only, and any other is an
/// error, not read as this one.
pub const SAVED_POINT_VERSION: u64 = 1;

/// A Run at a minute, as a browser's Run stores it: the world without its log (`world_parts`), how
/// many records the log held and how many metric samples the Run took, and how many times each
/// contract gave its input back (`PlainHost::contracts`). The random source is the world's seed
/// (`CounterRNG`), and the ID counters, the scheduled events and the model state are parts of the
/// world; the floor plan is built again from the world's overlay, and the open actions from the
/// log (`ActiveActions::of_log`).
///
/// Its bytes: `SHSP`, then varints: the version, the minute (zigzag), the records, the samples,
/// the contracts' count and each contract's name (its length and its UTF-8) and count; then the
/// world's parts as `world_parts` writes them, to the end.
#[derive(Clone, Debug, PartialEq)]
pub struct SavedPoint {
    pub minute: i64,
    pub records: u64,
    pub samples: u64,
    pub contracts: BTreeMap<String, u64>,
    /// `world_parts` of the world at `minute`.
    pub world: Vec<u8>,
}

impl SavedPoint {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.world.len() + 64);
        out.extend_from_slice(SAVED_POINT_MAGIC);
        put_varint(&mut out, SAVED_POINT_VERSION);
        put_zigzag(&mut out, self.minute);
        put_varint(&mut out, self.records);
        put_varint(&mut out, self.samples);
        put_varint(&mut out, self.contracts.len() as u64);
        for (name, count) in &self.contracts {
            put_varint(&mut out, name.len() as u64);
            out.extend_from_slice(name.as_bytes());
            put_varint(&mut out, *count);
        }
        out.extend_from_slice(&self.world);
        out
    }

    /// The saved point `to_bytes` wrote: an error for bytes that are not one, or of another
    /// version. Its world's parts are read when it is restored (`world_document`).
    pub fn of_bytes(bytes: &[u8]) -> Result<Self> {
        let bad = |what: &str| Error::Value(format!("not a saved point: {what}"));
        let mut cursor = Cursor::new(bytes);
        if cursor.take(SAVED_POINT_MAGIC.len()).ok() != Some(SAVED_POINT_MAGIC) {
            return Err(bad("its first bytes"));
        }
        let version = cursor.varint()?;
        if version != SAVED_POINT_VERSION {
            return Err(Error::Value(format!("a saved point of version {version}: this core reads version {SAVED_POINT_VERSION} only")));
        }
        let minute = cursor.zigzag()?;
        let records = cursor.varint()?;
        let samples = cursor.varint()?;
        let count = cursor.varint()?;
        let mut contracts = BTreeMap::new();
        for _ in 0..count {
            let length = usize::try_from(cursor.varint()?).map_err(|_| bad("a length"))?;
            let name = std::str::from_utf8(cursor.take(length)?).map_err(|_| bad("a contract's name"))?.to_owned();
            if contracts.insert(name, cursor.varint()?).is_some() {
                return Err(bad("a contract twice"));
            }
        }
        if minute < 0 {
            return Err(bad("its minute"));
        }
        Ok(Self { minute, records, samples, contracts, world: cursor.rest().to_vec() })
    }
}

/// The world's document of the parts `world_parts` wrote, `event_log` an empty list: what
/// `load::world` reads. An error for bytes that are not such parts, a part twice or one that is not
/// JSON.
pub fn world_document(bytes: &[u8]) -> Result<Map<String, Value>> {
    let mut cursor = Cursor::new(bytes);
    let length = |cursor: &mut Cursor<'_>| usize::try_from(cursor.varint()?).map_err(|_| corrupt("a length"));
    let count = length(&mut cursor)?;
    let mut document = Map::new();
    for _ in 0..count {
        let name_length = length(&mut cursor)?;
        let name = std::str::from_utf8(cursor.take(name_length)?).map_err(|_| corrupt("a part's name"))?.to_owned();
        let raw_length = length(&mut cursor)?;
        let part: Value = serde_json::from_slice(cursor.take(raw_length)?).map_err(|_| corrupt("a part that is not JSON"))?;
        if name == "event_log" || document.insert(name, part).is_some() {
            return Err(corrupt("a part twice"));
        }
    }
    if !cursor.rest().is_empty() {
        return Err(corrupt("bytes after the parts"));
    }
    document.insert("event_log".to_owned(), Value::Array(Vec::new()));
    Ok(document)
}

/// The Run at a saved point, read back: the saved point, then the stored log's and samples'
/// tables and blocks in the order they were written, of which it keeps the first records and
/// samples up to the saved point's counts (blocks are not cut at minutes). `PlainRun::restore`
/// then builds the Run. The records and samples are kept as they were packed, against the tables
/// read, in the blocks read (the last cut after the last one kept); the log's indexes are made
/// again from each record. A restored Run appends to them, so it can store its own blocks from
/// there (a Branch's, #267).
#[derive(Debug)]
pub struct Restore {
    pub point: SavedPoint,
    log_packer: Packer,
    log: EventLog,
    log_blocks: Vec<Vec<u8>>,
    log_starts: Vec<u64>,
    sample_packer: Packer,
    sample_blocks: Vec<Vec<u8>>,
    sample_starts: Vec<u64>,
    sample_digest: Sha256,
    /// Whether the samples read hold the ledger's columns (`MetricSamples`); None before the first.
    sample_balances: Option<bool>,
}

/// Where a piece at `offset` of the block `index` starts (`Blocks`), or an error past them.
fn start(index: usize, offset: usize) -> Result<u64> {
    match (u32::try_from(index), u32::try_from(offset)) {
        (Ok(index), Ok(offset)) => Ok((u64::from(index) << 32) | u64::from(offset)),
        _ => Err(corrupt("a block too large")),
    }
}

impl Restore {
    /// A restore of the saved point of `bytes` (`SavedPoint::of_bytes`).
    pub fn new(bytes: &[u8]) -> Result<Self> {
        Ok(Self {
            point: SavedPoint::of_bytes(bytes)?,
            log_packer: Packer::new(),
            log: EventLog::new(),
            log_blocks: Vec::new(),
            log_starts: Vec::new(),
            sample_packer: Packer::new(),
            sample_blocks: Vec::new(),
            sample_starts: Vec::new(),
            sample_digest: Sha256::new(),
            sample_balances: None,
        })
    }

    /// Whether the log has fewer records than the saved point's.
    pub fn log_needed(&self) -> bool {
        (self.log_starts.len() as u64) < self.point.records
    }

    /// Whether there are fewer samples than the saved point's.
    pub fn samples_needed(&self) -> bool {
        (self.sample_starts.len() as u64) < self.point.samples
    }

    /// The log's tables' new entries (`Packer::write_tables`).
    pub fn read_log_tables(&mut self, bytes: &[u8]) -> Result<()> {
        self.log_packer.read_tables(bytes)
    }

    /// The records of a block, up to the saved point's count: how many were taken.
    pub fn read_log_block(&mut self, bytes: &[u8]) -> Result<usize> {
        let (index, mut at, mut taken) = (self.log_blocks.len(), 0, 0);
        while self.log_needed() && at < bytes.len() {
            let (record, used) = self.log_packer.unpack(&bytes[at..])?;
            if used == 0 {
                return Err(corrupt("an empty record"));
            }
            if self.log.add(record.event.index_fields(record.realized_minute, record.status), true, None) != 0 {
                return Err(corrupt("a record's event ID or sequence twice"));
            }
            self.log_starts.push(start(index, at)?);
            at += used;
            taken += 1;
        }
        if taken > 0 {
            self.log_blocks.push(bytes[..at].to_vec());
        }
        Ok(taken)
    }

    /// The samples' tables' new entries.
    pub fn read_sample_tables(&mut self, bytes: &[u8]) -> Result<()> {
        self.sample_packer.read_tables(bytes)
    }

    /// The samples of a block, up to the saved point's count: how many were taken.
    pub fn read_sample_block(&mut self, bytes: &[u8]) -> Result<usize> {
        let (index, mut at, mut taken) = (self.sample_blocks.len(), 0, 0);
        while self.samples_needed() && at < bytes.len() {
            let unpacked = unpack_sample(&self.sample_packer, &bytes[at..])?;
            let (sample, minute, used) = (unpacked.sample, unpacked.minute, unpacked.used);
            if used == 0 {
                return Err(corrupt("an empty sample"));
            }
            if self.sample_balances.is_some_and(|kept| kept != unpacked.balances.is_some()) {
                return Err(corrupt("samples with and without the ledger's columns"));
            }
            self.sample_balances = Some(unpacked.balances.is_some());
            if !self.sample_starts.is_empty() {
                self.sample_digest.update(b"\n");
            }
            self.sample_digest.update(engine::metric_line(&sample, minute)?);
            self.sample_starts.push(start(index, at)?);
            at += used;
            taken += 1;
        }
        if taken > 0 {
            self.sample_blocks.push(bytes[..at].to_vec());
        }
        Ok(taken)
    }

    /// The saved point, its log and its samples: an error where fewer records or samples were
    /// read than the saved point's.
    pub(crate) fn into_parts(self) -> Result<(SavedPoint, EventLog, MetricSamples)> {
        let short = |what: &str| Error::Value(format!("not a saved point's Run: the stored {what} end before the saved point's"));
        if self.log_needed() {
            return Err(short("records"));
        }
        if self.samples_needed() {
            return Err(short("samples"));
        }
        let mut log = self.log;
        log.adopt_packed(self.log_packer, Blocks::of_sealed(self.log_blocks), self.log_starts)?;
        let samples = MetricSamples::adopt(self.sample_packer, Blocks::of_sealed(self.sample_blocks), self.sample_starts, self.sample_digest, self.sample_balances);
        Ok((self.point, log, samples))
    }
}
