"""The Rust kernels (docs/rust-port.md).

The sheltersim_native extension is required; docs/development.md ("Rustのkernel") builds and
installs it. A kernel returns exactly the bytes of the Python code it
replaced. Where a function here returns None, or a kernel raises Unsupported, the kernel leaves
that value to the Python code that the caller still has.
"""

from __future__ import annotations

from typing import Any

try:
    import sheltersim_native as _native
except ImportError as exc:  # pragma: no cover - the package cannot run without it
    raise ImportError(
        "sheltersim needs its Rust extension sheltersim_native; build and install it as "
        "docs/development.md (Rustのkernel) says"
    ) from exc


# Catch this around a kernel from rules() or state(): the Python code then decides.
Unsupported: type[Exception] = _native.Unsupported


def rules(name: str) -> Any:
    """The module of native/src/rules/<name>.rs.

    Porting lanes call their kernels through it and need no function in this file.
    """
    return getattr(_native.rules, name)


def state(name: str) -> Any:
    """The module of native/src/state/<name>.rs."""
    return getattr(_native.state, name)


def world_core() -> Any:
    """A new sheltersim_native.WorldCore (core/state_mirror.py)."""
    return _native.WorldCore()


def install_tracked_types(*classes: type) -> None:
    """Tell the kernels which dict and list subclasses read as plain ones (core/tracked.rs)."""
    _native.install_tracked_types(*classes)


def install_id_map_type(id_map: type) -> None:
    """Tell the kernels that a world's maps of entities read as plain dicts (core/tracked.rs)."""
    _native.install_id_map_type(id_map)


def install_detached_types(table: type, placeholder: object) -> None:
    """Tell the kernels the class of model_state while a namespace is detached, and the
    placeholder it holds for that namespace (core/tracked.rs)."""
    _native.install_detached_types(table, placeholder)


def canonical_json(value: Any) -> bytes | None:
    """json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False,
    allow_nan=False).encode(), or None when the kernel leaves value to json."""
    try:
        return _native.canonical_json(value)
    except _native.Unsupported:
        return None


def canonical_sha256(value: Any) -> str | None:
    """The SHA-256 hex digest of canonical_json(value), or None."""
    try:
        return _native.canonical_sha256(value)
    except _native.Unsupported:
        return None


def scheduled_event_checked(*arguments: Any) -> tuple[Any, Any] | None:
    """The frozen payload and provenance of an event that passes ScheduledEvent's checks,
    or None for the Python checks to decide, which raise any error."""
    try:
        return _native.scheduled_event_checked(*arguments)
    except _native.Unsupported:
        return None


def view_state_hash(*arguments: Any) -> str | None:
    """The audit hash of a WorldView, or None when the kernel leaves it to Python."""
    try:
        return _native.view_state_hash(*arguments)
    except (_native.Unsupported, TypeError):
        return None


def movement_positions_valid(positions: Any, zones: Any, edges: Any) -> bool:
    """True only if every movement_v1 position passes the Python rules."""
    return _native.movement_positions_valid(positions, zones, edges)


def world_agents_valid(
    agents: Any, zones: Any, activity_type: Any, initial_needs_type: Any, needs_type: Any
) -> bool:
    """True only if every AgentState passes the per-Agent rules of a v6 WorldState."""
    return _native.world_agents_valid(agents, zones, activity_type, initial_needs_type, needs_type)


def index_row(row: dict[str, Any]) -> tuple[str, str, list, list, str, list, str | None] | None:
    """(canonical JSON, its SHA-256, identities, event references, sorted keys joined by NUL,
    strings to check, the text its tokens are read from or None) of a row of plain lists,
    str-keyed dicts and JSON scalars (storage/history_index.py), or None for any other row.
    Identities and references may repeat."""
    return _native.index_row(row)


def dumps_length(value: Any) -> int | None:
    """len(json.dumps(value, ensure_ascii=False).encode()), or None to leave it to json."""
    try:
        return _native.dumps_length(value)
    except _native.Unsupported:
        return None


def record_json(record: Any, *types: type) -> bytes | None:
    """The canonical JSON of record.to_dict() for a record of the types EventLogRecord and
    ScheduledEvent with only frozen plain values, or None for any other."""
    return _native.record_json(record, *types)


def history_pins() -> Any:
    """What tells cheaply that a history's entries were not changed (history.rs)."""
    return _native.HistoryPins()


def same_saved_records(records: list[Any], raws: list[bytes], *types: type) -> bool:
    """Whether each of records (of the types EventLogRecord and ScheduledEvent) is exactly the
    record restoring raws gives, read from the bytes; a false says nothing about ==, and
    the caller compares restored records."""
    return _native.same_saved_records(records, raws, *types)


def same_json_items(current: Any, documents: list[Any], start: int, count: int) -> bool:
    """Whether each of current[:count] is exactly documents[start + i], types included
    (1, 1.0 and True differ, and a tuple and a list)."""
    return _native.same_json_items(current, documents, start, count)


def values_snapshot() -> Any:
    """A new held list of a dict's values (sheltersim_native.ValuesSnapshot)."""
    return _native.ValuesSnapshot()


def event_store() -> Any:
    """A new Rust EventLog store (sheltersim_native.EventStore)."""
    return _native.EventStore()


def split_json_array(raw: bytes) -> list[bytes]:
    """The bytes of each item of the canonical JSON array raw.

    Raises ValueError when raw is not one JSON array.
    """
    return _native.split_json_array(raw)


def sensitive_candidates(raws: list[bytes]) -> tuple[list[str], list[str]]:
    """The distinct object keys of the JSON texts raws, and their distinct string values the
    privacy patterns could match. Every other string value
    can not match them (native/src/checkpoint.rs). Raises ValueError on a bad string."""
    return _native.sensitive_candidates(raws)


def chunk_hashes(items: list[bytes], chunks: list[tuple[int, int]]) -> list[str]:
    """The SHA-256 hex digest of b"[" + b",".join(items[start : start + count]) + b"]" for
    each (start, count) of chunks, computed in parallel."""
    return _native.chunk_hashes(items, chunks)


def content_chunk_starts(items: list[bytes], average: int, maximum: int) -> list[int]:
    """Where each archive chunk of items starts when chunks end where the items' own
    bytes say (native/src/checkpoint.rs), on average every `average` items and after at
    most `maximum`; for lists that drop or insert items (#138)."""
    return _native.content_chunk_starts(items, average, maximum)
