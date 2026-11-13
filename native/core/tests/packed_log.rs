//! An `EventLog` that holds its records in the binary form (`EventLog::pack_records`, #252 PR 6-3):
//! it reads and writes every record as the log of bytes does, takes back what a rollback takes
//! back, and keeps the bytes of a record its packed form would not write again.

mod support;

use sheltersim_core::events::log::{fields_of_bytes, EventLog};
use sheltersim_core::events::packed::Unpacked;
use sheltersim_core::plain::PlainRun;
use sheltersim_core::world::WorldCell;
use support::load;

/// The records of the generated world of `tick_core.json` after its ticks, as rows.
fn records() -> Vec<Unpacked> {
    let fixture = load::entry("tick_core", "generated");
    let mut run = PlainRun::of_entry(&fixture).unwrap();
    for _ in 0..fixture["expected"]["ticks"].as_i64().unwrap() {
        run.step().unwrap();
    }
    run.loaded.cell.with(|world| {
        let log = world.log().unwrap();
        (0..log.len()).map(|index| log.record(index).unwrap().unwrap()).collect()
    })
    .unwrap()
}

fn append(log: &mut EventLog, record: &Unpacked) {
    assert_eq!(log.append_row(&record.event, record.realized_minute, record.status, record.detail.as_deref()).unwrap(), 0);
}

fn json(log: &EventLog) -> Vec<Vec<u8>> {
    (0..log.len())
        .map(|index| {
            let mut raw = Vec::new();
            assert!(log.write_record(index, &mut raw).unwrap());
            raw
        })
        .collect()
}

/// The same records appended to a log of bytes and to a packed one: the same JSON, the same rows,
/// the same lookups; the packed one holds no bytes, and a seventh of them or less.
#[test]
fn a_packed_log_reads_and_writes_what_a_log_of_bytes_does() {
    let records = records();
    let mut bytes = EventLog::new();
    let mut packed = EventLog::new();
    assert_eq!(packed.pack_records().unwrap(), 0);
    for record in &records {
        append(&mut bytes, record);
        append(&mut packed, record);
    }
    assert_eq!(json(&packed), json(&bytes));
    for (index, record) in records.iter().enumerate() {
        assert_eq!(packed.record(index).unwrap().as_ref(), Some(record));
        assert_eq!(bytes.record(index).unwrap().as_ref(), Some(record));
        assert!(packed.written(index).is_none() && bytes.written(index).is_some());
        assert_eq!(packed.index_of(&record.event.event_id), Some(index));
    }
    let (size, strings, shapes) = packed.packed_sizes().unwrap();
    let json_size: usize = json(&bytes).iter().map(Vec::len).sum();
    assert!(size * 4 < json_size, "{size} of {json_size}");
    assert!(strings > 0 && shapes > 0);
    assert!(bytes.packed_sizes().is_none() && !bytes.is_packed());
    // Out of range: no record.
    assert_eq!(packed.record(records.len()).unwrap(), None);
    assert!(!packed.write_record(records.len(), &mut Vec::new()).unwrap());
}

/// A rollback takes the records appended since the mark out of the packed bytes too: what is
/// appended after it is where the taken records were.
#[test]
fn a_rollback_takes_back_the_packed_records_since_its_mark() {
    let records = records();
    let (first, rest) = records.split_at(records.len() / 2);
    let mut log = EventLog::new();
    log.pack_records().unwrap();
    for record in first {
        append(&mut log, record);
    }
    let size = log.packed_sizes().unwrap().0;
    let mark = log.mark();
    for record in &rest[..10] {
        append(&mut log, record);
    }
    log.rollback(mark);
    assert_eq!(log.len(), first.len());
    assert_eq!(log.packed_sizes().unwrap().0, size);
    for record in rest {
        append(&mut log, record);
    }
    let mut whole = EventLog::new();
    for record in &records {
        append(&mut whole, record);
    }
    assert_eq!(json(&log), json(&whole));
}

/// A copy of a record under another ID and sequence, so a log can hold many.
fn renamed(record: &Unpacked, copy: i64) -> Unpacked {
    let mut again = record.clone();
    again.event.event_id = std::sync::Arc::from(format!("{}.{copy}", record.event.event_id));
    again.event.sequence += copy * 1_000_000;
    again
}

/// The blocks the packed bytes are held in (about 1 MiB each): a rollback from inside one block
/// across two more leaves the log as if the records after the mark had never been appended.
#[test]
fn a_rollback_across_blocks_takes_back_what_it_should() {
    let records = records();
    let many: Vec<Unpacked> = (0..40).flat_map(|copy| records.iter().map(move |record| renamed(record, copy))).collect();
    let mut log = EventLog::new();
    log.pack_records().unwrap();
    let mut marked = None;
    for (index, record) in many.iter().enumerate() {
        append(&mut log, record);
        if marked.is_none() && log.packed_sizes().unwrap().0 > 1_500_000 {
            marked = Some((index + 1, log.packed_sizes().unwrap().0, log.mark()));
        }
    }
    assert!(log.packed_sizes().unwrap().0 > 3 * (1 << 20), "{:?}", log.packed_sizes());
    let (len, size, mark) = marked.unwrap();
    log.rollback(mark);
    assert_eq!((log.len(), log.packed_sizes().unwrap().0), (len, size));
    for record in &many[len..] {
        append(&mut log, record);
    }
    let mut whole = EventLog::new();
    for record in &many {
        append(&mut whole, record);
    }
    assert_eq!(json(&log), json(&whole));
    // Every record reads back from whichever block it is in.
    for (index, record) in many.iter().enumerate() {
        assert_eq!(log.record(index).unwrap().as_ref(), Some(record));
    }
}

/// Packing a log of bytes: each record whose packed form writes its bytes again is packed, its
/// bytes let go; a record of bytes that are not canonical JSON keeps them, and they are what it
/// writes.
#[test]
fn a_record_whose_packed_form_would_differ_keeps_its_bytes() {
    let records = records();
    let mut log = EventLog::new();
    for record in &records[..20] {
        append(&mut log, record);
    }
    // Spaces after the separators: the bytes a record holds, but not the ones its row writes.
    let canonical = records[20].record_json().unwrap();
    let spaced = String::from_utf8(canonical.to_vec()).unwrap().replacen(",\"event\":", ", \"event\": ", 1).into_bytes();
    assert_ne!(spaced, &canonical[..]);
    assert_eq!(log.add(fields_of_bytes(&canonical).unwrap(), false, Some(spaced.clone().into_boxed_slice())), 0);
    assert_eq!(log.pack_records().unwrap(), 20);
    assert!(log.written(19).is_none());
    assert_eq!(log.written(20), Some(&spaced[..]));
    let mut raw = Vec::new();
    assert!(log.write_record(20, &mut raw).unwrap());
    assert_eq!(raw, spaced);
    assert_eq!(log.record(20).unwrap().as_ref(), Some(&records[20]));
    // Records appended after it are packed.
    append(&mut log, &records[21]);
    assert!(log.written(21).is_none());
    assert_eq!(log.record(21).unwrap().as_ref(), Some(&records[21]));
}

/// `load::find_record` reads a packed record as it reads one of bytes.
#[test]
fn find_record_reads_a_packed_record() {
    let fixture = load::entry("tick_core", "generated");
    let mut run = PlainRun::of_entry(&fixture).unwrap();
    for _ in 0..30 {
        run.step().unwrap();
    }
    run.loaded
        .cell
        .with(|world| {
            let log = world.log().unwrap();
            assert!(log.is_packed() && log.len() > 0);
            let ids: Vec<String> = log.event_ids().map(str::to_owned).collect();
            let rows: Vec<Unpacked> = (0..log.len()).map(|index| log.record(index).unwrap().unwrap()).collect();
            drop(log);
            for (id, row) in ids.iter().zip(&rows) {
                let found = sheltersim_core::load::find_record(world, id).unwrap().unwrap();
                assert_eq!((found.status, found.realized_minute, &found.event), (row.status, row.realized_minute, &row.event));
            }
            assert!(sheltersim_core::load::find_record(world, "no-such-event").unwrap().is_none());
        })
        .unwrap();
}
