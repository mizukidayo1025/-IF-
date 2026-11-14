"""Derived SQLite evidence/checkpoint index. Never owns simulation state or RNG.

Writers ingest immutable checkpoint deltas. Reads require the published branch head
and use B-tree ranges/keyset cursors; old Runs opt in through explicit index rebuild.
The authoritative JSON/checkpoint hashes remain unchanged and independently readable.
"""

from __future__ import annotations

import base64
import functools
import hashlib
import json
import re
import sqlite3
import time
from bisect import bisect_left
from contextlib import contextmanager
from pathlib import Path
from typing import Any, Iterator

from sheltersim import native
from sheltersim.core.saved_items import SavedItems
from sheltersim.domain.social_dialogue import virtual_dialogue_example
from sheltersim.events.queue import saved_realized_minute
from sheltersim.storage import history_payloads
from sheltersim.storage.projects import (
    ProjectStore,
    StorageError,
    _private_field,
    canonical,
    digest,
    safe_row,
)

# Version 4 (#130) keeps each record's key once, in records, with an integer id that the
# entity, token and event link rows point to. Every other repeated text (branch, event and
# cause ids, source, type, status, entities and tokens) is a row of names, held by its id, and
# a record kind is a fixed code (KINDS). The SHA-256 of a payload is its 32 bytes. Only
# record_key holds the key: the other indexes end at the minute, and rows of one minute are
# put in key order by a sort, which a search reads no slower for the smaller indexes.
# Version 5 (#138) keeps no payload for a row derived from one EventLog record alone (an
# event's row, and the social and action rows made from it): the row holds the record's
# position in the EventLog, and a read derives the payload again from the indexed head
# Checkpoint and checks it against payload_hash. Every other row keeps its payload.
# Version 6 (#138) names in entities only what an entity filter can match (not the values of
# _NOT_ENTITY_KEYS), holds a key of 64 lowercase hex digits as its 32 bytes (read back as
# the same text by _KEY_TEXT, in the same order), and keeps of a dialogue row made from an
# event only its body-state effects, which the head Checkpoint may no longer hold.
_SCHEMA = """
CREATE TABLE IF NOT EXISTS generations(branch TEXT NOT NULL, kind TEXT NOT NULL, value INTEGER NOT NULL, PRIMARY KEY(branch,kind));
CREATE TABLE IF NOT EXISTS heads(branch TEXT PRIMARY KEY, hash TEXT NOT NULL, minute INTEGER NOT NULL, counts TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS checkpoints(branch TEXT NOT NULL, minute INTEGER NOT NULL, hash TEXT NOT NULL, state_hash TEXT NOT NULL, PRIMARY KEY(branch,minute));
CREATE TABLE IF NOT EXISTS lineage(branch TEXT PRIMARY KEY, parent TEXT NOT NULL, fork_minute INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS kinds(code INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE);
CREATE TABLE IF NOT EXISTS names(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE);
CREATE TABLE IF NOT EXISTS records(id INTEGER PRIMARY KEY, branch INTEGER NOT NULL, kind INTEGER NOT NULL, minute INTEGER NOT NULL, key NOT NULL, event INTEGER, cause INTEGER, source INTEGER, type INTEGER, status INTEGER, position INTEGER, payload BLOB, payload_hash BLOB NOT NULL, CHECK(position IS NOT NULL OR payload IS NOT NULL));
CREATE UNIQUE INDEX IF NOT EXISTS record_key ON records(branch,kind,key);
CREATE INDEX IF NOT EXISTS record_time ON records(branch,kind,minute);
CREATE INDEX IF NOT EXISTS record_event ON records(event) WHERE kind=4;
CREATE INDEX IF NOT EXISTS record_cause ON records(cause) WHERE kind=4;
CREATE INDEX IF NOT EXISTS record_source ON records(branch,kind,source,minute);
CREATE INDEX IF NOT EXISTS record_status ON records(branch,kind,status,minute);
CREATE INDEX IF NOT EXISTS record_type ON records(branch,kind,type,minute);
CREATE TABLE IF NOT EXISTS entities(entity INTEGER NOT NULL, record INTEGER NOT NULL, PRIMARY KEY(entity,record)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS tokens(token INTEGER NOT NULL, record INTEGER NOT NULL, PRIMARY KEY(token,record)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS links(event INTEGER NOT NULL, record INTEGER NOT NULL, PRIMARY KEY(event,record)) WITHOUT ROWID;
"""  # noqa: E501
_SCHEMA_VERSION = HISTORY_INDEX_VERSION = 6
# The record kinds, coded in the order of their names, so that rows ordered by code are
# ordered as their names were. A new kind takes a new index version.
KINDS = (
    "actions",
    "care",
    "conversations",
    "dialogue",
    "events",
    "kpis",
    "operations",
    "social",
    "staff",
    "transitions",
)
_KIND_CODES = {name: code for code, name in enumerate(KINDS)}
_EVENTS = _KIND_CODES["events"]  # record_event and record_cause name it as a literal.
_CONVERSATIONS = _KIND_CODES["conversations"]
_WRITE_CACHE_KIB = 16_384
# The indexing process keeps its connection, and its cache, from one save to the next.
_KEPT_WRITER_CACHE_KIB = 262_144
# The names a writer remembers between its writes; the rest it reads again.
_KEPT_NAMES = 200_000
# A key of 64 lowercase hex digits (an event's or a row's SHA-256) is stored as its 32
# bytes; _KEY_TEXT reads every stored key back as its text. Rows are ordered by that text,
# as versions up to 5 ordered the text they stored: within a kind, hash keys and texts such
# as "applied:..." are ordered as before, which SQLite's order of texts before blobs is not.
_HEX_KEY = re.compile(r"[0-9a-f]{64}")
_KEY_TEXT = "(CASE WHEN typeof({0}key)='blob' THEN lower(hex({0}key)) ELSE {0}key END)"
_R_KEY = _KEY_TEXT.format("r.")
_PLAIN_KEY = _KEY_TEXT.format("")


def _stored_key(key: str) -> str | bytes:
    """The form a key is stored in."""
    return bytes.fromhex(key) if _HEX_KEY.fullmatch(key) else key


_REBUILD_REQUIRED = (
    "history_index_rebuild_required: this Run's history index was written in an older "
    "form; rebuild it (POST R/index) or compute the Run again"
)


def _storage_code(error: sqlite3.Error) -> str:
    if getattr(error, "sqlite_errorcode", None) == sqlite3.SQLITE_FULL:
        return "storage_capacity_exceeded"
    return "history_index_unavailable"


@functools.cache
def _operation_receipts() -> Any:
    # Building a TypeAdapter compiles its validator; build each one once (#223).
    from pydantic import TypeAdapter

    from sheltersim.contracts.operations import OperationHistoryRecord

    return TypeAdapter(OperationHistoryRecord)


@functools.cache
def _queue_receipts() -> Any:
    from pydantic import TypeAdapter

    from sheltersim.contracts.operations import FacilityQueueTransitionReceipt

    return TypeAdapter(FacilityQueueTransitionReceipt)


def cursor_encode(value: dict[str, Any]) -> str:
    return base64.urlsafe_b64encode(canonical(value).encode()).decode()


def cursor_decode(value: str | None, scope: str, anchor: str) -> tuple[int, str] | None:
    if value is None:
        return None
    try:
        data = json.loads(base64.b64decode(value, altchars=b"-_", validate=True))
        if (
            set(data) != {"scope", "anchor", "minute", "key"}
            or data["scope"] != scope
            or data["anchor"] != anchor
            or type(data["minute"]) is not int
            or not isinstance(data["key"], str)
            or len(data["key"]) > 128
        ):
            raise ValueError("foreign cursor")
        return data["minute"], data["key"]
    except (ValueError, TypeError, KeyError, UnicodeError) as exc:
        raise StorageError("stale_or_foreign_cursor: restart this query") from exc


# Rows repeat the same few token texts and shapes of keys; read each once.
@functools.lru_cache(maxsize=4096)
def _terms(text: str) -> list[str]:
    # Literal tokens, not SQL/FTS expressions. Japanese contiguous terms stay intact.
    return sorted(set(re.findall(r"[\w:-]{1,80}", text.casefold())))[:16]


@functools.lru_cache(maxsize=4096)
def _any_private_field(keys: str) -> bool:
    return any(map(_private_field, keys.split("\0")))


# Identity fields whose values no entity filter (agent, household, staff, zone or
# conversation) can name: ids of events, actions, sources, models, topics, tasks, requests,
# facilities and resources. Version 6 leaves them out of entities (#138); the identity
# fields of every other name are kept. native/src/checkpoint.rs holds the same list.
_NOT_ENTITY_KEYS = frozenset(
    {
        "action_id",
        "applied_event_ids",
        "causation_event_ids",
        "causation_id",
        "environmental_stressor_event_ids",
        "event_id",
        "event_ids",
        "facility_id",
        "model_id",
        "model_source_id",
        "modifier_event_ids",
        "relationship_policy_id",
        "relationship_policy_source_id",
        "request_id",
        "reservation_id",
        "resource_id",
        "route_edge_ids",
        "shipment_id",
        "source_event_ids",
        "source_id",
        "source_ids",
        "task_id",
        "topic_id",
        "warehouse_id",
    }
)

# The row fields whose words are indexed as tokens.
_TOKEN_FIELDS = ("event_type", "action", "kind", "status", "trigger", "topic_id", "task_kind")


def _identities(value: Any) -> set[str]:
    identities: set[str] = set()
    if isinstance(value, dict):
        for key, item in value.items():
            if key.endswith(("_id", "_ids")):
                if key in _NOT_ENTITY_KEYS:
                    continue
                if isinstance(item, str):
                    identities.add(item)
                elif isinstance(item, list):
                    identities.update(x for x in item if isinstance(x, str))
            elif isinstance(item, (dict, list)):
                identities.update(_identities(item))
    elif isinstance(value, list):
        for item in value:
            identities.update(_identities(item))
    return identities


def _event_references(value: Any) -> set[str]:
    result = set()
    if isinstance(value, dict):
        for key, item in value.items():
            if key in {"event_id", "causation_id"} and isinstance(item, str):
                result.add(item)
            elif key in {
                "event_ids",
                "source_event_ids",
                "causation_event_ids",
                "modifier_event_ids",
                "environmental_stressor_event_ids",
            } and isinstance(item, (list, tuple)):
                result.update(part for part in item if isinstance(part, str))
            elif isinstance(item, (dict, list, tuple)):
                result.update(_event_references(item))
    elif isinstance(value, (list, tuple)):
        for item in value:
            result.update(_event_references(item))
    return result


# The rows made from one EventLog record alone. Version 5 stores the record's position for
# them, not their payloads, and a read makes them again with these (#138); a change to what
# they make is a change of the index, which payload_hash then refuses.
_SOCIAL_EVENT_TYPES = frozenset(
    {
        "information_contact_update",
        "information_correction",
        "proximity_contact_update",
        "social_interaction",
    }
)
_SOCIAL_ACTOR_FIELDS = (
    "agent_id",
    "participant_agent_id",
    "sender_agent_id",
    "recipient_agent_id",
    "first_agent_id",
    "second_agent_id",
)
_SOCIAL_FIELDS = (
    "agent_id",
    "participant_agent_id",
    "interaction_type",
    "action",
    "topic_id",
    "message_id",
    "belief",
    "contact_basis",
    "contact_minutes",
    "distance_m",
    "zone_id",
    "reason_short",
)


def _event_row(record: dict[str, Any]) -> dict[str, Any]:
    """The row of an EventLog record."""
    return {
        **record["event"],
        "minute": record["realized_minute"],
        "status": record["status"],
        "detail": record.get("detail"),
        "time_semantics": "recorded_event_minute",
    }


def _social_row(record: dict[str, Any]) -> dict[str, Any] | None:
    """The social row of an applied information, contact or social event, or None."""
    event = record["event"]
    event_type = event.get("event_type")
    payload = event.get("payload", {})
    if (
        record["status"] != "applied"
        or event_type not in _SOCIAL_EVENT_TYPES
        or not isinstance(payload, dict)
    ):
        return None
    social = {
        "schema_version": 1,
        "event_id": event["event_id"],
        "minute": record["realized_minute"],
        "time_semantics": "recorded_event_minute",
        "event_type": event_type,
        "status": "applied",
        "actor_ids": sorted(
            {
                value
                for name in _SOCIAL_ACTOR_FIELDS
                if isinstance((value := payload.get(name)), str)
            }
        ),
        "source_id": event.get("source_id"),
        "causation_id": event.get("causation_id"),
        "evidence_semantics": (
            "structured_model_social_event"
            if event_type == "social_interaction"
            else "recorded_information_or_contact_event"
        ),
    }
    for name in _SOCIAL_FIELDS:
        value = payload.get(name)
        if type(value) in (str, int, float, bool):
            social[name] = value
    state_context = payload.get("conversation_state_context")
    if isinstance(state_context, dict):
        social["conversation_state_context"] = state_context
    return social


def _action_row(record: dict[str, Any]) -> dict[str, Any] | None:
    """The action, care or operations row of an applied event, or None."""
    from sheltersim.inspection.action_history import applied_action_record

    if record["status"] != "applied":
        return None
    return applied_action_record(record["event"], record["realized_minute"])


def _dialogue_row(
    social: dict[str, Any], observed_model_effects: list[dict[str, Any]]
) -> dict[str, Any]:
    """The dialogue row of an applied social interaction, from its social row and the
    body-state effects of its event. A deterministic, explicitly fictional example may help
    readers understand the applied typed intent; it is never a stored quote."""
    dialogue = {
        **social,
        "record_type": "structured_social_interaction",
        "synthetic": True,
        "utterance_recorded": False,
        "utterance_status": "not_recorded",
        "observed_model_effects": observed_model_effects,
    }
    dialogue.update(_virtual_utterance_example(social, observed_model_effects) or {})
    return dialogue


def _derived_row(
    kind: int, key: str, record: dict[str, Any], kept: str | None = None
) -> dict[str, Any] | None:
    """The row of kind and key that ingest made from an EventLog record and, for a dialogue
    row, the part of it kept in the index (kept, its JSON), or None."""
    if kind == _EVENTS:
        return _event_row(record)
    if key.startswith("event:") and kind == _KIND_CODES["social"]:
        return _social_row(record)
    if key.startswith("event:") and kind == _KIND_CODES["dialogue"] and kept is not None:
        social = _social_row(record)
        effects = json.loads(kept).get("observed_model_effects")
        if social is None or not isinstance(effects, list):
            return None
        return _dialogue_row(social, effects)
    if key.startswith("applied:"):
        return _action_row(record)
    return None


# Version 2 added lineage: a branch forked from another reads the parent's rows before its
# fork minute instead of copying them. Version 3 stored the payloads of new rows compressed,
# as BLOBs (storage/history_payloads.py, #80). Version 4 holds keys and repeated texts once
# (#130). Version 5 holds EventLog positions in place of the payloads derived from them,
# and version 6 smaller entities, keys and dialogue rows (#138). A file of versions 1 to 5
# is neither read nor written, nor converted: rebuilding the Run's index from its
# Checkpoints replaces it. Code that knows only version 5 refuses version 6.
_OLDER_VERSIONS = (1, 2, 3, 4, 5)


def _check_version(version: int, *, new: bool = False) -> None:
    """Refuse a file of another version; with new, an empty file (version 0) is taken."""
    if version == _SCHEMA_VERSION or (new and version == 0):
        return
    if version in _OLDER_VERSIONS:
        raise StorageError(_REBUILD_REQUIRED)
    raise StorageError("history_index_version_unsupported")


def _segments(db: sqlite3.Connection, branch: str) -> list[tuple[str, int | None]]:
    """The branches whose rows a branch reads, each with the minute its rows end before:
    the branch itself (no end), then its parent before the fork minute, and so on."""
    segments: list[tuple[str, int | None]] = [(branch, None)]
    limit: int | None = None
    current = branch
    while True:
        row = db.execute(
            "SELECT parent,fork_minute FROM lineage WHERE branch=?", (current,)
        ).fetchone()
        if row is None:
            return segments
        current = row[0]
        limit = row[1] if limit is None else min(limit, row[1])
        if any(name == current for name, _ in segments):
            raise StorageError("history_index_lineage_cycle")
        segments.append((current, limit))


def _name_id(db: sqlite3.Connection, name: Any) -> int | None:
    """The id of a stored name, or None, which no row holds, if none is stored."""
    if name is None:
        return None
    row = db.execute("SELECT id FROM names WHERE name=?", (name,)).fetchone()
    return row[0] if row is not None else None


def _scope(
    segments: list[tuple[int | None, int | None]], alias: str = "r"
) -> tuple[str, list[Any]]:
    """A WHERE term selecting the rows of segments of branch ids, and its arguments."""
    terms, args = [], []
    for name, limit in segments:
        if limit is None:
            terms.append(f"{alias}.branch=?")
            args.append(name)
        else:
            terms.append(f"({alias}.branch=? AND {alias}.minute<?)")
            args.extend((name, limit))
    return "(" + " OR ".join(terms) + ")", args


def _conversation_turns(
    db: sqlite3.Connection, branch: str, conversation_id: str | None, limit: int
) -> list[sqlite3.Row]:
    """The payloads of a branch's last conversation turns, newest first, of one conversation
    or of all. A conversation's turns are found among the branch's, which are few."""
    if conversation_id:
        return db.execute(
            "SELECT r.payload,r.payload_hash FROM records r WHERE r.branch=? AND "
            f"r.kind={_CONVERSATIONS} AND EXISTS(SELECT 1 FROM entities e WHERE "
            f"e.entity=? AND e.record=r.id) ORDER BY {_R_KEY} DESC LIMIT ?",
            (_name_id(db, branch), _name_id(db, conversation_id), limit),
        ).fetchall()
    return db.execute(
        f"SELECT payload,payload_hash FROM records WHERE branch=? AND kind={_CONVERSATIONS} "
        f"ORDER BY {_PLAIN_KEY} DESC LIMIT ?",
        (_name_id(db, branch), limit),
    ).fetchall()


def _virtual_utterance_example(
    record: dict[str, Any], observed_model_effects: list[dict[str, Any]]
) -> dict[str, Any] | None:
    """Build a labeled, deterministic illustration from applied intent and state deltas."""
    interaction = record.get("interaction_type")
    reason = record.get("reason_short")
    if not isinstance(interaction, str) or not isinstance(reason, str):
        return None
    stressed = reason in {"high_stress", "long_wait", "elevated_stress"}
    deltas: dict[str, float] = {}
    for effect in observed_model_effects:
        values = effect.get("deltas") if isinstance(effect, dict) else None
        if not isinstance(values, dict):
            continue
        for name, value in values.items():
            if isinstance(name, str) and type(value) in (int, float):
                deltas[name] = deltas.get(name, 0.0) + float(value)
    reassurance_up = deltas.get("reassurance", 0.0) > 0
    uncertainty_down = deltas.get("uncertainty", 0.0) < 0
    negative_affect_down = deltas.get("negative_affect", 0.0) < 0
    discontent_down = deltas.get("discontent", 0.0) < 0
    state_cues = [
        cue
        for cue, matched in (
            ("reassurance_increased", reassurance_up),
            ("uncertainty_decreased", uncertainty_down),
            ("negative_affect_decreased", negative_affect_down),
            ("discontent_decreased", discontent_down),
        )
        if matched
    ]

    if interaction == "reassurance_exchange":
        if reassurance_up and uncertainty_down:
            example = (
                "わかっていることを一緒に確かめたら、少し安心できた気がします。"
                "新しい案内も一緒に確認しましょう。"
            )
        elif stressed:
            example = "不安が強いときは、今わかっていることを一つずつ確認しましょう。"
        else:
            example = "気になっている点を、今わかっている範囲で一緒に整理しましょう。"
    elif interaction == "family_care":
        example = (
            "必要なことを一緒に確認できて、少し安心しました。次にできることを考えましょう。"
            if reassurance_up
            else "まず必要なことを確認して、一緒に対応しよう。"
        )
    elif interaction == "voluntary_support":
        example = (
            "声をかけてもらえて、少し気持ちが軽くなった気がします。必要なことを一緒に確認しましょう。"
            if negative_affect_down
            else "必要なことがあれば、一緒に確認しましょう。"
        )
    elif interaction == "cooperation":
        example = (
            "役割を分けて進められて、少し安心しました。できることから一緒に続けましょう。"
            if discontent_down and reassurance_up
            else "役割を分けて、できることから一緒に進めましょう。"
        )
    elif interaction == "complaint_response":
        example = (
            "困っている点を聞いてもらえて、少し落ち着けました。今わかっている案内を一緒に確かめましょう。"
            if discontent_down
            else "困っている点を確認しました。必要な案内を一緒に確かめましょう。"
        )
    elif interaction == "information_reaction":
        example = "聞いた内容の出どころを確かめてから、周りと共有したいです。"
    else:
        example = None
    if example is None:
        return None
    result = {
        "virtual_utterance_example": example,
        "virtual_utterance_policy": "typed-interaction-state-grounded-v2",
        "virtual_utterance_basis": {
            "interaction_type": interaction,
            "action": record.get("action"),
            "reason_short": reason,
            "topic_id": record.get("topic_id"),
            "state_cues": state_cues,
            "observed_model_effects": observed_model_effects,
        },
        "virtual_utterance_label": "fictional_example_not_recorded_speech",
    }
    dialogue = virtual_dialogue_example(record, observed_model_effects)
    if dialogue is not None:
        result["virtual_dialogue_example"] = dialogue
        result["virtual_utterance_policy"] = "typed-interaction-state-grounded-v3"
    return result


class _RecordBatch:
    """New index rows of one write transaction, inserted a few hundred records at a time.

    Each table receives the new records' rows in the order they were put, as one insert
    per record did before (#47). A key already stored or put earlier must carry the same
    payload, and then adds only event links its stored record may lack. The batch numbers
    its records itself, so that their entity, token and link rows can name them before
    they are inserted; the write transaction keeps any other writer out meanwhile.
    """

    _FLUSH_RECORDS = 500

    def __init__(
        self, db: sqlite3.Connection, branch: str, known: dict[Any, int] | None = None
    ) -> None:
        self.db, self.branch = db, branch
        # Name ids read in earlier, committed writes (kept by the caller), and those read
        # or added in this one, which count only once it commits.
        self.known = known if known is not None else {}
        self.names: dict[Any, int] = {}
        self.branch_id = self.name(branch)
        # The ancestors whose rows this branch reads (their minute limits); a row they
        # hold already is not stored again.
        self.ancestors: list[tuple[int | None, int]] = []
        self.pending: dict[tuple[int, str], tuple[int, str]] = {}
        self.next_id = db.execute("SELECT COALESCE(MAX(id),0)+1 FROM records").fetchone()[0]
        self.records: list[tuple[Any, ...]] = []
        self.entities: list[tuple[int, int]] = []
        self.tokens: list[tuple[int, int]] = []
        self.links: list[tuple[int, int]] = []
        self.references: list[tuple[int, int]] = []
        self.added: dict[str, int] = {}

    def __enter__(self) -> _RecordBatch:
        return self

    def __exit__(self, error: type[BaseException] | None, *_: Any) -> None:
        if error is None:
            self.flush()

    def name(self, name: Any) -> int | None:
        """The id of a name, added to names if it is new; None stays None."""
        if name is None:
            return None
        # Only texts are remembered: 1, 1.0 and True are one key of a dict, not of names.
        text = type(name) is str
        found = self.known.get(name) if text else None
        if found is None and text:
            found = self.names.get(name)
        if found is None:
            row = self.db.execute("SELECT id FROM names WHERE name=?", (name,)).fetchone()
            if row is not None:
                found = row[0]
            else:
                cursor = self.db.execute("INSERT INTO names(name) VALUES(?)", (name,))
                found = cursor.lastrowid
            if text:
                self.names[name] = found
        return found

    def put(
        self,
        kind: str,
        row: dict[str, Any],
        *,
        key: str | None = None,
        position: int | None = None,
        kept: dict[str, Any] | None = None,
    ) -> None:
        """Put a row; with position, a row made from the EventLog record there, which is
        stored without its payload but for kept, the part of it the record cannot give
        (#138)."""
        code = _KIND_CODES.get(kind)
        if code is None:
            raise StorageError("history_index_kind_unknown")
        branch = self.branch_id
        # A plain row with nothing the privacy check changes is read in one pass in Rust.
        read = native.index_row(row)
        # The token text of a container depends on its key order, here not yet sorted.
        if read is not None and (read[5] or read[6] is None or _any_private_field(read[4])):
            read = None
        if read is None:
            row, encoded = safe_row(row)
            payload_hash = hashlib.sha256(encoded.encode()).hexdigest()  # digest(row)
            identities, references = _identities(row), _event_references(row)
        else:
            encoded, payload_hash, identities, references = read[:4]
        minute = row.get("minute", row.get("minute_index"))
        if type(minute) is not int or minute < 0:
            raise StorageError("invalid_index_record_minute")
        key = key or payload_hash
        stored_key = _stored_key(key)
        # A stored row is compared by the SHA-256 of its canonical JSON: compressed bytes
        # may differ between zlib builds (#80), and a row of version 5 may have no payload.
        existing = self.pending.get((code, key))
        if existing is None:
            stored = self.db.execute(
                "SELECT id,payload_hash FROM records WHERE branch=? AND kind=? AND key=?",
                (branch, code, stored_key),
            ).fetchone()
            if stored is not None:
                existing = (stored[0], stored[1].hex())
        if existing is None and code != _CONVERSATIONS:
            # An ancestor's rows before the fork minute are read as this branch's.
            for ancestor, limit in self.ancestors:
                if minute >= limit:
                    break
                stored = self.db.execute(
                    "SELECT payload_hash FROM records WHERE branch=? AND kind=? AND key=? "
                    "AND minute<?",
                    (ancestor, code, stored_key, limit),
                ).fetchone()
                if stored is not None:
                    if stored[0].hex() != payload_hash:
                        raise StorageError("immutable_history_record_conflict")
                    return
        if existing is not None:
            if existing[1] != payload_hash:
                raise StorageError("immutable_history_record_conflict")
            record = existing[0]
            self.references.extend((self.name(event), record) for event in sorted(set(references)))
            return
        record, self.next_id = self.next_id, self.next_id + 1
        self.pending[(code, key)] = (record, payload_hash)
        name = self.name
        self.records.append(
            (
                record,
                branch,
                code,
                minute,
                stored_key,
                name(row.get("event_id")),
                name(row.get("causation_id")),
                name(row.get("source_id")),
                name(row.get("event_type", row.get("action", row.get("kind")))),
                name(row.get("status")),
                position,
                history_payloads.encode(safe_row(kept)[1] if kept is not None else encoded)
                if position is None or kept is not None
                else None,
                bytes.fromhex(payload_hash),
            )
        )
        self.added[kind] = self.added.get(kind, 0) + 1
        # In sorted order, so that new names take the same ids in every process.
        self.entities.extend((name(item), record) for item in sorted(set(identities)))
        # Index bounded metadata, never long answers, raw prompts or arbitrary prose.
        words = _terms(
            read[6]
            if read is not None
            else " ".join(str(row.get(field, "")) for field in _TOKEN_FIELDS)
        )
        self.tokens.extend((name(word), record) for word in words)
        self.links.extend((name(event), record) for event in sorted(set(references)))
        if len(self.records) >= self._FLUSH_RECORDS:
            self.flush()

    def flush(self) -> None:
        db = self.db
        db.executemany("INSERT INTO records VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)", self.records)
        db.executemany("INSERT INTO entities VALUES(?,?)", self.entities)
        db.executemany("INSERT INTO tokens VALUES(?,?)", self.tokens)
        db.executemany("INSERT INTO links VALUES(?,?)", self.links)
        # Event links of records stored before; their own links are inserted above.
        db.executemany("INSERT OR IGNORE INTO links VALUES(?,?)", self.references)
        db.executemany(
            "INSERT INTO generations VALUES(?,?,?) ON CONFLICT(branch,kind) DO UPDATE SET "
            "value=value+excluded.value",
            [(self.branch, kind, count) for kind, count in self.added.items()],
        )
        self.pending.clear()
        self.records.clear()
        self.entities.clear()
        self.tokens.clear()
        self.links.clear()
        self.references.clear()
        self.added.clear()


def _leading(items: list[Any], before: Any) -> int:
    """How many leading items satisfy before."""
    count = 0
    for item in items:
        if not before(item):
            break
        count += 1
    return count


# How long a rebuild waits for readers of the old index file to close it, on Windows.
_REPLACE_ATTEMPTS, _REPLACE_PAUSE_SECONDS = 50, 0.1


def _replace_file(source: Path, target: Path) -> None:
    """Put source in target's place at once. On POSIX a reader keeps the file it opened;
    Windows refuses to replace a file any connection has open, so a short read (a status
    request, a frame) is waited for, and a longer one makes the rebuild be asked again."""
    for attempt in range(_REPLACE_ATTEMPTS):
        try:
            source.replace(target)
            return
        except PermissionError as exc:
            if attempt == _REPLACE_ATTEMPTS - 1:
                raise StorageError(
                    "history_index_unavailable: the old index is in use; try again"
                ) from exc
            time.sleep(_REPLACE_PAUSE_SECONDS)


class HistoryIndex:
    def __init__(
        self, store: ProjectStore, project_id: str, run_id: str, *, keep_writer: bool = False
    ) -> None:
        self.store, self.project_id, self.run_id = store, project_id, run_id
        self.path = store._path(project_id, "runs", run_id, "history.sqlite")
        # With keep_writer, write transactions share one connection, whose page cache
        # stays valid between them while no other connection writes; close() ends it.
        self._keep_writer = keep_writer
        self._writer: sqlite3.Connection | None = None
        # The name ids of committed writes, which never change in a file.
        self._names: dict[Any, int] = {}

    def close(self) -> None:
        """Close the kept write connection, if any."""
        writer, self._writer = self._writer, None
        if writer is not None:
            writer.close()

    def _open_writer(self) -> sqlite3.Connection:
        # Branches of a Run index their saves at once (#48); a writer waits for another
        # branch's write, up to a fork's copy of its parent's rows.
        connection = sqlite3.connect(self.path.as_uri() + "?mode=rwc", uri=True, timeout=120)
        connection.row_factory = sqlite3.Row
        try:
            # An index written while #268 used a write-ahead log keeps that mode in
            # the file; return it to the rollback journal (#269). With readers open
            # the mode stays until a later write; on other files this does nothing.
            connection.execute("PRAGMA journal_mode=DELETE")
            version = connection.execute("PRAGMA user_version").fetchone()[0]
            _check_version(version, new=True)
            if version != _SCHEMA_VERSION:
                connection.executescript(
                    "BEGIN;"
                    + _SCHEMA
                    + "".join(
                        f"INSERT OR IGNORE INTO kinds VALUES({code},'{name}');"
                        for code, name in enumerate(KINDS)
                    )
                    + f"PRAGMA user_version={_SCHEMA_VERSION};COMMIT;"
                )
            page_size = connection.execute("PRAGMA page_size").fetchone()[0]
            # A derived file cannot individually exceed the configured disk cap.
            connection.execute(f"PRAGMA max_page_count={max(1, self.store.max_bytes // page_size)}")
            # Hold one ingest's changed pages in memory. With the 2 MiB default the
            # growing index spills pages mid-transaction, each spill syncing the
            # journal first (#47). The commit still syncs as before.
            cache_kib = _KEPT_WRITER_CACHE_KIB if self._keep_writer else _WRITE_CACHE_KIB
            connection.execute(f"PRAGMA cache_size=-{cache_kib}")
        except BaseException:
            connection.close()
            raise
        return connection

    @contextmanager
    def connect(
        self, *, write: bool = False, saved_points: bool = False
    ) -> Iterator[sqlite3.Connection]:
        """A transaction on the index. With saved_points, a read of only the heads and
        checkpoints tables, whose form no version changed, and of a file of any version."""
        if not write and not self.path.exists():
            raise StorageError("history_index_required: explicitly rebuild the saved Run index")
        if self.path.is_symlink():
            raise StorageError("unsafe_history_index")
        if write:
            with self._write_transaction() as connection:
                yield connection
            return
        connection = sqlite3.connect(self.path.as_uri() + "?mode=ro", uri=True, timeout=10)
        connection.row_factory = sqlite3.Row
        try:
            version = connection.execute("PRAGMA user_version").fetchone()[0]
            if not (saved_points and version in _OLDER_VERSIONS):
                _check_version(version)
            connection.execute("PRAGMA query_only=ON")
            connection.execute("BEGIN")
            yield connection
        except sqlite3.Error as exc:
            raise StorageError(_storage_code(exc)) from exc
        finally:
            connection.close()

    @contextmanager
    def _write_transaction(self) -> Iterator[sqlite3.Connection]:
        keep = self._keep_writer
        connection = self._writer if keep else None
        try:
            if connection is None:
                connection = self._open_writer()
                if keep:
                    self._writer = connection
            connection.execute("BEGIN IMMEDIATE")
            yield connection
            if self.store.usage()["bytes"] > self.store.max_bytes:
                connection.rollback()
                raise StorageError("storage_capacity_exceeded")
            connection.commit()
        except sqlite3.Error as exc:
            if connection is not None:
                connection.rollback()
            raise StorageError(_storage_code(exc)) from exc
        except BaseException:
            # An error of the caller's rolled back when the connection closed; a kept
            # connection is used again.
            if keep and connection is not None:
                connection.rollback()
            raise
        finally:
            if not keep and connection is not None:
                connection.close()

    def ingest(
        self,
        branch: str,
        checkpoint: Any,
        *,
        observations: list[dict[str, Any] | bytes] | None = None,
        rebuild: bool = False,
        inherit_from: str | None = None,
    ) -> None:
        """Append only new immutable records; checkpoint lists are already in writer memory.

        A new branch forked from inherit_from reads the parent's rows before the fork
        minute instead of storing them (#256); it stores only rows from the fork minute
        on, and none its ancestors hold already.
        """
        with self.connect(write=True) as db, _RecordBatch(db, branch, self._names) as records:
            prior = db.execute("SELECT * FROM heads WHERE branch=?", (branch,)).fetchone()
            forked = inherit_from is not None and prior is None and not rebuild
            if forked:
                db.execute(
                    "INSERT INTO lineage VALUES(?,?,?)",
                    (branch, inherit_from, checkpoint.simulation_minute),
                )
            if rebuild:
                # A rebuilt branch derives every row itself and reads no ancestor.
                db.execute("DELETE FROM lineage WHERE branch=?", (branch,))
                # This index is a replaceable read model of immutable source logs.
                # Clear only the requested branch so a new projection version can
                # rebuild it without conflicting with rows derived by older code.
                # Saved conversation turns are their own append-only source and are
                # not reconstructed from simulation checkpoints, so retain them.
                derived = "SELECT id FROM records WHERE branch=? AND kind!=" + str(_CONVERSATIONS)
                for table in ("entities", "tokens", "links"):
                    db.execute(
                        f"DELETE FROM {table} WHERE record IN ({derived})", (records.branch_id,)
                    )
                db.execute(
                    "DELETE FROM records WHERE branch=? AND kind!=?",
                    (records.branch_id, _CONVERSATIONS),
                )
                db.execute(
                    "DELETE FROM generations WHERE branch=? AND kind!='conversations'", (branch,)
                )
                for table in ("heads", "checkpoints"):
                    db.execute(f"DELETE FROM {table} WHERE branch=?", (branch,))
                prior = None
            previous_minute = prior["minute"] if prior else -1
            counts = json.loads(prior["counts"]) if prior and not rebuild else {}
            if rebuild:
                previous_minute = -1
            records.ancestors = [
                (_name_id(db, name), limit)
                for name, limit in _segments(db, branch)[1:]
                if limit is not None
            ]
            if forked:
                # Events and KPI samples before the fork minute are the parent's rows;
                # histories are read again from the minute before, as after a save.
                fork_minute = checkpoint.simulation_minute
                events = checkpoint.world_state["event_log"]
                if isinstance(events, SavedItems):
                    counts["events"] = _leading(
                        events.raws, lambda raw: saved_realized_minute(raw) < fork_minute
                    )
                else:
                    counts["events"] = _leading(
                        events, lambda record: record["realized_minute"] < fork_minute
                    )
                counts["kpis"] = _leading(
                    checkpoint.metrics_state["snapshots"],
                    lambda sample: sample["minute_index"] < fork_minute,
                )
                previous_minute = fork_minute
            if previous_minute > checkpoint.simulation_minute:
                raise StorageError("history_index_ahead_of_checkpoint")
            known = db.execute(
                "SELECT hash FROM checkpoints WHERE branch=? AND minute=?",
                (branch, checkpoint.simulation_minute),
            ).fetchone()
            if known and known[0] != checkpoint.checkpoint_hash:
                raise StorageError("immutable_checkpoint_index_conflict")
            world, metrics = checkpoint.world_state, checkpoint.metrics_state
            events = world["event_log"]
            body_history = world.get("model_state", {}).get("body_state_v1", {}).get("history", [])
            body_effects_by_event: dict[str, list[dict[str, Any]]] = {}
            if isinstance(body_history, list):
                for item in body_history:
                    if not isinstance(item, dict) or not isinstance(item.get("event_id"), str):
                        continue
                    deltas = item.get("deltas", {})
                    if not isinstance(deltas, dict):
                        continue
                    observed_deltas = {
                        name: value
                        for name, value in deltas.items()
                        if name
                        in {
                            "positive_affect",
                            "negative_affect",
                            "reassurance",
                            "discontent",
                            "uncertainty",
                            "relationship_trust_delta",
                        }
                        and isinstance(value, (int, float))
                        and not isinstance(value, bool)
                    }
                    if not observed_deltas:
                        continue
                    body_effects_by_event.setdefault(item["event_id"], []).append(
                        {
                            "state_kind": item.get("kind"),
                            "agent_ids": item.get("actor_ids", []),
                            "deltas": observed_deltas,
                        }
                    )
            geometry = None
            offset = counts.get("events", 0)
            if offset > len(events):
                raise StorageError("history_event_prefix_shortened")
            keys = getattr(checkpoint, "event_keys", None)
            for index, record in enumerate(events[offset:], start=offset):
                event = record["event"]
                # The sent bytes are the record's canonical JSON, which digest() would write again.
                records.put(
                    "events",
                    _event_row(record),
                    key=keys[index - offset] if keys else digest(record),
                    position=index,
                )
                if record["status"] == "applied":
                    event_type = event.get("event_type")
                    payload = event.get("payload", {})
                    social = _social_row(record)
                    if social is not None:
                        records.put(
                            "social", social, key="event:" + event["event_id"], position=index
                        )
                        if event_type == "social_interaction":
                            effects = body_effects_by_event.get(event["event_id"], [])
                            # Its body-state effects may have left the head Checkpoint's
                            # history, so the dialogue row keeps them (#138).
                            records.put(
                                "dialogue",
                                _dialogue_row(social, effects),
                                key="event:" + event["event_id"],
                                position=index,
                                kept={"observed_model_effects": effects},
                            )
                    action = _action_row(record)
                    continuity_is_shared = event[
                        "event_type"
                    ] == "staff_support_continuity" and "shared_operations" in world.get(
                        "model_configs", {}
                    )
                    if action is not None and not continuity_is_shared:
                        index_kind = {
                            "care": "care",
                            "operations": "operations",
                        }.get(action.get("record_kind"), "actions")
                        records.put(
                            index_kind, action, key="applied:" + event["event_id"], position=index
                        )
                    if event["event_type"] in {
                        "care_support_transition",
                        "staff_support_continuity",
                        "staff_handoff",
                        "facility_recovery_transition",
                        "staff_operation_transition",
                    }:
                        if "shared_operations" not in world.get("model_configs", {}):
                            raise StorageError("operation_event_without_signed_contract")
                        try:
                            receipt = _operation_receipts().validate_python(event["payload"])
                        except (TypeError, ValueError) as exc:
                            raise StorageError("operation_event_receipt_invalid") from exc
                        if receipt.causation_id != event.get("causation_id") or (
                            event.get("source_id") is not None
                            and event["source_id"] not in receipt.source_ids
                        ):
                            raise StorageError("operation_event_receipt_provenance_mismatch")
                        row = receipt.model_dump(mode="json")
                        row["source_id"] = event.get("source_id") or next(
                            iter(row.get("source_ids", ())), None
                        )
                        row["minute"] = record["realized_minute"]
                        if (
                            receipt.event_id != event["event_id"]
                            or receipt.minute != record["realized_minute"]
                        ):
                            raise StorageError("operation_event_receipt_time_or_id_mismatch")
                        records.put(
                            "care" if row["record_type"] == "care" else "operations",
                            row,
                            key=f"{row['record_type']}:{receipt.event_id}",
                        )
                    if event_type == "facility_queue_transition" and isinstance(payload, dict):
                        from sheltersim.contracts.extensions import FacilityGeometry
                        from sheltersim.domain.facility_queues import (
                            FACILITY_QUEUES_POLICY_VERSION,
                            FACILITY_QUEUES_STATE_KEY,
                        )

                        try:
                            receipt = _queue_receipts().validate_python(payload)
                            if geometry is None:
                                # One checkpoint has one geometry; parse it once.
                                geometry = FacilityGeometry.model_validate(
                                    world.get("model_configs", {}).get("facility_geometry", {})
                                )
                        except (TypeError, ValueError) as exc:
                            raise StorageError("facility_queue_event_receipt_invalid") from exc
                        point = next(
                            (
                                row
                                for row in geometry.service_points
                                if row.queue_id == receipt.queue_id
                            ),
                            None,
                        )
                        if (
                            receipt.event_id != event.get("event_id")
                            or receipt.minute != record["realized_minute"]
                            or receipt.causation_id != event.get("causation_id")
                            or event.get("source_id") != FACILITY_QUEUES_POLICY_VERSION
                            or FACILITY_QUEUES_STATE_KEY not in world.get("model_state", {})
                            or point is None
                            or (point.id, point.facility_id)
                            != (receipt.service_point_id, receipt.facility_id)
                        ):
                            raise StorageError("facility_queue_event_receipt_provenance_mismatch")
                        row = receipt.model_dump(mode="json")
                        row["source_id"] = event.get("source_id")
                        records.put("operations", row, key=f"operations:{receipt.event_id}")
            counts["events"] = len(events)
            snapshots = metrics["snapshots"]
            start = counts.get("kpis", 0)
            resource_rows = metrics.get("resource_snapshots", [])
            for index in range(start, len(snapshots)):
                row = dict(snapshots[index])
                row.pop("source_ids_json", None)
                if resource_rows:
                    row.update(resource_rows[index])
                row["time_semantics"] = "end_of_tick"
                records.put("kpis", row, key=str(row["minute_index"]))
            counts["kpis"] = len(snapshots)
            counts["model_namespaces"] = sorted(world["model_state"])
            counts.setdefault("first_indexed_checkpoint_minute", checkpoint.simulation_minute)
            for kind, namespace, field in (
                ("social", "body_state_v1", "minute"),
                ("staff", "staff_operations_v1", "complete_minute"),
                ("transitions", "population_transitions_v1", "minute"),
                ("transitions", "population_presence_v1", "minute"),
                ("transitions", "population_lifecycle_v1", "minute"),
            ):
                history = world["model_state"].get(namespace, {}).get("history", [])
                # Histories are chronological, may roll over, and may contain several
                # records at one minute. Revisit that minute; stable hashes deduplicate.
                first = bisect_left(
                    history, max(0, previous_minute - 1), key=lambda row: row[field]
                )
                for record in history[first:]:
                    records.put(kind, {**record, "minute": record[field]})
            for row in observations or []:
                # rules/decisions.rs writes a decision's row as JSON.
                if type(row) is bytes:
                    row = json.loads(row)
                records.put(row.get("record_kind", "actions"), row)
            db.execute(
                "INSERT OR REPLACE INTO checkpoints VALUES(?,?,?,?)",
                (
                    branch,
                    checkpoint.simulation_minute,
                    checkpoint.checkpoint_hash,
                    checkpoint.state_hash,
                ),
            )
            db.execute(
                "INSERT OR REPLACE INTO heads VALUES(?,?,?,?)",
                (
                    branch,
                    checkpoint.checkpoint_hash,
                    checkpoint.simulation_minute,
                    canonical(counts),
                ),
            )

        self._remember(records.names)

    def _remember(self, names: dict[Any, int]) -> None:
        """Keep the name ids of a committed write for the next writes of a kept writer."""
        if not self._keep_writer:
            return
        if len(self._names) + len(names) > _KEPT_NAMES:
            self._names.clear()
        self._names.update(names)

    def add_turn(self, branch: str, turn: dict[str, Any]) -> None:
        with self.connect(write=True) as db, _RecordBatch(db, branch, self._names) as records:
            records.put("conversations", turn, key=turn["turn_id"])
        self._remember(records.names)

    def ready(self, db: sqlite3.Connection, branch: str, anchor: str) -> None:
        row = db.execute("SELECT hash FROM heads WHERE branch=?", (branch,)).fetchone()
        if row is None or row[0] != anchor:
            raise StorageError("history_index_stale: explicitly resume index rebuild")

    def answering_head(
        self, db: sqlite3.Connection, branch: str, anchor: str, anchor_minute: int | None
    ) -> tuple[str, int]:
        """The indexed head to answer as of, and its minute, for a branch whose pointer
        is at anchor (at anchor_minute).

        A save moves the branch pointer before it indexes the new head, so a read in
        between finds the index behind the pointer (#259, #262). Saved Checkpoints never
        change and a branch only grows, so the index still answers as of its own head.
        An index ahead of the pointer, or without the branch, is stale.
        """
        head = db.execute("SELECT hash,minute FROM heads WHERE branch=?", (branch,)).fetchone()
        if head is None or (
            head[0] != anchor and (anchor_minute is None or head[1] >= anchor_minute)
        ):
            raise StorageError("history_index_stale: explicitly resume index rebuild")
        return head[0], head[1]

    def nearest(
        self, branch: str, minute: int, anchor: str, anchor_minute: int | None = None
    ) -> tuple[int, str] | None:
        """The latest saved Checkpoint at or before minute on a branch whose head is anchor;
        the anchor itself is the head at anchor_minute."""
        with self.connect(saved_points=True) as db:
            answered, _ = self.answering_head(db, branch, anchor, anchor_minute)
            if answered != anchor and anchor_minute is not None and minute >= anchor_minute:
                return anchor_minute, anchor
            row = db.execute(
                "SELECT minute,hash FROM checkpoints WHERE branch=? AND minute<=? ORDER BY "
                "minute DESC LIMIT 1",
                (branch, minute),
            ).fetchone()
            return (row[0], row[1]) if row else None

    def search(
        self, branch: str, query: Any, anchor: str, anchor_minute: int | None = None
    ) -> dict[str, Any]:
        """Records of a branch whose pointer is at anchor (at anchor_minute), as of the
        indexed head that answering_head() gives; the answer names that head."""
        scope = digest(
            {
                "project": self.project_id,
                "run": self.run_id,
                "branch": branch,
                **query.model_dump(mode="json", exclude={"cursor", "limit"}),
            }
        )
        kind = "events" if query.kind == "causal" else query.kind
        kinds = (
            (
                "events",
                "actions",
                "social",
                "staff",
                "transitions",
                "dialogue",
                "care",
                "operations",
            )
            if query.kind == "causal" and query.include_related
            else (kind,)
        )
        codes = [_KIND_CODES[name] for name in kinds]
        graph = None
        with self.connect() as db:
            anchor, indexed_minute = self.answering_head(db, branch, anchor, anchor_minute)
            head_hash = anchor
            # A filter whose value no row holds takes the id None, which matches nothing.
            ids: dict[Any, int | None] = {}

            def name_id(name: Any) -> int | None:
                if name not in ids:
                    ids[name] = _name_id(db, name)
                return ids[name]

            where, args = (
                [
                    "r.branch=?",
                    "r.kind IN (" + ",".join("?" for _ in codes) + ")",
                    "r.minute BETWEEN ? AND ?",
                ],
                [None, *codes, query.start, query.end],
            )
            if kind == "kpis":
                where.append("(r.minute-?) % ? = 0")
                args.extend([query.start, query.step])
            for identity in (
                query.agent_id,
                query.household_id,
                query.staff_id,
                query.zone_id,
                query.conversation_id,
            ):
                if identity:
                    where.append(
                        "EXISTS(SELECT 1 FROM entities e WHERE e.entity=? AND e.record=r.id)"
                    )
                    args.append(name_id(identity))
            for column, value in (
                ("type", query.event_type),
                ("source", query.source_id),
                ("status", query.status),
            ):
                if value is not None:
                    where.append(f"r.{column}=?")
                    args.append(name_id(value))
            if query.event_id and query.kind != "causal":
                where.append("EXISTS(SELECT 1 FROM links l WHERE l.event=? AND l.record=r.id)")
                args.append(name_id(query.event_id))
            for term in _terms(query.text or ""):
                where.append("EXISTS(SELECT 1 FROM tokens t WHERE t.token=? AND t.record=r.id)")
                args.append(name_id(term))
            # Conversation turns belong to their branch; other rows are read through its
            # lineage.
            segments = [
                (name_id(name), limit)
                for name, limit in (
                    [(branch, None)] if kind == "conversations" else _segments(db, branch)
                )
            ]
            # The rows of the branch's lineage. A cursor is bound to the query (scope), not to
            # these (#278).
            lineage, lineage_args = _scope(segments)
            where[0] = lineage
            args[:1] = lineage_args

            def position(cursor: tuple[int, str] | None, allowed: tuple[str, ...]) -> None:
                if cursor is None:
                    return
                if ":" not in cursor[1]:
                    raise StorageError("stale_or_foreign_cursor")
                after_kind, after_key = cursor[1].split(":", 1)
                if after_kind not in allowed:
                    raise StorageError("stale_or_foreign_cursor")
                where.append(f"(r.minute,r.kind,{_R_KEY})>(?,?,?)")
                args.extend((cursor[0], _KIND_CODES[after_kind], after_key))

            if query.kind != "conversations":
                position(cursor_decode(query.cursor, scope, anchor), kinds)
            if query.kind == "causal":
                root = name_id(query.event_id)
                # Event ids by name id; the root counts even if no row names it.
                visited, frontier = {root}, [root]
                link_limit_hit = False
                while frontier and len(visited) < 500:
                    current = frontier.pop(0)
                    links = db.execute(
                        # Ordered, so that a graph cut off at 500 events keeps the same ones
                        # whatever order the rows and names were written in (#48, #130).
                        f"SELECT r.event,r.cause FROM records r WHERE {lineage} AND "
                        f"r.kind={_EVENTS} AND (r.event=? OR r.cause=?) "
                        f"ORDER BY r.minute,{_R_KEY} LIMIT 501",
                        (*lineage_args, current, current),
                    ).fetchall()
                    link_limit_hit = link_limit_hit or len(links) >= 501
                    for row in links:
                        for other in row:
                            if other and other not in visited:
                                if len(visited) == 500:
                                    break
                                visited.add(other)
                                frontier.append(other)
                linked = sorted(event for event in visited if event is not None)
                where.append(
                    "EXISTS(SELECT 1 FROM links l WHERE l.record=r.id AND l.event IN ("
                    + ",".join("?" for _ in linked)
                    + "))"
                )
                args.extend(linked)
                graph = {
                    "causal_graph_truncated": link_limit_hit
                    or bool(frontier)
                    or len(visited) >= 500,
                    "root_present": db.execute(
                        f"SELECT 1 FROM records r WHERE {lineage} AND r.kind={_EVENTS} AND "
                        "r.event=? LIMIT 1",
                        (*lineage_args, root),
                    ).fetchone()
                    is not None,
                }
            if kind == "conversations":
                counter = db.execute(
                    "SELECT value FROM generations WHERE branch=? AND kind='conversations'",
                    (branch,),
                ).fetchone()
                anchor = digest({"head": anchor, "turn_count": counter[0] if counter else 0})
                position(cursor_decode(query.cursor, scope, anchor), ("conversations",))
            # One ordered select per branch of the lineage keeps each on its index;
            # the compound select merges them in order.
            parts, part_args = [], []
            rest, rest_args = where[1:], args[len(lineage_args) :]
            for segment in segments:
                segment_scope, segment_args = _scope([segment])
                parts.append(
                    f"SELECT r.branch,r.kind,r.minute,{_R_KEY} AS key,r.position,r.payload,"
                    "r.payload_hash "
                    "FROM records r "
                    "WHERE " + " AND ".join([segment_scope, *rest])
                )
                part_args.extend((*segment_args, *rest_args))
            rows = db.execute(
                " UNION ALL ".join(parts) + " ORDER BY minute,kind,key LIMIT ?",
                (*part_args, query.limit + 1),
            ).fetchall()
            coverage_rows = db.execute(
                "SELECT r.kind,MIN(r.minute),MAX(r.minute),COUNT(*) FROM records r "
                f"WHERE {lineage} "
                "AND r.kind IN (" + ",".join("?" for _ in codes) + ") "
                "AND r.minute BETWEEN ? AND ? GROUP BY r.kind",
                (*lineage_args, *codes, query.start, query.end),
            ).fetchall()
            head = db.execute("SELECT counts FROM heads WHERE branch=?", (branch,)).fetchone()
            selected = rows[: query.limit]
            texts = self.payload_texts(db, selected, {segments[0][0]: head_hash})
        coverage_by_kind = {
            KINDS[row[0]]: {
                "oldest_minute": row[1],
                "newest_minute": row[2],
                "indexed_records": row[3],
            }
            for row in coverage_rows
        }
        indexed_minutes = [
            minute
            for item in coverage_by_kind.values()
            for minute in (item["oldest_minute"], item["newest_minute"])
            if minute is not None
        ]
        counts = json.loads(head[0]) if head is not None else {}
        items = [
            {**json.loads(text), "record_kind": KINDS[row["kind"]]}
            for row, text in zip(selected, texts, strict=True)
        ]
        following = (
            cursor_encode(
                {
                    "scope": scope,
                    "anchor": anchor,
                    "minute": selected[-1]["minute"],
                    "key": KINDS[selected[-1]["kind"]] + ":" + selected[-1]["key"],
                }
            )
            if len(rows) > query.limit
            else None
        )
        return {
            "schema_version": 1,
            "items": items,
            "next_cursor": following,
            "anchor_hash": anchor,
            "indexed_through_minute": indexed_minute,
            "total_items": None,
            "count_status": "not_calculated",
            "query_strategy": "indexed_keyset",
            "coverage": "persisted_index_records",
            "coverage_scope": "branch_kind_time_window_before_identity_and_text_filters",
            "completeness": "unknown",
            "completeness_reason": (
                "The index exposes persisted producer records but cannot certify that every "
                "domain event was produced or retained."
            ),
            "oldest_minute": min(indexed_minutes) if indexed_minutes else None,
            "newest_minute": max(indexed_minutes) if indexed_minutes else None,
            "retention_limit": None,
            "retention_limit_reason": (
                "No per-kind source eviction boundary is persisted; absence of records is not "
                "evidence that no event occurred."
            ),
            "requested_range": [query.start, query.end],
            "coverage_by_kind": coverage_by_kind,
            "first_indexed_checkpoint_minute": counts.get("first_indexed_checkpoint_minute"),
            "complete_lifetime_history": False,
            "causal_interpretation": "Recorded model links, not empirical causal proof",
            **(graph or {}),
        }

    def payload_texts(
        self, db: sqlite3.Connection, rows: list[sqlite3.Row], heads: dict[int, str] | None = None
    ) -> list[str]:
        """The canonical JSON of each row (with its branch, kind, key, position, payload and
        payload_hash), checked against its hash. A row with a position is made again from
        the EventLog record there in its branch's head Checkpoint (#138), with the part of it
        the row keeps, if any: heads, by branch id, or else the heads table. EventLog records
        are only appended, so a head holds every record its branch indexed, at the same
        position. A key is read as its text or in its stored form."""
        heads = dict(heads or {})
        wanted: dict[int, set[int]] = {}
        for row in rows:
            if row["position"] is not None:
                wanted.setdefault(row["branch"], set()).add(row["position"])
        sources: dict[tuple[int, int], dict[str, Any]] = {}
        for branch_id, positions in sorted(wanted.items()):
            if branch_id not in heads:
                owner = db.execute(
                    "SELECT h.hash FROM heads h JOIN names n ON n.name=h.branch WHERE n.id=?",
                    (branch_id,),
                ).fetchone()
                if owner is None:
                    raise StorageError("history_index_stale: explicitly resume index rebuild")
                heads[branch_id] = owner[0]
            records = self.store.checkpoint_items(
                self.project_id,
                self.run_id,
                heads[branch_id],
                ("world_state", "event_log"),
                sorted(positions),
            )
            for position, record in records.items():
                sources[branch_id, position] = record
        texts = []
        for row in rows:
            if row["position"] is None:
                text = history_payloads.decode(row["payload"])
            else:
                record = sources[row["branch"], row["position"]]
                kept = history_payloads.decode(row["payload"]) if row["payload"] else None
                key = row["key"].hex() if isinstance(row["key"], bytes) else row["key"]
                derived = (
                    _derived_row(row["kind"], key, record, kept)
                    if isinstance(record, dict) and isinstance(record.get("event"), dict)
                    else None
                )
                if derived is None:
                    raise StorageError(
                        "history_index_record_corrupt: rebuild from immutable source"
                    )
                text = safe_row(derived)[1]
            if hashlib.sha256(text.encode()).digest() != row["payload_hash"]:
                raise StorageError("history_index_record_corrupt: rebuild from immutable source")
            texts.append(text)
        return texts

    def rebuild(self, branch: str) -> dict[str, Any]:
        """Explicit legacy migration, never a hidden GET side effect or simulation."""
        lease = self.store.claim_run(self.project_id, self.run_id)
        try:
            if self.path.exists() and self.version() == 0:
                # An empty file, left by a writer stopped before it made the tables.
                self._remove()
            if self.path.exists() and self.version() in _OLDER_VERSIONS:
                return self._replace_older(branch)
            reference = self.store.branch_reference(self.project_id, self.run_id, branch)
            if self.path.exists():
                with self.connect() as db:
                    head = db.execute("SELECT hash FROM heads WHERE branch=?", (branch,)).fetchone()
                if head is not None and head["hash"] == reference.checkpoint_hash:
                    # The action and dialogue decisions recorded while computing live only
                    # in this index, not in any Checkpoint; a rebuild would drop them (#213).
                    raise StorageError(
                        "history_index_current: the index already matches the Branch head; "
                        "rebuilding it would drop the decisions recorded while computing"
                    )
            return self._rebuild_branch(branch, [])
        finally:
            self.store.release_run(lease)

    def _replace_older(self, branch: str) -> dict[str, Any]:
        """Rebuild every branch of a Run whose index is of an older version into a new file,
        which then replaces the old one; until then the old file stays as it was (#130).

        The saved points of the old file (its checkpoints table, the same in every version)
        are kept; its other rows, the decisions recorded while computing among them, are
        derived again from each branch's head or lost."""
        with self.connect(saved_points=True) as db:
            saved = [tuple(row) for row in db.execute("SELECT * FROM checkpoints")]
        branches = sorted(
            item["branch_id"] for item in self.store.branches(self.project_id, self.run_id)
        )
        if branch not in branches:
            self.store.branch_reference(self.project_id, self.run_id, branch)
        target = self.path.with_name(self.path.name + ".rebuild")
        new = HistoryIndex(self.store, self.project_id, self.run_id)
        new.path = target
        new._remove()
        try:
            results = {
                name: new._rebuild_branch(name, [row for row in saved if row[0] == name])
                for name in branches
            }
            self.close()
            _replace_file(target, self.path)
        except BaseException:
            new._remove()
            raise
        return {**results[branch], "rebuilt_branch_ids": branches}

    def _rebuild_branch(self, branch: str, saved: list[tuple[Any, ...]]) -> dict[str, Any]:
        """Index a branch again from its head Checkpoint, with the saved points of its
        legacy timeline.json and saved (rows of an older index's checkpoints table)."""
        from types import SimpleNamespace

        reference = self.store.branch_reference(self.project_id, self.run_id, branch)
        document = self.store.checkpoint_document(
            self.project_id, self.run_id, reference.checkpoint_hash
        )
        checkpoint = SimpleNamespace(**document)
        self.ingest(branch, checkpoint, rebuild=True)
        legacy = self.store._path(
            self.project_id, "runs", self.run_id, "branches", branch, "timeline.json"
        )
        entries = self.store._read(legacy)["checkpoints"] if legacy.exists() else {}
        points = [(int(minute), key, "") for minute, key in entries.items()]
        for _, minute, key, state_hash in saved:
            # An older index's points are the branch's own while within its pointer.
            if reference.fork_minute <= minute <= reference.computed_until:
                points.append((minute, key, state_hash))
        with self.connect(write=True) as db:
            for minute, key, state_hash in points:
                if (
                    not isinstance(key, str)
                    or not re.fullmatch(r"[a-f0-9]{64}", key)
                    or not reference.fork_minute <= minute <= reference.computed_until
                ):
                    raise StorageError("invalid_legacy_checkpoint_index")
                # The original checkpoint is hash-validated when selected for replay.
                known = db.execute(
                    "SELECT hash,state_hash FROM checkpoints WHERE branch=? AND minute=?",
                    (branch, minute),
                ).fetchone()
                if known and known[0] != key:
                    raise StorageError("immutable_checkpoint_index_conflict")
                if known and not known[1] and state_hash:
                    db.execute(
                        "UPDATE checkpoints SET state_hash=? WHERE branch=? AND minute=?",
                        (state_hash, branch, minute),
                    )
                db.execute(
                    "INSERT OR IGNORE INTO checkpoints VALUES(?,?,?,?)",
                    (branch, minute, key, state_hash),
                )
        for turn in self.store.conversations(self.project_id, self.run_id, branch):
            self.add_turn(branch, turn)
        return {
            "schema_version": 1,
            "status": "indexed_not_verified",
            "branch_id": branch,
            "checkpoint_hash": checkpoint.checkpoint_hash,
            "limits": (
                "Legacy actions/expired contacts absent from source cannot be reconstructed "
                "by an index. Action and dialogue decisions recorded while computing are "
                "not in Checkpoints and are not restored."
            ),
        }

    def version(self) -> int:
        """The version of the index file, which must exist; it reads nothing else."""
        if self.path.is_symlink():
            raise StorageError("unsafe_history_index")
        try:
            connection = sqlite3.connect(self.path.as_uri() + "?mode=ro", uri=True, timeout=10)
            try:
                return connection.execute("PRAGMA user_version").fetchone()[0]
            finally:
                connection.close()
        except sqlite3.Error as exc:
            raise StorageError(_storage_code(exc)) from exc

    def require_current(self) -> None:
        """Refuse, before computing or forking, a Run whose index is of another version."""
        if self.path.exists():
            # An empty file, left by a writer stopped before it made the tables, is taken.
            _check_version(self.version(), new=True)

    def _remove(self) -> None:
        """Delete the index file and its rollback journal; the caller holds the Run."""
        self.close()
        for path in (self.path, self.path.with_name(self.path.name + "-journal")):
            try:
                path.unlink(missing_ok=True)
            except OSError as exc:
                raise StorageError("history_index_unavailable: the old index is in use") from exc

    def latest_kpis(self, db: sqlite3.Connection, branch: str) -> dict[str, Any] | None:
        """The last KPI sample a branch itself indexed, checked against its hash."""
        row = db.execute(
            "SELECT payload,payload_hash FROM records WHERE branch=? AND kind=? "
            f"ORDER BY minute DESC,{_PLAIN_KEY} DESC LIMIT 1",
            (_name_id(db, branch), _KIND_CODES["kpis"]),
        ).fetchone()
        if row is None:
            return None
        metrics = json.loads(history_payloads.decode(row["payload"]))
        return metrics if digest(metrics) == row["payload_hash"].hex() else None

    def checkpoints(
        self, branch: str, start: int, end: int, limit: int, cursor: str | None
    ) -> dict[str, Any]:
        reference = self.store.branch_reference(self.project_id, self.run_id, branch)
        if (
            not reference.fork_minute <= start <= end <= reference.computed_until
            or not 1 <= limit <= 100
        ):
            raise StorageError("invalid_checkpoint_page_range")
        scope = digest(
            {
                "run": self.run_id,
                "branch": branch,
                "start": start,
                "end": end,
                "kind": "checkpoints",
            }
        )
        with self.connect(saved_points=True) as db:
            anchor, indexed_minute = self.answering_head(
                db, branch, reference.checkpoint_hash, reference.computed_until
            )
            after = cursor_decode(cursor, scope, anchor)
            rows = db.execute(
                "SELECT minute,hash,state_hash FROM checkpoints WHERE branch=? AND minute "
                "BETWEEN ? AND ? AND minute>? ORDER BY minute LIMIT ?",
                (branch, start, end, after[0] if after else -1, limit + 1),
            ).fetchall()
        selected = rows[:limit]
        return {
            "schema_version": 1,
            "items": [dict(row) for row in selected],
            "anchor_hash": anchor,
            "indexed_through_minute": indexed_minute,
            "next_cursor": cursor_encode(
                {
                    "scope": scope,
                    "anchor": anchor,
                    "minute": selected[-1]["minute"],
                    "key": "",
                }
            )
            if len(rows) > limit
            else None,
            "total_items": None,
            "query_strategy": "indexed_keyset",
        }

    def conversation_tail(
        self, branch: str, conversation_id: str | None, limit: int = 6
    ) -> list[dict[str, Any]]:
        if not 1 <= limit <= 100:
            raise StorageError("invalid_page_limit")
        reference = self.store.branch_reference(self.project_id, self.run_id, branch)
        with self.connect() as db:
            self.answering_head(db, branch, reference.checkpoint_hash, reference.computed_until)
            rows = _conversation_turns(db, branch, conversation_id, limit)
        turns = []
        for row in rows:
            turn = json.loads(history_payloads.decode(row["payload"]))
            if digest(turn) != row["payload_hash"].hex():
                raise StorageError("history_index_record_corrupt")
            turns.append(turn)
        return turns[::-1]
