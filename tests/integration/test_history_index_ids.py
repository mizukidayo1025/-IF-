"""An index answers the same whatever order its ids run in.

Since version 4 (#130) each record has an integer id, which its entity, token and event link
rows name, and every repeated text is a row of names, held by its id. Ids follow the order rows
were written in, while answers are ordered by minute, kind and key. So the same Run is read
again from a copy whose record ids and name ids run backwards: every query, page by page, must
answer the same, and anything leaning on id order shows.
"""

from __future__ import annotations

import copy
import json
import shutil
import sqlite3
from pathlib import Path
from typing import Any

import pytest

from sheltersim.contracts.workspace import HistoryQuery
from sheltersim.experiments.scenario import ScenarioSpec
from sheltersim.storage import history_index
from sheltersim.storage.history_index import HistoryIndex
from sheltersim.storage.projects import ProjectStore, digest

END = 120
FORK_MINUTE = 30
BRANCHES = ("main", "capacity")
KINDS = (
    "events",
    "kpis",
    "social",
    "staff",
    "actions",
    "transitions",
    "dialogue",
    "care",
    "operations",
)
_ID_TABLES = (
    # (table, columns naming a record, columns naming a name)
    ("records", ("id",), ("branch", "event", "cause", "source", "type", "status")),
    ("entities", ("record",), ("entity",)),
    ("tokens", ("record",), ("token",)),
    ("links", ("record",), ("event",)),
    ("names", (), ("id",)),
)


def _index_of(root: Path) -> HistoryIndex:
    return HistoryIndex(ProjectStore(root), "p", "r")


def _build(root: Path, scenario: ScenarioSpec, saved_timeline, program_of) -> None:
    _store, timeline = saved_timeline(root, scenario)
    timeline.advance("main", END, checkpoint_interval=15)
    program = program_of(2, start_minute=FORK_MINUTE)
    timeline.fork(
        "main",
        FORK_MINUTE,
        "capacity",
        program=program,
        approved_program_hash=digest(program.model_dump(mode="json")),
    )
    timeline.advance("capacity", END, checkpoint_interval=15)
    index = _index_of(root)
    for number in range(9):
        turn = {
            "turn_id": f"turn-{8 - number}",
            "conversation_id": f"talk-{number % 2}",
            "minute": number,
        }
        index.add_turn("main", turn)


def _reverse_ids(path: Path) -> None:
    """Number the records and the names of an index backwards, keeping every link."""
    with sqlite3.connect(path) as db:
        last_record = db.execute("SELECT MAX(id) FROM records").fetchone()[0]
        last_name = db.execute("SELECT MAX(id) FROM names").fetchone()[0]
        for table, records, names in _ID_TABLES:
            for columns, last in ((records, last_record), (names, last_name)):
                for column in columns:
                    # Through negative values, so no two rows hold one id meanwhile.
                    db.execute(f"UPDATE {table} SET {column}=-{column}")
                    db.execute(f"UPDATE {table} SET {column}={last}+1+{column}")


@pytest.fixture(scope="module")
def numberings(tmp_path_factory, generated_previews, add_toilets_program, saved_timeline):
    """The same Run with its ids as written and backwards: {name: projects root}."""
    preview = generated_previews(duration_days=END / 1440)
    scenario = ScenarioSpec.model_validate(copy.deepcopy(preview["scenario"]))
    base = tmp_path_factory.mktemp("numberings")
    with pytest.MonkeyPatch.context() as monkeypatch:
        monkeypatch.setenv("SHELTERSIM_LLM_MODE", "offline")
        _build(base / "written", scenario, saved_timeline, add_toilets_program)
    shutil.copytree(base / "written", base / "backwards")
    _reverse_ids(_index_of(base / "backwards").path)
    return {name: base / name for name in ("written", "backwards")}


def _pages(index: HistoryIndex, branch: str, anchor: str, **fields: Any) -> list[dict[str, Any]]:
    """Every page of a query as the API returns it, cursors and all, with limit 7."""
    pages = []
    cursor = None
    while True:
        query = HistoryQuery(end=END, limit=7, cursor=cursor, **fields)
        page = index.search(branch, query, anchor)
        pages.append(page)
        cursor = page.get("next_cursor")
        if not cursor:
            return pages


def _values(root: Path, branch: str) -> dict[str, Any]:
    """Filter values that exist in this Run: every text a filter can match, in sorted order."""
    index = _index_of(root)
    with index.connect() as db:
        tokens = [
            row[0]
            for row in db.execute(
                "SELECT DISTINCT n.name FROM tokens t JOIN names n ON n.id=t.token ORDER BY 1"
            )
        ]
        entities = [
            row[0]
            for row in db.execute(
                "SELECT DISTINCT n.name FROM entities e JOIN names n ON n.id=e.entity ORDER BY 1"
            )
        ]
        events = [
            row[0]
            for row in db.execute(
                "SELECT n.name FROM records r JOIN names n ON n.id=r.event WHERE r.kind=? AND "
                "r.branch=(SELECT id FROM names WHERE name=?) ORDER BY r.minute,r.key LIMIT 400",
                (history_index.KINDS.index("events"), branch),
            )
        ]
        # The values of the type, status and source columns of the branch's own records.
        columns = {
            column: [
                row[0]
                for row in db.execute(
                    f"SELECT n.name FROM records r JOIN names n ON n.id=r.{column} WHERE "
                    "r.branch=(SELECT id FROM names WHERE name=?) GROUP BY n.name "
                    "ORDER BY COUNT(*) DESC,n.name LIMIT 3",
                    (branch,),
                )
            ]
            for column in ("type", "status", "source")
        }
        rows = db.execute(
            "SELECT n.name,r.branch,r.kind,r.key,r.position,r.payload,r.payload_hash "
            "FROM entities e JOIN names n ON n.id=e.entity JOIN records r ON r.id=e.record"
        ).fetchall()
        texts = index.payload_texts(db, rows)
        staff = sorted(
            {
                row["name"]
                for row, text in zip(rows, texts, strict=True)
                if json.loads(text).get("staff_id") == row["name"]
            }
        )
    return {
        "words": tokens[:12],
        "agents": [item for item in entities if item.startswith("A0")][:6],
        "staff": staff[:3],
        "zones": [item for item in entities if "sleeping" in item or "reception" in item][:3],
        "households": [item for item in entities if item.startswith("H0")][:3],
        "events": events[:: max(len(events) // 6, 1)],
        "types": columns["type"],
        "statuses": columns["status"],
        "sources": columns["source"],
    }


def _answers(root: Path, values: dict[str, dict[str, Any]]) -> dict[str, Any]:
    """Every query on entities and tokens that the index has, on every branch of the Run."""
    index = _index_of(root)
    store = index.store
    answers: dict[str, Any] = {}
    for branch in BRANCHES:
        head = store.branch_reference("p", "r", branch).checkpoint_hash
        known = values[branch]

        def ask(label: str, **fields: Any) -> None:
            answers[f"{branch}/{label}"] = _pages(index, branch, head, **fields)

        for kind in KINDS:
            ask(f"{kind}", kind=kind)
        for agent in known["agents"]:
            for kind in ("events", "actions", "social", "dialogue", "care"):
                ask(f"{kind} agent {agent}", kind=kind, agent_id=agent)
        for staff in known["staff"]:
            ask(f"operations staff {staff}", kind="operations", staff_id=staff)
        for zone in known["zones"]:
            ask(f"actions zone {zone}", kind="actions", zone_id=zone)
        for household in known["households"]:
            ask(f"people {household}", kind="events", household_id=household)
        for word in known["words"]:
            for kind in ("events", "actions"):
                ask(f"{kind} text {word}", kind=kind, text=word)
        for kind in ("events", "actions", "social", "kpis"):
            for event_type in known["types"]:
                ask(f"{kind} type {event_type}", kind=kind, event_type=event_type)
            for status in known["statuses"]:
                ask(f"{kind} status {status}", kind=kind, status=status)
            for source in known["sources"]:
                ask(f"{kind} source {source}", kind=kind, source_id=source)
        ask("kpis every 7", kind="kpis", start=3, step=7)
        for event in known["events"]:
            ask(f"events event {event}", kind="events", event_id=event)
            ask(f"causal {event}", kind="causal", event_id=event)
            ask(f"related {event}", kind="causal", event_id=event, include_related=True)
        answers[f"{branch}/checkpoints"] = index.checkpoints(branch, FORK_MINUTE, END, 100, None)
        answers[f"{branch}/nearest"] = [
            index.nearest(branch, minute, head, END) for minute in (FORK_MINUTE, 44, END)
        ]
    for conversation in ("talk-0", "talk-1", None):
        for limit in (1, 3, 100):
            answers[f"tail {conversation} {limit}"] = index.conversation_tail(
                "main", conversation, limit
            )
    answers["conversations"] = _pages(
        index,
        "main",
        store.branch_reference("p", "r", "main").checkpoint_hash,
        kind="conversations",
        conversation_id="talk-0",
    )
    return answers


def _text(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, sort_keys=False)


def test_the_backwards_copy_numbers_records_and_names_against_the_written_one(numberings):
    def first_and_last(name: str) -> list[Any]:
        with sqlite3.connect(_index_of(numberings[name]).path) as db:
            return [
                db.execute(f"SELECT {column} FROM {table} ORDER BY id {order} LIMIT 1").fetchone()
                for table, column in (("records", "key"), ("names", "name"))
                for order in ("ASC", "DESC")
            ]

    written, backwards = first_and_last("written"), first_and_last("backwards")
    assert written != backwards
    assert written == [backwards[1], backwards[0], backwards[3], backwards[2]]


def test_every_query_answers_the_same_in_order_whatever_order_the_ids_run_in(numberings) -> None:
    values = {branch: _values(numberings["written"], branch) for branch in BRANCHES}
    empty = [name for name, found in values["main"].items() if not found]
    assert empty == []
    assert values == {branch: _values(numberings["backwards"], branch) for branch in BRANCHES}
    written = _answers(numberings["written"], values)
    backwards = _answers(numberings["backwards"], values)
    assert written.keys() == backwards.keys()
    assert sum(len(page["items"]) for page in written["main/events"]) > 7
    for label in (" agent ", " text ", " type ", " status ", " source "):
        assert any(
            page["items"] for key, pages in written.items() if label in key for page in pages
        )
    assert any(
        page["items"]
        for key, pages in written.items()
        if key.startswith("main/causal")
        for page in pages
    )
    assert [turn["turn_id"] for turn in written["tail talk-0 3"]] == ["turn-4", "turn-6", "turn-8"]
    different = [key for key in written if _text(written[key]) != _text(backwards[key])]
    assert different == []


def test_a_backwards_numbered_index_takes_new_rows_that_answer_alike(numberings, tmp_path) -> None:
    """A writer numbers new records after the last id and finds names by their text."""
    values = {branch: _values(numberings["written"], branch) for branch in BRANCHES}
    answers = {}
    for name in ("written", "backwards"):
        root = tmp_path / name
        shutil.copytree(numberings[name], root)
        index = _index_of(root)
        for branch in BRANCHES:
            turn = {"turn_id": "turn-9", "conversation_id": "talk-0", "minute": 9}
            index.add_turn(branch, turn)
        answers[name] = _answers(root, values)
    assert answers["written"]["tail talk-0 1"][0]["turn_id"] == "turn-9"
    different = [
        key
        for key in answers["written"]
        if _text(answers["written"][key]) != _text(answers["backwards"][key])
    ]
    assert different == []
