"""The history index stores each new record once with its entity and search rows,
whether a write holds a few records or more than one batch of them (#47).

A record already stored, or put earlier in the same write, adds nothing again, and a
different payload under its key is refused. A refused write and a write over the
storage capacity leave the index as it was. Since version 4 (#130) a record's key is
stored once, the rows about it name its id, and repeated texts are rows of names. Since
version 5 (#138) a row made from an EventLog record holds the record's position instead of
its payload (tests/integration/test_history_index_positions.py). An index of an older
version is refused, but for its saved points, until it is rebuilt.
That a saved Run's index answers every history kind is checked in
tests/integration/test_project_evidence_routes.py.
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
from sheltersim.storage import history_index, history_payloads
from sheltersim.storage.history_index import (
    KINDS,
    HistoryIndex,
    _conversation_turns,
    _RecordBatch,
)
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
            for table in ("records", "entities", "tokens", "links", "names")
        } | {"generations": dict(db.execute("SELECT kind,value FROM generations").fetchall())}


def _name(db: sqlite3.Connection, name: str) -> int:
    return db.execute("SELECT id FROM names WHERE name=?", (name,)).fetchone()[0]


def test_records_beyond_one_batch_are_all_stored_with_entities_and_tokens(index):
    count = 2 * _RecordBatch._FLUSH_RECORDS + 3
    _put(index, *(("events", _record(number), None) for number in range(count)))
    # Each record has two identities (event_id, agent_id), two tokens (event_type and
    # status) and a link to its own event. Names are stored once: the branch, every event
    # id, seven agents, the type and the status.
    assert _counts(index) == {
        "records": count,
        "entities": 2 * count,
        "tokens": 2 * count,
        "links": count,
        "names": 1 + count + 7 + 2,
        "generations": {"events": count},
    }
    with sqlite3.connect(index.path.as_uri() + "?mode=ro", uri=True) as db:
        event = _name(db, "event-1000")
        linked = db.execute("SELECT record FROM links WHERE event=?", (event,)).fetchall()
        stored = db.execute(
            "SELECT id FROM records WHERE kind=? AND event=?", (KINDS.index("events"), event)
        ).fetchall()
        keys = db.execute(
            "SELECT DISTINCT typeof(key),length(payload_hash) FROM records"
        ).fetchall()
    assert linked == stored and len(linked) == 1
    # Keys are text, kept once, and hashes are their 32 bytes.
    assert keys == [("text", 32)]


def test_a_repeated_record_is_stored_once_within_and_across_writes(index):
    _put(
        index, ("events", _record(1), "k"), ("events", _record(1), "k"), ("kpis", _record(2), None)
    )
    _put(index, ("events", _record(1), "k"), ("kpis", _record(2), None))
    assert _counts(index) == {
        "records": 2,
        "entities": 4,
        "tokens": 4,
        "links": 2,
        "names": 7,
        "generations": {"events": 1, "kpis": 1},
    }


def test_kinds_are_coded_in_the_order_of_their_names():
    """Rows ordered by kind code are ordered as by name, as the cursors of a search need,
    and the partial indexes name the code of events as a literal."""
    assert list(KINDS) == sorted(KINDS) and len(set(KINDS)) == len(KINDS)
    assert history_index._EVENTS == KINDS.index("events") == 4
    assert "WHERE kind=4;" in history_index._SCHEMA


def test_a_record_of_an_unknown_kind_is_refused(index):
    with pytest.raises(StorageError, match="history_index_kind_unknown"):
        _put(index, ("people", _record(1), None))
    assert _counts(index)["records"] == 0


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
        records.ancestors = [(records.name("main"), 50)]
        records.put("events", _record(10), key="early")
        records.put("events", _record(60), key="late")
    with sqlite3.connect(index.path.as_uri() + "?mode=ro", uri=True) as db:
        stored = db.execute(
            "SELECT key FROM records WHERE branch=?", (_name(db, "child"),)
        ).fetchall()
    # The parent's row at minute 60 is after the fork, so the child stores its own.
    assert stored == [("late",)]
    with pytest.raises(StorageError, match="immutable_history_record_conflict"):
        with index.connect(write=True) as db, _RecordBatch(db, "child") as records:
            records.ancestors = [(records.name("main"), 50)]
            records.put("events", {**_record(10), "status": "rejected"}, key="early")


@pytest.mark.parametrize("version", [1, 2, 3, 4])
def test_an_index_of_an_older_version_is_refused_but_for_its_saved_points(index, version):
    """#130, #138: versions 1 to 4 are not converted. Records are neither read nor written until
    the index is rebuilt; heads and checkpoints, the same in every version, are read."""
    with index.connect(write=True) as db:
        db.execute("INSERT INTO heads VALUES('main','head',60,'{}')")
        db.execute("INSERT INTO checkpoints VALUES('main',30,'saved','state')")
        db.execute(f"PRAGMA user_version={version}")
    before = index.path.read_bytes()
    for write in (False, True):
        with pytest.raises(StorageError) as refused:
            with index.connect(write=write):
                pass
        assert refused.value.code == "history_index_rebuild_required"
    with pytest.raises(StorageError, match="history_index_rebuild_required"):
        index.require_current()
    assert index.nearest("main", 45, "head", 60) == (30, "saved")
    assert index.version() == version
    assert index.path.read_bytes() == before


def test_an_index_of_a_newer_version_is_refused_even_for_its_saved_points(index):
    with sqlite3.connect(index.path) as db:
        db.execute("PRAGMA user_version=6")
    for options in ({}, {"write": True}, {"saved_points": True}):
        with pytest.raises(StorageError, match="history_index_version_unsupported"):
            with index.connect(**options):
                pass


def test_a_new_index_is_made_at_the_current_version_with_its_kinds(index):
    with sqlite3.connect(index.path.as_uri() + "?mode=ro", uri=True) as db:
        assert db.execute("PRAGMA user_version").fetchone()[0] == 5
        assert db.execute("SELECT name FROM kinds ORDER BY code").fetchall() == [
            (name,) for name in KINDS
        ]


def test_a_conversations_turns_are_read_newest_first_from_its_branch(index):
    turns = [
        {"turn_id": f"turn-{number}", "conversation_id": f"talk-{number % 2}", "minute": number}
        for number in range(5)
    ]
    _put(index, *(("conversations", turn, turn["turn_id"]) for turn in turns))
    _put(index, ("events", {**_record(1), "conversation_id": "talk-1"}, None))
    with index.connect() as db:

        def keys(conversation: str | None, limit: int, branch: str = "main") -> list[str]:
            rows = _conversation_turns(db, branch, conversation, limit)
            return [json.loads(history_payloads.decode(row[0]))["turn_id"] for row in rows]

        assert keys("talk-1", 5) == ["turn-3", "turn-1"]
        assert keys("talk-0", 2) == ["turn-4", "turn-2"]
        assert keys(None, 3) == ["turn-4", "turn-3", "turn-2"]
        assert keys("talk-9", 5) == keys(None, 5, "other") == []


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


def test_a_kept_writer_remembers_only_the_names_of_committed_writes(index):
    """Names added by a write that is rolled back are gone; the writer must not reuse
    their ids, but add them again."""
    kept = HistoryIndex(index.store, "p", "r", keep_writer=True)
    try:
        kept.add_turn("main", {"turn_id": "turn-1", "conversation_id": "talk-1", "minute": 1})
        assert kept._names.keys() == {"main", "turn-1", "talk-1"}
        with pytest.raises(StorageError, match="immutable_history_record_conflict"):
            kept.add_turn("main", {"turn_id": "turn-1", "conversation_id": "talk-2", "minute": 1})
        assert "talk-2" not in kept._names
        kept.add_turn("main", {"turn_id": "turn-2", "conversation_id": "talk-2", "minute": 2})
        assert "talk-2" in kept._names
        # The rows of every write name names that exist.
        with sqlite3.connect(index.path.as_uri() + "?mode=ro", uri=True) as db:
            missing = db.execute(
                "SELECT COUNT(*) FROM entities e LEFT JOIN names n ON n.id=e.entity "
                "WHERE n.id IS NULL"
            ).fetchone()[0]
            names = [row[0] for row in db.execute("SELECT name FROM names ORDER BY id")]
        assert missing == 0
        assert names == ["main", "talk-1", "turn-1", "talk-2", "turn-2"]
    finally:
        kept.close()


def _payload_forms(index: HistoryIndex) -> list[str]:
    with sqlite3.connect(index.path.as_uri() + "?mode=ro", uri=True) as db:
        return [row[0] for row in db.execute("SELECT typeof(payload) FROM records ORDER BY key")]


def test_a_stored_payload_is_compared_as_text_whatever_bytes_hold_it(index):
    """#80: a kept key takes the same payload again and refuses another one, whether its
    row holds the bytes this build compresses or those another zlib build did. Its own
    branch's rows and an ancestor's are compared alike."""
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
        db.execute(
            "INSERT INTO records VALUES(NULL,?,?,?,?,?,?,?,?,?,?,?,?)",
            (*tuple(stored)[1:4], "other_bytes", *tuple(stored)[5:11], relevel, stored[12]),
        )
        db.execute("INSERT INTO lineage VALUES('child','main',50)")
    assert _payload_forms(index) == ["blob", "blob"]

    def stored_rows() -> list[Any]:
        # Nothing is written again: the same rows, in the same bytes.
        with sqlite3.connect(index.path.as_uri() + "?mode=ro", uri=True) as db:
            return db.execute("SELECT * FROM records ORDER BY branch,key").fetchall()

    before = stored_rows()
    keys = ("compressed", "other_bytes")
    _put(index, *(("events", _record(1), key) for key in keys))
    assert stored_rows() == before
    for key in keys:
        with pytest.raises(StorageError, match="immutable_history_record_conflict"):
            _put(index, ("events", {**_record(1), "status": "rejected"}, key))
        with index.connect(write=True) as db, _RecordBatch(db, "child") as records:
            records.ancestors = [(records.name("main"), 50)]
            records.put("events", _record(1), key=key)
        with pytest.raises(StorageError, match="immutable_history_record_conflict"):
            with index.connect(write=True) as db, _RecordBatch(db, "child") as records:
                records.ancestors = [(records.name("main"), 50)]
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


def test_a_version_two_saved_index_is_refused_until_it_is_rebuilt_with_its_saved_points(
    tmp_path,
):
    """#130: the index of a saved Run, version 2, answers its saved points but no history.
    Rebuilding any branch replaces the file with a version 5 one, in which every branch is
    indexed again from its head Checkpoint and keeps the saved points of the old file."""
    shutil.copytree(SAVED_RUN_ENGINE_V1, tmp_path / "run")
    store = ProjectStore(tmp_path / "run")
    index = HistoryIndex(store, "p", "r")
    assert index.version() == 2
    references = {branch: store.branch_reference("p", "r", branch) for branch in ("main", "child")}
    saved_points = {
        branch: index.checkpoints(branch, reference.fork_minute, 60, 100, None)["items"]
        for branch, reference in references.items()
    }
    assert len(saved_points["main"]) > 1 and saved_points["child"]
    with pytest.raises(StorageError, match="history_index_rebuild_required"):
        _saved_run_answers(tmp_path / "run")
    with pytest.raises(StorageError, match="history_index_rebuild_required"):
        index.conversation_tail("main", None)

    rebuilt = index.rebuild("child")
    assert rebuilt["status"] == "indexed_not_verified" and rebuilt["branch_id"] == "child"
    assert rebuilt["rebuilt_branch_ids"] == ["child", "main"]
    assert index.version() == 5
    assert not index.path.with_name("history.sqlite.rebuild").exists()
    answers = _saved_run_answers(tmp_path / "run")
    assert sum(len(page["items"]) for page in answers["main/events"]) == 17
    assert answers["main/agent"][0]["items"] and answers["main/causal"][0]["items"]
    for branch in ("main", "child"):
        assert answers[f"{branch}/checkpoints"]["items"] == saved_points[branch]
    # The rebuilt index is current and takes new rows.
    with pytest.raises(StorageError, match="history_index_current"):
        index.rebuild("main")
    turn = {"turn_id": "turn-1", "conversation_id": "talk-1", "minute": 61}
    index.add_turn("main", turn)
    assert index.conversation_tail("main", "talk-1", 5) == [turn]


def test_a_failed_rebuild_of_an_older_index_leaves_the_old_file(tmp_path, monkeypatch):
    shutil.copytree(SAVED_RUN_ENGINE_V1, tmp_path / "run")
    index = HistoryIndex(ProjectStore(tmp_path / "run"), "p", "r")
    before = index.path.read_bytes()

    def fail(self, branch, saved):
        if branch == "main":
            raise StorageError("history_index_unavailable: injected")
        return original(self, branch, saved)

    original = HistoryIndex._rebuild_branch
    monkeypatch.setattr(HistoryIndex, "_rebuild_branch", fail)
    with pytest.raises(StorageError, match="injected"):
        index.rebuild("child")
    assert index.path.read_bytes() == before
    assert not index.path.with_name("history.sqlite.rebuild").exists()


def test_an_empty_index_file_is_taken_by_a_computation(index):
    """A writer stopped before it made the tables leaves an empty file of version 0, which
    a computation takes and writes the tables into."""
    index.path.unlink()
    index.path.write_bytes(b"")
    assert index.version() == 0
    index.require_current()
    _put(index, ("events", _record(1), None))
    assert _counts(index)["records"] == 1


def _causal(index: HistoryIndex, order: list[int]) -> list[dict[str, Any]]:
    """Every page of the causal answer about event-0, which caused 600 events at one minute,
    each of which caused one more: stored in order. Only the second generation shows which
    of the first the cut-off graph kept."""
    rows = [("events", {**_record(0), "event_id": "event-0"}, "root")]
    for number in order:
        child = f"child-{number:03d}"
        rows += [
            ("events", {**_record(1), "event_id": child, "causation_id": "event-0"}, child),
            ("events", {**_record(2), "event_id": f"grand{child}", "causation_id": child}, None),
        ]
    _put(index, *rows)
    with index.connect(write=True) as db:
        db.execute("INSERT INTO heads VALUES('main','head',60,'{}')")
    pages, cursor = [], None
    while True:
        query = HistoryQuery(kind="causal", event_id="event-0", end=60, limit=100, cursor=cursor)
        pages.append(index.search("main", query, "head", 60))
        cursor = pages[-1]["next_cursor"]
        if cursor is None:
            return pages


def test_a_cut_off_causal_graph_keeps_the_same_events_whatever_order_they_were_written(
    tmp_path,
):
    """Past 500 events the graph is cut off; which events it keeps depends on the time and
    key of the links, not on the ids rows and names took (#130)."""
    answers = []
    for name, order in (("forward", list(range(600))), ("backward", list(range(599, -1, -1)))):
        (tmp_path / name / "p" / "runs" / "r").mkdir(parents=True)
        answers.append(_causal(HistoryIndex(ProjectStore(tmp_path / name), "p", "r"), order))
    assert answers[0][0]["causal_graph_truncated"] is True
    grandchildren = [
        item["event_id"]
        for page in answers[0]
        for item in page["items"]
        if item["event_id"].startswith("grand")
    ]
    # The graph keeps event-0 and 499 children, the first by key.
    assert sorted(grandchildren) == [f"grandchild-{number:03d}" for number in range(499)]
    assert json.dumps(answers[0]) == json.dumps(answers[1])


def test_a_rebuild_waits_for_a_reader_of_the_old_file_on_windows(tmp_path, monkeypatch):
    """Windows refuses to replace a file a connection holds open; the rebuild tries again
    while a short read ends, and else is refused, keeping the old file."""
    shutil.copytree(SAVED_RUN_ENGINE_V1, tmp_path / "run")
    index = HistoryIndex(ProjectStore(tmp_path / "run"), "p", "r")
    before = index.path.read_bytes()
    original, refusals = Path.replace, []
    monkeypatch.setattr(history_index, "_REPLACE_PAUSE_SECONDS", 0)

    def busy(self, target):
        if len(refusals) < limit:
            refusals.append(target)
            raise PermissionError(13, "in use")
        return original(self, target)

    monkeypatch.setattr(Path, "replace", busy)
    limit = history_index._REPLACE_ATTEMPTS
    with pytest.raises(StorageError, match="history_index_unavailable"):
        index.rebuild("main")
    assert index.path.read_bytes() == before
    assert not index.path.with_name("history.sqlite.rebuild").exists()
    refusals.clear()
    limit = 3
    index.rebuild("main")
    assert len(refusals) == 3 and index.version() == 5
