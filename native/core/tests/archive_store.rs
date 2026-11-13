//! What a Run in the browser stores (`archive`, #252 PR 6-5): the sealed blocks of its packed log
//! and samples, taken as a Run goes, with the tables' new entries before them, and its last world
//! without the log; read back in that order, they give the Run's digests. Bytes that are not such
//! pieces are an error.

mod support;

use std::collections::HashMap;
use std::sync::Arc;

use sheltersim_core::archive::{world_part_digests, world_parts, LogReader, SampleReader};
use sheltersim_core::checkpoint::sha256_hex;
use sheltersim_core::events::log::EventLog;
use sheltersim_core::events::packed::{Blocks, Packer, Unpacked};
use sheltersim_core::plain::PlainRun;
use sheltersim_core::world::WorldCell;
use support::load;

/// The pieces stored so far, in order, and how much of each store they cover.
#[derive(Default)]
struct Stored {
    pieces: Vec<(&'static str, Vec<u8>)>,
    log: (usize, usize, usize),
    samples: (usize, usize, usize),
}

/// The new table entries and the newly sealed blocks (every block when `last`) of a store.
fn take(kind: (&'static str, &'static str), packer: &Packer, blocks: &Blocks, at: &mut (usize, usize, usize), last: bool, pieces: &mut Vec<(&'static str, Vec<u8>)>) {
    let (strings, shapes, written) = *at;
    let end = if last { blocks.blocks().len() } else { blocks.sealed() };
    if end > written {
        let mut tables = Vec::new();
        packer.write_tables(strings, shapes, &mut tables);
        pieces.push((kind.0, tables));
        for block in &blocks.blocks()[written..end] {
            pieces.push((kind.1, block.clone()));
        }
        *at = (packer.strings().len(), packer.shapes().len(), end);
    }
}

fn flush(run: &PlainRun, stored: &mut Stored, last: bool) {
    run.loaded
        .cell
        .with(|world| {
            let log = world.log().unwrap();
            let (packer, blocks) = log.packed_store().unwrap();
            take(("log_tables", "log_block"), packer, blocks, &mut stored.log, last, &mut stored.pieces);
        })
        .unwrap();
    let (packer, blocks) = run.host.samples.store();
    take(("sample_tables", "sample_block"), packer, blocks, &mut stored.samples, last, &mut stored.pieces);
}

fn read_back(pieces: &[(&'static str, Vec<u8>)]) -> (LogReader, SampleReader) {
    let mut log = LogReader::new();
    let mut samples = SampleReader::new();
    for (kind, bytes) in pieces {
        match *kind {
            "log_tables" => log.read_tables(bytes).unwrap(),
            "log_block" => drop(log.read_block(bytes).unwrap()),
            "sample_tables" => samples.read_tables(bytes).unwrap(),
            "sample_block" => drop(samples.read_block(bytes).unwrap()),
            other => panic!("{other}"),
        }
    }
    (log, samples)
}

/// Each world of `tick_core.json`, stored every 60 ticks and at the end: read back, its log, its
/// samples and its last world give the Run's digests.
#[test]
fn a_stored_run_reads_back_to_the_run_s_digests() {
    for name in ["small_replay", "generated"] {
        let fixture = load::entry("tick_core", name);
        let ticks = fixture["expected"]["ticks"].as_i64().unwrap();
        let mut run = PlainRun::of_entry(&fixture).unwrap();
        let mut stored = Stored::default();
        for at in 0..ticks {
            run.step().unwrap();
            if at % 60 == 59 {
                flush(&run, &mut stored, false);
            }
        }
        flush(&run, &mut stored, true);
        let digests: HashMap<&str, String> = run.digests().unwrap().into_iter().collect();
        let (log, samples) = read_back(&stored.pieces);
        let count = run.loaded.cell.with(|world| world.log().unwrap().len()).unwrap();
        assert!(run.loaded.cell.with(|world| world.log().unwrap().all_packed()).unwrap(), "{name}");
        assert_eq!(log.records(), count as u64, "{name}");
        assert_eq!(log.digest(), digests["event_log"], "{name}");
        assert_eq!(samples.samples(), run.host.samples.len() as u64, "{name}");
        assert_eq!(samples.digest(), run.host.metrics_digest(), "{name}");
        assert_eq!(samples.digest(), fixture["expected"]["metrics"].as_str().unwrap(), "{name}");
        let world = run.loaded.cell.with(|world| world_parts(world).unwrap()).unwrap();
        let parts = world_part_digests(&world).unwrap();
        assert_eq!(parts.len(), digests.len() - 1, "{name}");
        for (part, digest) in &parts {
            assert_eq!(&digests[part.as_str()], digest, "{name}: {part}");
        }
        // A world piece cut short, or with bytes after it, is not one.
        assert!(world_part_digests(&world[..world.len() - 1]).is_err());
        let mut longer = world.clone();
        longer.push(0);
        assert!(world_part_digests(&longer).is_err());
    }
}

/// A log of several blocks, stored as they seal while records are appended and rolled back: read
/// back, the digest of what the log holds at the end.
#[test]
fn sealed_blocks_stored_as_records_go_read_back_to_the_log() {
    let fixture = load::entry("tick_core", "generated");
    let mut run = PlainRun::of_entry(&fixture).unwrap();
    for _ in 0..fixture["expected"]["ticks"].as_i64().unwrap() {
        run.step().unwrap();
    }
    let records: Vec<Unpacked> = run
        .loaded
        .cell
        .with(|world| {
            let log = world.log().unwrap();
            (0..log.len()).map(|index| log.record(index).unwrap().unwrap()).collect()
        })
        .unwrap();
    let mut log = EventLog::new();
    log.pack_records().unwrap();
    let mut pieces = Vec::new();
    let mut at = (0, 0, 0);
    let mut sealed_while_going = 0;
    for copy in 0..40 {
        // Each copy appended in a transaction, the last ten of it rolled back and appended again.
        for (index, record) in records.iter().enumerate() {
            let mut again = record.clone();
            again.event.event_id = Arc::from(format!("{}.{copy}", record.event.event_id));
            again.event.sequence += copy * 1_000_000;
            if index == records.len() - 10 {
                let mark = log.mark();
                for later in &records[index..] {
                    let mut taken = later.clone();
                    taken.event.event_id = Arc::from(format!("{}.{copy}.taken", later.event.event_id));
                    taken.event.sequence += copy * 1_000_000 + 500_000;
                    log.append_row(&taken.event, taken.realized_minute, taken.status, taken.detail.as_deref()).unwrap();
                }
                log.rollback(mark);
            }
            log.append_row(&again.event, again.realized_minute, again.status, again.detail.as_deref()).unwrap();
        }
        let (packer, blocks) = log.packed_store().unwrap();
        let before = at.2;
        take(("log_tables", "log_block"), packer, blocks, &mut at, false, &mut pieces);
        sealed_while_going += at.2 - before;
    }
    let (packer, blocks) = log.packed_store().unwrap();
    take(("log_tables", "log_block"), packer, blocks, &mut at, true, &mut pieces);
    assert!(sealed_while_going >= 2, "{sealed_while_going} blocks sealed as records went");
    let (reader, _) = read_back(&pieces);
    let mut json = vec![b'['];
    for index in 0..log.len() {
        if index > 0 {
            json.push(b',');
        }
        assert!(log.write_record(index, &mut json).unwrap());
    }
    json.push(b']');
    assert_eq!(reader.records(), log.len() as u64);
    assert_eq!(reader.digest(), sha256_hex(&json));
}

/// Tables that skip entries or repeat them, blocks read before their tables, and bytes cut short
/// are errors; a reader that refused a table is as it was.
#[test]
fn pieces_out_of_order_or_changed_are_errors() {
    let fixture = load::entry("tick_core", "small_replay");
    let mut run = PlainRun::of_entry(&fixture).unwrap();
    for _ in 0..120 {
        run.step().unwrap();
    }
    let mut stored = Stored::default();
    flush(&run, &mut stored, true);
    let tables = &stored.pieces.iter().find(|(kind, _)| *kind == "log_tables").unwrap().1;
    let block = &stored.pieces.iter().find(|(kind, _)| *kind == "log_block").unwrap().1;
    // A block before its tables names strings no table holds.
    assert!(LogReader::new().read_block(block).is_err());
    let mut reader = LogReader::new();
    reader.read_tables(tables).unwrap();
    // The same entries again: they do not follow the table's.
    assert!(reader.read_tables(tables).is_err());
    assert_eq!(reader.read_block(block).unwrap() as u64, reader.records());
    for end in [0, 1, tables.len() / 2, tables.len() - 1] {
        assert!(LogReader::new().read_tables(&tables[..end]).is_err(), "cut at {end}");
    }
    let mut longer = tables.clone();
    longer.push(0);
    assert!(LogReader::new().read_tables(&longer).is_err());
    // Entries that start past the table's end leave a gap.
    let (packer, _) = run.loaded.cell.with(|world| world.log().unwrap().packed_store().map(|(packer, blocks)| (packer.clone(), blocks.clone())).unwrap()).unwrap();
    let mut gap = Vec::new();
    packer.write_tables(1, 0, &mut gap);
    assert!(LogReader::new().read_tables(&gap).is_err());
    for end in 0..block.len().min(200) {
        let mut cut = LogReader::new();
        cut.read_tables(tables).unwrap();
        let _ = cut.read_block(&block[..end]);
    }
    let samples = &stored.pieces.iter().find(|(kind, _)| *kind == "sample_block").unwrap().1;
    assert!(SampleReader::new().read_block(samples).is_err());
}
