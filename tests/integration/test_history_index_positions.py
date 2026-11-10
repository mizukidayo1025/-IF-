"""Version 5 of the history index (#138) keeps no payload for the rows made from one
EventLog record alone: an event's row, its social row (key `event:`) and its action, care
or operations row (key `applied:`). Such a row holds the record's position in the EventLog,
and a read makes the row again from the indexed head Checkpoint, checked against the
row's payload_hash. Dialogue rows, KPI samples, model_state histories and the decisions
recorded while computing keep their payloads.

That the rows made again are the bytes version 4 stored is pinned in
tests/integration/test_decisions_row.py (its digest of every row is the one before #138).
"""

from __future__ import annotations

import json
import sqlite3
from typing import Any

import pytest

from sheltersim.contracts.workspace import HistoryQuery
from sheltersim.storage import history_index
from sheltersim.storage.history_index import KINDS, HistoryIndex
from sheltersim.storage.projects import StorageError, digest


def _rows(index: HistoryIndex) -> list[sqlite3.Row]:
    with sqlite3.connect(index.path.as_uri() + "?mode=ro", uri=True) as db:
        db.row_factory = sqlite3.Row
        return db.execute(
            "SELECT n.name AS branch_name,r.kind,r.minute,r.key,r.position,"
            "r.payload IS NULL AS bare "
            "FROM records r JOIN names n ON n.id=r.branch ORDER BY r.id"
        ).fetchall()


def _events(store: Any, branch: str) -> list[dict[str, Any]]:
    reference = store.branch_reference("p", "r", branch)
    document = store.checkpoint_document("p", "r", reference.checkpoint_hash)
    return list(document["world_state"]["event_log"])


def _search(index: HistoryIndex, store: Any, branch: str, **fields: Any) -> list[dict[str, Any]]:
    reference = store.branch_reference("p", "r", branch)
    items, cursor = [], None
    while True:
        page = index.search(
            branch,
            HistoryQuery(limit=100, cursor=cursor, **fields),
            reference.checkpoint_hash,
            reference.computed_until,
        )
        items.extend(page["items"])
        cursor = page["next_cursor"]
        if not cursor:
            return items


def test_rows_made_from_an_event_keep_its_position_and_no_payload(
    generated_scenario, tmp_path, saved_timeline
) -> None:
    store, timeline = saved_timeline(tmp_path, generated_scenario)
    timeline.advance("main", 60, checkpoint_interval=15)
    index = HistoryIndex(store, "p", "r")
    rows = _rows(index)
    events = _events(store, "main")
    by_kind: dict[tuple[str, bool], int] = {}
    for row in rows:
        kind = KINDS[row["kind"]]
        positional = kind == "events" or row["key"].startswith(("applied:", "event:"))
        if kind == "dialogue":
            positional = False
        assert bool(row["bare"]) is positional, (kind, row["key"])
        assert (row["position"] is not None) is positional
        by_kind[kind, positional] = by_kind.get((kind, positional), 0) + 1
        if kind == "events":
            # The key of an event's row is the digest of the record at its position.
            assert digest(events[row["position"]]) == row["key"]
        elif positional:
            assert row["key"].split(":", 1)[1] == events[row["position"]]["event"]["event_id"]
    assert by_kind["events", True] == len(events)
    assert by_kind["social", True] and by_kind["dialogue", False] and by_kind["kpis", False]
    assert any(kind in {"actions", "care", "operations"} for kind, bare in by_kind if bare)
    # Every event comes back as the row its record makes, in the order of the index.
    found = _search(index, store, "main", kind="events", end=59)
    assert len(found) == len(events)
    for item in found:
        assert item["record_kind"] == "events"
        assert item["time_semantics"] == "recorded_event_minute"


def test_a_row_made_otherwise_than_it_was_indexed_is_refused(
    generated_scenario, tmp_path, saved_timeline, monkeypatch
) -> None:
    """A change to what a row made from an event holds changes the index: the read refuses
    the row instead of answering with what the old Run never indexed."""
    store, timeline = saved_timeline(tmp_path, generated_scenario)
    timeline.advance("main", 30, checkpoint_interval=15)
    index = HistoryIndex(store, "p", "r")
    assert _search(index, store, "main", kind="events", end=29)
    made = history_index._event_row
    monkeypatch.setattr(history_index, "_event_row", lambda record: {**made(record), "new": 1})
    with pytest.raises(StorageError, match="history_index_record_corrupt"):
        _search(index, store, "main", kind="events", end=29)


def test_a_fork_reads_its_parents_rows_from_the_parents_head(
    generated_scenario, tmp_path, saved_timeline
) -> None:
    """A child branch reads its parent's rows before the fork minute; their records are
    in the parent's head Checkpoint, which has gone on after the fork."""
    store, timeline = saved_timeline(tmp_path, generated_scenario)
    timeline.advance("main", 30, checkpoint_interval=15)
    timeline.fork("main", 30, "child")
    timeline.advance("main", 60, checkpoint_interval=15)
    timeline.advance("child", 45, checkpoint_interval=15)
    index = HistoryIndex(store, "p", "r")
    child = [row for row in _rows(index) if row["branch_name"] == "child"]
    # The child holds no row of its own before the fork minute.
    assert child and min(row["minute"] for row in child) >= 30
    for kind in ("events", "social", "actions"):
        before = _search(index, store, "main", kind=kind, end=29)
        assert before and _search(index, store, "child", kind=kind, end=29) == before
    after = _search(index, store, "child", kind="events", start=30, end=44)
    assert after and all(item["minute"] >= 30 for item in after)
    assert json.dumps(after) != json.dumps(_search(index, store, "main", kind="events", end=44))
