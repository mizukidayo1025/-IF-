"""The history index stores each new record once with its entity and search rows,
whether a write holds a few records or more than one batch of them (#47).

A record already stored, or put earlier in the same write, adds nothing again, and a
different payload under its key is refused. A refused write and a write over the
storage capacity leave the index as it was. That a saved Run's index answers every
history kind is checked in tests/integration/test_project_evidence_routes.py.
"""

from __future__ import annotations

import json
import shutil
import sqlite3
import zlib
from pathlib import Path
from typing import Any

import pytest
from support.paths import SAVED_RUN_ENGINE_V1

from sheltersim.contracts.workspace import HistoryQuery
from sheltersim.storage import history_payloads
from sheltersim.storage.history_index import HistoryIndex, _RecordBatch
from sheltersim.storage.projects import ProjectStore, StorageError


@pytest.fixture
def index(tmp_path) -> HistoryIndex:
    store = ProjectStore(tmp_path)
    (tmp_path / "p" / "runs" / "r").mkdir(parents=True)
    index = HistoryIndex(store, "p", "r")
    with index.connect(write=True):  # An empty index with its tables.
        pass
    return index


def _record(number: int) -> dict:
    return {
        "minute": number,
        "event_id": f"event-{number}",
        "agent_id": f"agent-{number % 7}",
        "event_type": "social_interaction",
        "status": "applied",
    }


def _put(index: HistoryIndex, *rows: tuple[str, dict, str | None]) -> None:
    with index.connect(write=True) as db, _RecordBatch(db, "main") as records:
        for kind, row, key in rows:
            records.put(kind, row, key=key)


def _counts(index: HistoryIndex) -> dict:
    with sqlite3.connect(index.path.as_uri() + "?mode=ro", uri=True) as db:
        return {
            table: db.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0]
            for table in ("records", "entities", "tokens")
        } | {"generations": dict(db.execute("SELECT kind,value FROM generations").fetchall())}


def test_records_beyond_one_batch_are_all_stored_with_entities_and_tokens(index):
    count = 2 * _RecordBatch._FLUSH_RECORDS + 3
    _put(index, *(("events", _record(number), None) for number in range(count)))
    # Each record has two identities (event_id, agent_id) and three tokens
    # (event_type, status and its own event link).
    assert _counts(index) == {
        "records": count,
        "entities": 2 * count,
        "tokens": 3 * count,
        "generations": {"events": count},
    }
    with sqlite3.connect(index.path.as_uri() + "?mode=ro", uri=True) as db:
        keys = db.execute(
            "SELECT key FROM tokens WHERE token='@event:event-1000' AND kind='events'"
        ).fetchall()
        stored = db.execute(
            "SELECT key FROM records WHERE kind='events' AND event_id='event-1000'"
        ).fetchall()
    assert keys == stored and len(keys) == 1


def test_a_repeated_record_is_stored_once_within_and_across_writes(index):
    _put(
        index, ("events", _record(1), "k"), ("events", _record(1), "k"), ("kpis", _record(2), None)
    )
    _put(index, ("events", _record(1), "k"), ("kpis", _record(2), None))
    assert _counts(index) == {
        "records": 2,
        "entities": 4,
        "tokens": 6,
        "generations": {"events": 1, "kpis": 1},
    }


@pytest.mark.parametrize("first_put", ["in_an_earlier_write", "earlier_in_this_batch", "flushed"])
def test_a_changed_payload_under_a_kept_key_is_refused_and_nothing_is_written(index, first_put):
    if first_put == "in_an_earlier_write":
        _put(index, ("events", _record(1), "k"))
    before = _counts(index)
    rows = [("events", _record(number), None) for number in range(2, 600)]
    changed = ("events", {**_record(1), "status": "rejected"}, "k")
    if first_put == "earlier_in_this_batch":
        rows[:0] = [("events", _record(1), "k"), changed]
    elif first_put == "flushed":
        rows = [("events", _record(1), "k"), *rows, changed]
    else:
        rows.append(changed)
    with pytest.raises(StorageError, match="immutable_history_record_conflict"):
        _put(index, *rows)
    assert _counts(index) == before


def test_an_index_write_over_the_storage_capacity_is_refused_and_rolled_back(index):
    _put(index, ("events", _record(1), None))
    before = _counts(index)
    index.store.max_bytes = index.store.usage()["bytes"] + 1_000
    with pytest.raises(StorageError, match="storage_capacity_exceeded"):
        _put(index, *(("events", _record(number), None) for number in range(2, 2_000)))
    assert _counts(index) == before


def test_an_index_left_in_wal_mode_returns_to_the_rollback_journal(index):
    """#268 wrote indexes with a write-ahead log, a mode kept in the file; the next
    write returns them to the rollback journal (#269)."""
    with sqlite3.connect(index.path) as db:
        assert db.execute("PRAGMA journal_mode=WAL").fetchone()[0] == "wal"
    _put(index, ("events", _record(1), None))
    with sqlite3.connect(index.path.as_uri() + "?mode=ro", uri=True) as db:
        assert db.execute("PRAGMA journal_mode").fetchone()[0] == "delete"
    assert not index.path.with_name(index.path.name + "-wal").exists()
    assert _counts(index)["records"] == 1


def test_a_branch_reads_its_ancestors_before_the_smallest_fork_minute(index):
    """#256: lineage rows name each branch's parent and fork minute."""
    from sheltersim.storage.history_index import _scope, _segments

    with index.connect(write=True) as db:
        db.execute("INSERT INTO lineage VALUES('a','main',50)")
        db.execute("INSERT INTO lineage VALUES('b','a',30)")
        db.execute("INSERT INTO lineage VALUES('c','main',70)")
        assert _segments(db, "main") == [("main", None)]
        assert _segments(db, "a") == [("a", None), ("main", 50)]
        assert _segments(db, "b") == [("b", None), ("a", 30), ("main", 30)]
        assert _segments(db, "c") == [("c", None), ("main", 70)]
        scope, args = _scope(_segments(db, "b"))
        assert scope == "(r.branch=? OR (r.branch=? AND r.minute<?) OR (r.branch=? AND r.minute<?))"
        assert args == ["b", "a", 30, "main", 30]


def test_a_forked_branch_stores_no_row_its_parent_holds_before_the_fork_minute(index):
    _put(index, ("events", _record(10), "early"), ("events", _record(60), "late"))
    with index.connect(write=True) as db:
        db.execute("INSERT INTO lineage VALUES('child','main',50)")
    with index.connect(write=True) as db, _RecordBatch(db, "child") as records:
        records.ancestors = [("main", 50)]
        records.put("events", _record(10), key="early")
        records.put("events", _record(60), key="late")
    with sqlite3.connect(index.path.as_uri() + "?mode=ro", uri=True) as db:
        stored = db.execute("SELECT key FROM records WHERE branch='child'").fetchall()
    # The parent's row at minute 60 is after the fork, so the child stores its own.
    assert stored == [("late",)]
    with pytest.raises(StorageError, match="immutable_history_record_conflict"):
        with index.connect(write=True) as db, _RecordBatch(db, "child") as records:
            records.ancestors = [("main", 50)]
            records.put("events", {**_record(10), "status": "rejected"}, key="early")


def test_a_version_one_index_is_read_and_upgraded_and_a_newer_one_refused(index):
    """Version 1 indexes have no lineage and read the same; a write makes them version 3,
    whose new rows hold compressed payloads (#80). An index of a version this code does
    not know is refused, read or written."""
    _put(index, ("events", _record(1), "k"))
    with sqlite3.connect(index.path) as db:
        db.execute("DROP TABLE lineage")
        db.execute("PRAGMA user_version=1")
    with index.connect() as db:
        assert db.execute("SELECT COUNT(*) FROM records").fetchone()[0] == 1
    with index.connect(write=True):
        pass
    with sqlite3.connect(index.path) as db:
        assert db.execute("PRAGMA user_version").fetchone()[0] == 3
        assert db.execute("SELECT COUNT(*) FROM lineage").fetchone()[0] == 0
        db.execute("PRAGMA user_version=4")
    for write in (False, True):
        with pytest.raises(StorageError, match="history_index_version_unsupported"):
            with index.connect(write=write):
                pass


def _turn_keys(index: HistoryIndex) -> list[str]:
    # The rows HistoryIndex.conversation_tail reads for one conversation.
    with index.connect() as db:
        rows = db.execute(
            "SELECT e.key FROM entities e JOIN records r ON r.branch=e.branch AND r.kind=e.kind "
            "AND r.key=e.key WHERE e.branch=? AND e.kind='conversations' AND e.entity=? "
            "ORDER BY e.key DESC LIMIT ?",
            ("main", "talk-1", 5),
        ).fetchall()
    return [row[0] for row in rows]


def test_only_a_conversation_has_turn_order_rows_and_its_query_reads_them(index):
    turn = {"turn_id": "turn-1", "conversation_id": "talk-1", "minute": 1}
    _put(index, ("conversations", turn, "turn-1"), ("events", _record(1), None))
    with index.connect() as db:
        row_count = db.execute(
            "SELECT COUNT(*) FROM entities INDEXED BY entity_turn_order WHERE kind='conversations'"
        ).fetchone()[0]
        total = db.execute("SELECT COUNT(*) FROM entities").fetchone()[0]
        plan = db.execute(
            "EXPLAIN QUERY PLAN SELECT e.key FROM entities e WHERE e.branch=? "
            "AND e.kind='conversations' AND e.entity=? ORDER BY e.key DESC LIMIT ?",
            ("main", "talk-1", 5),
        ).fetchall()
    assert 0 < row_count < total
    assert "entity_turn_order" in " ".join(row["detail"] for row in plan)
    assert _turn_keys(index) == ["turn-1"]


def test_a_version_two_index_with_a_full_turn_order_index_gives_the_same_turns(index):
    with sqlite3.connect(index.path) as db:
        db.execute("DROP INDEX entity_turn_order")
        db.execute("CREATE INDEX entity_turn_order ON entities(branch,kind,entity,key)")
    turn = {"turn_id": "turn-1", "conversation_id": "talk-1", "minute": 1}
    _put(index, ("conversations", turn, "turn-1"))
    assert _turn_keys(index) == ["turn-1"]


def test_opening_a_current_index_to_write_changes_nothing_in_the_file(index):
    before = index.path.read_bytes()
    with index.connect(write=True):
        pass
    assert index.path.read_bytes() == before


def test_a_kept_writer_sees_other_writers_and_survives_a_failed_write(index):
    kept = HistoryIndex(index.store, "p", "r", keep_writer=True)
    try:
        for number, writer in enumerate((kept, index, kept, kept, index)):
            _put(writer, ("events", _record(number), None))
        assert _counts(index)["records"] == 5
        with pytest.raises(RuntimeError), kept.connect(write=True) as db:
            db.execute("DELETE FROM records")
            raise RuntimeError("the caller's own failure")
        assert _counts(index)["records"] == 5
        _put(kept, ("events", _record(5), None))
        assert _counts(index)["records"] == 6
    finally:
        kept.close()
    # A second close, and a new connection after it, are fine.
    kept.close()
    _put(kept, ("events", _record(6), None))
    kept.close()
    assert _counts(index)["records"] == 7


def _payload_forms(index: HistoryIndex) -> list[str]:
    with sqlite3.connect(index.path.as_uri() + "?mode=ro", uri=True) as db:
        return [row[0] for row in db.execute("SELECT typeof(payload) FROM records ORDER BY key")]


def test_a_stored_payload_is_compared_as_text_whatever_bytes_hold_it(index):
    """#80: a kept key takes the same payload again and refuses another one, whether its
    row holds compressed bytes, bytes another zlib build compressed, or text (as before
    version 3). Its own branch's rows and an ancestor's are compared alike."""
    _put(index, ("events", _record(1), "compressed"))
    with index.connect(write=True) as db:
        stored = db.execute("SELECT * FROM records WHERE key='compressed'").fetchone()
        text = history_payloads.decode(stored["payload"])
        # The same text in other compressed bytes, as another level or zlib build makes.
        other = zlib.compressobj(
            1, zlib.DEFLATED, -15, 9, zlib.Z_DEFAULT_STRATEGY, zdict=history_payloads.dictionary(1)
        )
        relevel = bytes((1,)) + other.compress(text.encode()) + other.flush()
        assert relevel != history_payloads.encode(text)
        for key, payload in (("other_bytes", relevel), ("text", text)):
            db.execute(
                "INSERT INTO records VALUES(?,?,?,?,?,?,?,?,?,?,?)",
                (*tuple(stored)[:2], key, *tuple(stored)[3:9], payload, stored["payload_hash"]),
            )
        db.execute("INSERT INTO lineage VALUES('child','main',50)")
    assert _payload_forms(index) == ["blob", "blob", "text"]

    def stored_rows() -> list[Any]:
        # Nothing is written again: the same rows, in the same bytes.
        with sqlite3.connect(index.path.as_uri() + "?mode=ro", uri=True) as db:
            return db.execute("SELECT * FROM records ORDER BY branch,key").fetchall()

    before = stored_rows()
    keys = ("compressed", "other_bytes", "text")
    _put(index, *(("events", _record(1), key) for key in keys))
    assert stored_rows() == before
    for key in keys:
        with pytest.raises(StorageError, match="immutable_history_record_conflict"):
            _put(index, ("events", {**_record(1), "status": "rejected"}, key))
        with index.connect(write=True) as db, _RecordBatch(db, "child") as records:
            records.ancestors = [("main", 50)]
            records.put("events", _record(1), key=key)
        with pytest.raises(StorageError, match="immutable_history_record_conflict"):
            with index.connect(write=True) as db, _RecordBatch(db, "child") as records:
                records.ancestors = [("main", 50)]
                records.put("events", {**_record(1), "status": "rejected"}, key=key)
    assert stored_rows() == before


def _saved_run_answers(root: Path) -> dict[str, Any]:
    """What the index of a copy of fixtures/saved_runs/engine-v1-small answers: every
    page of each kind, filters, causal links, Checkpoints and conversation turns."""
    store = ProjectStore(root)
    index = HistoryIndex(store, "p", "r")
    answers: dict[str, Any] = {}
    for branch in ("main", "child"):
        reference = store.branch_reference("p", "r", branch)
        event = {"kind": "events", "event_id": "action-lifecycle-1"}
        for label, fields in (
            ("events", {"kind": "events"}),
            ("kpis", {"kind": "kpis"}),
            ("actions", {"kind": "actions"}),
            ("agent", {"kind": "actions", "agent_id": "A001"}),
            ("text", {"kind": "events", "text": "agent_travel_complete"}),
            ("event", event),
            ("causal", {**event, "kind": "causal"}),
            ("related", {**event, "kind": "causal", "include_related": True}),
        ):
            pages: list[dict[str, Any]] = []
            cursor = None
            while True:
                query = HistoryQuery(end=90, limit=7, cursor=cursor, **fields)
                pages.append(index.search(branch, query, reference.checkpoint_hash))
                cursor = pages[-1]["next_cursor"]
                if not cursor:
                    break
            answers[f"{branch}/{label}"] = pages
        answers[f"{branch}/checkpoints"] = index.checkpoints(
            branch, reference.fork_minute, 60, 100, None
        )
        answers[f"{branch}/tail"] = index.conversation_tail(branch, None, 100)
    return answers


def test_a_version_two_saved_index_answers_the_same_compressed_and_takes_new_rows(tmp_path):
    """#80: the index of a saved Run, version 2 with text payloads, answers as it did
    when every payload is compressed instead; a write makes it version 3, and its new
    rows are compressed beside the old text ones, which keep answering the same."""
    roots = {name: tmp_path / name for name in ("text", "compressed")}
    for root in roots.values():
        shutil.copytree(SAVED_RUN_ENGINE_V1, root)
    indexes = {name: HistoryIndex(ProjectStore(root), "p", "r") for name, root in roots.items()}
    assert set(_payload_forms(indexes["text"])) == {"text"}
    with sqlite3.connect(indexes["compressed"].path) as db:
        assert db.execute("PRAGMA user_version").fetchone()[0] == 2
        rows = db.execute("SELECT rowid,payload FROM records").fetchall()
        db.executemany(
            "UPDATE records SET payload=? WHERE rowid=?",
            [(history_payloads.encode(payload), rowid) for rowid, payload in rows],
        )
        db.execute("PRAGMA user_version=3")
    assert set(_payload_forms(indexes["compressed"])) == {"blob"}
    answers = _saved_run_answers(roots["text"])
    assert sum(len(page["items"]) for page in answers["main/events"]) == 17
    assert answers["main/agent"][0]["items"] and answers["main/causal"][0]["items"]
    assert json.dumps(_saved_run_answers(roots["compressed"])) == json.dumps(answers)

    index = indexes["text"]
    turn = {"turn_id": "turn-1", "conversation_id": "talk-1", "minute": 61}
    index.add_turn("main", turn)
    # After every queried minute, so the old answers stay whole.
    _put(index, ("events", {**_record(100), "event_id": "event-late"}, "late"))
    with sqlite3.connect(index.path) as db:
        assert db.execute("PRAGMA user_version").fetchone()[0] == 3
        forms = dict(
            db.execute("SELECT key,typeof(payload) FROM records WHERE key IN ('turn-1','late')")
        )
    assert forms == {"turn-1": "blob", "late": "blob"}
    assert _payload_forms(index).count("text") == len(rows)
    assert index.conversation_tail("main", "talk-1", 5) == [turn]
    again = _saved_run_answers(roots["text"])
    assert again.pop("main/tail") == [turn]
    answers.pop("main/tail")
    assert json.dumps(again) == json.dumps(answers)
