"""Compare rows_valid with the Python rules of validate_body_state.

record_social_event has no Python rule left to compare with (`kernel_only`).

At each rows_valid that says yes the Python rules run too: a yes they turn down is a difference. A
no they pass is counted with its reason: it only costs time. Each PROBE_EVERY-th yes also breaks a
restored copy with a value 1% beyond a bound, in a record and in a relationship that was accepted
before, and with a key naming nobody. The kernel must say no to each.
"""

from __future__ import annotations

import copy
from typing import Any

from shadow_kit import Recorder

KEY = "body_state_v1"
PROBE_EVERY = 997


def prepare(output: str) -> None:
    from sheltersim import native

    state_module = native.state("body_state")
    recorder = Recorder(output, "body_state")
    unsupported = native.Unsupported
    rows_kernel = state_module.rows_valid
    module: list[Any] = []

    def body_state() -> Any:
        if not module:
            from sheltersim.domain import body_state as loaded

            module.append(loaded)
        return module[0]

    probing = [False]

    def probe(world: Any) -> None:
        try:
            document = world.to_dict()
        except (ValueError, TypeError, KeyError):
            recorder.seen["rows.probe_no_document"] += 1
            return
        state_of = lambda copied: copied.model_state[KEY]  # noqa: E731
        from sheltersim.core.world import WorldState

        def restored() -> Any:
            copied = WorldState.from_dict(copy.deepcopy(document))
            return copied if rows_kernel(copied) is True else None

        def attempt(label: str, write: Any) -> None:
            copied = restored()
            if copied is None:
                recorder.seen["rows.probe_copy_refused"] += 1
                return
            if not write(state_of(copied)):
                recorder.seen["rows.probe_nothing_to_break"] += 1
                return
            recorder.seen["rows.probes"] += 1
            try:
                accepted = rows_kernel(copied) is True
            except unsupported:
                accepted = False
            if accepted:
                recorder.differ(f"rows_valid took {label}")

        def record_out_of_range(state: Any) -> bool:
            next(iter(state["agents"].values()))["reassurance"] = 1.01
            return True

        def trust_out_of_range(state: Any) -> bool:
            record = next(iter(state["relationships"].values()), None)
            if record is None:
                return False
            record["trust"] = 1.01
            return True

        def unknown_key(state: Any) -> bool:
            state["relationships"]['["nobody","here"]'] = {
                "contact_count": 0,
                "trust": 0.5,
                "contact_minutes": 0,
                "last_contact_minute": None,
                "last_interaction": None,
                "topic_ids": [],
            }
            return True

        attempt("a record 1% beyond its bound", record_out_of_range)
        attempt("a relationship 1% beyond its bound", trust_out_of_range)
        attempt("a key naming nobody", unknown_key)

    def rows_shadow(world: Any) -> bool:
        accepted = rows_kernel(world)
        if probing[0]:
            return accepted
        recorder.seen["rows.calls"] += 1
        loaded = body_state()
        kept = loaded._ROWS_VALID
        loaded._ROWS_VALID = None
        try:
            loaded.validate_body_state(world, validate_event_history=False)
            error = None
        except (ValueError, KeyError, TypeError) as raised:
            error = raised
        finally:
            loaded._ROWS_VALID = kept
        if accepted is not True:
            recorder.seen["rows.refused"] += 1
            if error is None:
                recorder.reasons["rows_valid: said no to what Python passes"] += 1
            return accepted
        recorder.seen["rows.accepted"] += 1
        if error is not None:
            recorder.differ(f"rows_valid took what Python turned down: {error}")
        state = world.model_state[KEY]
        recorder.seen["rows.relationships_read_by_python"] += len(state["relationships"])
        if recorder.seen["rows.accepted"] % PROBE_EVERY == 1:
            probing[0] = True
            try:
                probe(world)
            finally:
                probing[0] = False
        return accepted

    state_module.rows_valid = rows_shadow
