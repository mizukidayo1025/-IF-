//! What a Run in the browser stores (`archive`, #252 PR 6-5): the sealed blocks of its packed log
//! and samples, taken as a Run goes, with the tables' new entries before them, and its last world
//! without the log; read back in that order, they give the Run's digests. Bytes that are not such
//! pieces are an error.
//!
//! Saved points (#251): the Run at a minute, restored from its saved point and the stored log's and
//! samples' first records and samples, has the Run's digests at that minute and goes on to the
//! Run's end with the same digests.

mod support;

use std::collections::HashMap;
use std::sync::Arc;

use sheltersim_core::canonical::{state_digest, TREE_CHUNK_ITEMS};
use sheltersim_core::frame::Request;
use sheltersim_core::archive::{world_part_digests, world_parts, LogReader, Restore, SampleReader, SavedPoint, SAVED_POINT_VERSION};
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

/// The Run's digests now: each part's and the samples'.
fn digests_of(run: &PlainRun) -> (Vec<(&'static str, String)>, String) {
    (run.digests().unwrap(), run.host.metrics_digest())
}

/// The Run at a saved point: the saved point, then the stored pieces in order until it has the
/// records and samples it needs.
fn restored(fixture: &serde_json::Value, point: &[u8], pieces: &[(&'static str, Vec<u8>)]) -> sheltersim_core::error::Result<PlainRun> {
    let mut restore = Restore::new(point)?;
    for (kind, bytes) in pieces {
        if !restore.log_needed() && !restore.samples_needed() {
            break;
        }
        match *kind {
            "log_tables" => restore.read_log_tables(bytes)?,
            "log_block" => drop(restore.read_log_block(bytes)?),
            "sample_tables" => restore.read_sample_tables(bytes)?,
            "sample_block" => drop(restore.read_sample_block(bytes)?),
            other => panic!("{other}"),
        }
    }
    PlainRun::restore(fixture.clone(), restore)
}

/// A Run of each world of `tick_core.json` saved every `every` minutes as it goes (`small_replay`
/// is one day, saved every 6 hours): restored at each saved point, the Run has the digests the Run
/// had at that minute and saves the same point again; restored at a minute in the middle, it goes
/// on to the end with the Run's digests and the native path's.
#[test]
fn a_run_restored_at_a_saved_point_goes_on_as_the_run_did() {
    for (name, every) in [("small_replay", 360), ("generated", 60)] {
        let fixture = load::entry("tick_core", name);
        let ticks = fixture["expected"]["ticks"].as_i64().unwrap();
        let mut run = PlainRun::of_entry(&fixture).unwrap();
        let mut stored = Stored::default();
        let mut points = Vec::new();
        for at in 1..=ticks {
            run.step().unwrap();
            if at % 60 == 0 {
                flush(&run, &mut stored, false);
            }
            if at % every == 0 {
                points.push((at, run.saved_point().unwrap().to_bytes(), digests_of(&run)));
            }
        }
        flush(&run, &mut stored, true);
        let end = digests_of(&run);
        assert!(points.len() >= 4, "{name}: {} saved points", points.len());
        for (minute, point, digests) in &points {
            let again = restored(&fixture, point, &stored.pieces).unwrap();
            assert_eq!(again.minute().unwrap(), *minute, "{name}");
            assert_eq!(&digests_of(&again), digests, "{name} at {minute}");
            assert_eq!(&again.saved_point().unwrap().to_bytes(), point, "{name} at {minute}");
        }
        let (minute, point, _) = &points[points.len() / 2 - 1];
        let mut again = restored(&fixture, point, &stored.pieces).unwrap();
        for _ in *minute..ticks {
            again.step().unwrap();
        }
        assert_eq!(digests_of(&again), end, "{name} from {minute}");
        assert_eq!(again.host.metrics_digest(), fixture["expected"]["metrics"].as_str().unwrap(), "{name}");
        assert_eq!(again.host.contracts, run.host.contracts, "{name}");
    }
}

/// A Run that seals its blocks at each saved point and stores them before it (#267): every saved
/// point is restored from the pieces stored before it alone. Restored there, the Run stores its
/// own pieces after them, as the Run went on: the store then reads back to the end digests of the
/// Run that never stopped, and so does a store that goes on from several points in turn. Sealing
/// changes no digest.
#[test]
fn a_run_sealed_at_its_saved_points_goes_on_storing_after_a_restore() {
    for (name, every) in [("small_replay", 360), ("generated", 60)] {
        let fixture = load::entry("tick_core", name);
        let ticks = fixture["expected"]["ticks"].as_i64().unwrap();
        let mut plain = PlainRun::of_entry(&fixture).unwrap();
        for _ in 0..ticks {
            plain.step().unwrap();
        }
        let end = digests_of(&plain);
        // The Run sealed and saved every `every` minutes, its pieces stored every 25 ticks.
        let mut run = PlainRun::of_entry(&fixture).unwrap();
        let mut stored = Stored::default();
        let mut points = Vec::new();
        for at in 1..=ticks {
            run.step().unwrap();
            if at % 25 == 0 {
                flush(&run, &mut stored, false);
            }
            if at % every == 0 && at < ticks {
                run.seal_stores().unwrap();
                flush(&run, &mut stored, false);
                points.push((at, run.saved_point().unwrap().to_bytes(), stored.pieces.len(), stored.log, stored.samples, digests_of(&run)));
            }
        }
        flush(&run, &mut stored, true);
        assert_eq!(digests_of(&run), end, "{name}: sealing changed a digest");
        assert!(points.len() >= 3, "{name}: {} saved points", points.len());
        // Each point from the pieces stored before it, then on to the end storing after them.
        for (minute, point, pieces, log, samples, digests) in &points {
            let mut again = restored(&fixture, point, &stored.pieces[..*pieces]).unwrap();
            assert_eq!(&digests_of(&again), digests, "{name} at {minute}");
            let mut going = Stored { pieces: stored.pieces[..*pieces].to_vec(), log: *log, samples: *samples };
            for at in *minute + 1..=ticks {
                again.step().unwrap();
                if at % 25 == 0 {
                    flush(&again, &mut going, false);
                }
            }
            flush(&again, &mut going, true);
            assert_eq!(digests_of(&again), end, "{name} from {minute}");
            let (log_back, samples_back) = read_back(&going.pieces);
            assert_eq!(log_back.digest(), end.0.iter().find(|(part, _)| *part == "event_log").unwrap().1, "{name} from {minute}: the stored log");
            assert_eq!(samples_back.digest(), end.1, "{name} from {minute}: the stored samples");
        }
        // Stopped and restored at every point in turn, each time from what the last one stored.
        let mut going = Stored::default();
        let mut again = PlainRun::of_entry(&fixture).unwrap();
        for at in 1..=ticks {
            again.step().unwrap();
            if at % every == 0 && at < ticks {
                again.seal_stores().unwrap();
                flush(&again, &mut going, false);
                let point = again.saved_point().unwrap().to_bytes();
                again = restored(&fixture, &point, &going.pieces).unwrap();
            }
        }
        flush(&again, &mut going, true);
        assert_eq!(digests_of(&again), end, "{name}: restored at every point");
        let (log_back, samples_back) = read_back(&going.pieces);
        assert_eq!(log_back.records(), again.loaded.cell.with(|world| world.log().unwrap().len()).unwrap() as u64, "{name}");
        assert_eq!(samples_back.digest(), end.1, "{name}: restored at every point, the stored samples");
    }
}

/// The world's digest without the log's node a Run keeps (#266): every record read again.
fn uncached_digest(run: &PlainRun) -> String {
    run.loaded.cell.with(|world| state_digest(world, &run.rng_state)).unwrap().unwrap()
}

/// A Run keeps its log's node between digests (#266) and reads only the records added since: its
/// digest is the uncached one after each tick, across the log's runs of `TREE_CHUNK_ITEMS`
/// records, and in a Run restored at a saved point, whose first digest builds the node.
#[test]
fn a_run_s_kept_digest_of_its_log_is_the_digest_read_again() {
    for (name, every) in [("small_replay", 360), ("generated", 60)] {
        let fixture = load::entry("tick_core", name);
        let ticks = fixture["expected"]["ticks"].as_i64().unwrap();
        let mut run = PlainRun::of_entry(&fixture).unwrap();
        let mut stored = Stored::default();
        let mut points = Vec::new();
        let records = |run: &PlainRun| run.loaded.cell.with(|world| world.log().unwrap().len()).unwrap();
        let mut runs_crossed = 0;
        let mut last = records(&run);
        assert_eq!(run.state_digest().unwrap(), uncached_digest(&run), "{name} at 0");
        for at in 1..=ticks {
            run.step().unwrap();
            // The kept digest after every tick; the uncached one (slow) every 10 ticks and where
            // the log closed a run of records the node held open.
            let kept = run.state_digest().unwrap();
            let now = records(&run);
            let crossed = now / TREE_CHUNK_ITEMS > last / TREE_CHUNK_ITEMS && last % TREE_CHUNK_ITEMS != 0;
            if crossed || at % 10 == 0 {
                assert_eq!(kept, uncached_digest(&run), "{name} at {at}");
            }
            runs_crossed += usize::from(crossed);
            last = now;
            if at % 60 == 0 {
                flush(&run, &mut stored, false);
            }
            if at % every == 0 {
                points.push((at, run.saved_point().unwrap().to_bytes()));
            }
        }
        flush(&run, &mut stored, true);
        assert!(runs_crossed >= 2, "{name}: the log crossed {runs_crossed} runs of records with one open");
        let (minute, point) = &points[points.len() / 2 - 1];
        let mut again = restored(&fixture, point, &stored.pieces).unwrap();
        assert_eq!(again.state_digest().unwrap(), uncached_digest(&again), "{name} restored at {minute}");
        for at in *minute + 1..(*minute + 30).min(ticks) {
            again.step().unwrap();
            let frame: serde_json::Value = serde_json::from_slice(&again.frame(&Request { run_id: "run", branch_id: "main", computed_until: at, state_hash: None }).unwrap()).unwrap();
            assert_eq!(frame["state_hash"], uncached_digest(&again).as_str(), "{name} restored, at {at}");
        }
    }
}

/// A saved point of another version, cut short or changed, and a store that ends before the saved
/// point's records or samples, are errors, never a panic.
#[test]
fn saved_points_that_are_not_ones_are_errors() {
    let fixture = load::entry("tick_core", "small_replay");
    let mut run = PlainRun::of_entry(&fixture).unwrap();
    for _ in 0..120 {
        run.step().unwrap();
    }
    let mut stored = Stored::default();
    flush(&run, &mut stored, true);
    let point = run.saved_point().unwrap();
    let bytes = point.to_bytes();
    assert_eq!(SavedPoint::of_bytes(&bytes).unwrap(), point);
    let mut other = bytes.clone();
    other[4] = (SAVED_POINT_VERSION + 1) as u8;
    let error = SavedPoint::of_bytes(&other).unwrap_err().to_string();
    assert!(error.contains("version"), "{error}");
    for end in [0, 3, 4, 5, 8, bytes.len() / 2, bytes.len() - 1] {
        assert!(restored(&fixture, &bytes[..end], &stored.pieces).is_err(), "cut at {end}");
    }
    let mut longer = bytes.clone();
    longer.push(0);
    assert!(restored(&fixture, &longer, &stored.pieces).is_err());
    for at in (0..bytes.len()).step_by(bytes.len() / 97 + 1) {
        let mut changed = bytes.clone();
        changed[at] ^= 0x5a;
        let _ = restored(&fixture, &changed, &stored.pieces);
    }
    // Only the samples, or only the log: the store ends before the saved point's.
    let samples: Vec<_> = stored.pieces.iter().filter(|(kind, _)| kind.starts_with("sample")).cloned().collect();
    assert!(restored(&fixture, &bytes, &samples).is_err());
    let log: Vec<_> = stored.pieces.iter().filter(|(kind, _)| kind.starts_with("log")).cloned().collect();
    assert!(restored(&fixture, &bytes, &log).is_err());
    // A saved point whose world is of another minute than its own.
    let mut moved = point.clone();
    moved.minute += 1;
    assert!(restored(&fixture, &moved.to_bytes(), &stored.pieces).is_err());
    assert!(restored(&fixture, &bytes, &stored.pieces).is_ok());
}
