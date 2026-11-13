//! What the API's KPI history and offline report read of a Run's metric samples in the browser
//! (Issue #266): `GET …/branches/{branch}/history?kind=kpis` answers from the stored samples
//! (`MetricSamples`, #252) instead of the history index of the Python server
//! (`storage/history_index.py`), and the report reads the last sample.
//!
//! A row (`kpi_row`) is the index's: the sample's `MetricSnapshot` fields without
//! `source_ids_json`, the ledger's columns where the sample holds them (`resource_snapshots`),
//! `time_semantics`, `record_kind`, `run_id` and `branch_id`, as canonical JSON (keys sorted, the
//! floats as Python writes them). `KpiTable` holds a stored Run's samples read back and pages them
//! as the index's query does: the minutes from `start` to `end` that are `start` plus a multiple of
//! `step`, after the cursor's row, `limit` of them. The JS side (`js/api_answers.mjs`) checks the
//! query, makes the cursor and writes the rest of the answer.

use std::sync::Arc;

use crate::error::{Error, Result};
use crate::events::packed::Packer;
use crate::frame::{kpis, RESOURCE_COLUMNS};
use crate::json::Encoder;
use crate::plain::{unpack_sample, Balances, UnpackedSample};
use crate::rules::metrics::MetricSample;
use crate::value::V;

fn corrupt(what: &str) -> Error {
    Error::Value(format!("not stored samples: {what}"))
}

/// The history index's row of a sample (`HistoryIndex.ingest`, then the answer's `record_kind`,
/// `run_id` and `branch_id`), as canonical JSON.
pub fn kpi_row(sample: &MetricSample, minute: i64, balances: Option<&Balances>, run_id: &str, branch_id: &str) -> Result<Vec<u8>> {
    let mut row = kpis(sample, minute);
    let mut put = |name: &str, value: V| {
        row.insert(Arc::from(name), value);
    };
    for (names, values) in RESOURCE_COLUMNS.iter().zip(balances.into_iter().flatten()) {
        for (name, value) in names.iter().zip(values) {
            put(name, V::Int(*value));
        }
    }
    put("time_semantics", V::Str(Arc::from("end_of_tick")));
    put("record_kind", V::Str(Arc::from("kpis")));
    put("run_id", V::Str(Arc::from(run_id)));
    put("branch_id", V::Str(Arc::from(branch_id)));
    let mut encoder = Encoder::with_capacity(1024);
    V::Map(row).write_json(&mut encoder)?;
    Ok(encoder.out)
}

/// The final metrics of the offline report's evidence (`build_review_evidence`): each field of the
/// sample's `MetricSnapshot` but `minute_index` and `source_ids_json`, as a float, as canonical
/// JSON.
pub fn review_metrics(sample: &MetricSample) -> Result<Vec<u8>> {
    let mut row = kpis(sample, 0);
    row.shift_remove("minute_index");
    for value in row.values_mut() {
        if let V::Int(number) = value {
            *value = V::Float(*number as f64);
        }
    }
    let mut encoder = Encoder::with_capacity(1024);
    V::Map(row).write_json(&mut encoder)?;
    Ok(encoder.out)
}

/// A page of KPI rows (`KpiTable::page`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KpiPage {
    /// The rows, as a JSON list.
    pub items: Vec<u8>,
    /// The minute of the last row, which the next page's cursor names.
    pub last: Option<i64>,
    /// Whether rows follow the page.
    pub more: bool,
}

/// What a page asks for: the index's query of `kind=kpis` (`HistoryIndex.search`).
#[derive(Clone, Debug)]
pub struct KpiQuery<'a> {
    pub start: i64,
    pub end: i64,
    pub step: i64,
    pub limit: usize,
    /// The cursor's row: its minute and the key after `kpis:`; rows come after it in the order
    /// of (minute, key), the key a row's minute as text.
    pub after: Option<(i64, &'a str)>,
    /// A filter no KPI row passes (an identity, a type, a source, a status or a text term): no rows.
    pub filtered: bool,
    pub run_id: &'a str,
    pub branch_id: &'a str,
}

/// A stored Run's metric samples read back (the pieces `sample_tables` and `sample_block` of
/// `run_store.mjs`, in the order they were written), kept packed, with each one's minute.
#[derive(Clone, Debug, Default)]
pub struct KpiTable {
    packer: Packer,
    blocks: Vec<Vec<u8>>,
    /// Each sample's block and offset in it.
    starts: Vec<(usize, usize)>,
    minutes: Vec<i64>,
}

impl KpiTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// The tables' new entries (`Packer::write_tables`).
    pub fn read_tables(&mut self, bytes: &[u8]) -> Result<()> {
        self.packer.read_tables(bytes)
    }

    /// The samples of a block: how many it held. An error for bytes that are not samples, or
    /// samples whose minutes do not go up.
    pub fn read_block(&mut self, bytes: &[u8]) -> Result<usize> {
        let index = self.blocks.len();
        let (mut at, mut read) = (0, 0);
        while at < bytes.len() {
            let unpacked = unpack_sample(&self.packer, &bytes[at..])?;
            if unpacked.used == 0 {
                return Err(corrupt("an empty sample"));
            }
            if self.minutes.last().is_some_and(|last| *last >= unpacked.minute) {
                return Err(corrupt("the minutes do not go up"));
            }
            self.starts.push((index, at));
            self.minutes.push(unpacked.minute);
            at += unpacked.used;
            read += 1;
        }
        self.blocks.push(bytes.to_vec());
        Ok(read)
    }

    /// How many samples were read.
    pub fn len(&self) -> usize {
        self.minutes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.minutes.is_empty()
    }

    /// The sample `index`, or None past the last.
    pub fn sample(&self, index: usize) -> Result<Option<UnpackedSample>> {
        let Some(&(block, offset)) = self.starts.get(index) else {
            return Ok(None);
        };
        unpack_sample(&self.packer, &self.blocks[block][offset..]).map(Some)
    }

    /// The first index whose minute is at least `minute`.
    fn first_at(&self, minute: i64) -> usize {
        self.minutes.partition_point(|at| *at < minute)
    }

    /// How many samples there are from `start` to `end`, and the first's and the last's minutes
    /// (the index's `coverage_by_kind`, which no step, cursor or filter narrows).
    pub fn coverage(&self, start: i64, end: i64) -> Option<(usize, i64, i64)> {
        let first = self.first_at(start);
        let past = self.first_at(end.saturating_add(1));
        (first < past).then(|| (past - first, self.minutes[first], self.minutes[past - 1]))
    }

    /// A page of rows (see `KpiQuery`).
    pub fn page(&self, query: &KpiQuery<'_>) -> Result<KpiPage> {
        let mut page = KpiPage { items: vec![b'['], ..KpiPage::default() };
        if query.filtered || query.step < 1 || query.start > query.end {
            page.items.push(b']');
            return Ok(page);
        }
        let from = match query.after {
            Some((minute, _)) => query.start.max(minute),
            None => query.start,
        };
        let mut taken = 0;
        for index in self.first_at(from)..self.minutes.len() {
            let minute = self.minutes[index];
            if minute > query.end {
                break;
            }
            if (minute - query.start) % query.step != 0 {
                continue;
            }
            if let Some((after, key)) = query.after {
                if minute == after && minute.to_string().as_bytes() <= key.as_bytes() {
                    continue;
                }
            }
            if taken == query.limit {
                page.more = true;
                break;
            }
            let unpacked = self.sample(index)?.ok_or_else(|| corrupt("a sample is missing"))?;
            if taken > 0 {
                page.items.push(b',');
            }
            page.items.extend(kpi_row(&unpacked.sample, minute, unpacked.balances.as_ref(), query.run_id, query.branch_id)?);
            page.last = Some(minute);
            taken += 1;
        }
        page.items.push(b']');
        Ok(page)
    }
}
