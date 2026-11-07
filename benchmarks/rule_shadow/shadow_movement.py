"""Compare the kernels of native/src/rules/movement.rs with their Python rules at each call.

`advance_movement` is compared. The other rules of domain/movement.py have no Python rule left to
compare (`kernel_only`).

A writer's kernel runs on the world first, on a plain copy taken before it, and the Python rule then
runs on that copy with the world's other parts: it must give the same document and result. Key order
inside a record isn't compared, since a kernel leaves a record alone when its bits are the same and
a restored one has the JSON's order; the order of the maps is.

A queue trip's replan writes the event queue, the log, the people and the metrics, so the copy here
can't run it. Such a walk is counted (`advance.in_order_replans`), not compared here, and
tests/integration/test_movement_walk_order.py compares it whole. Neither the gate nor the 7-day run
has reached one.

The validator is compared on what it accepts: the Python rule must accept that too. What it refuses
goes to the Python rule anyway.
"""

from __future__ import annotations

import json
from typing import Any

from shadow_kit import Recorder, values_in

KEY = "movement_v1"
ROW_CHECK_EVERY = 25
MAPS = ("positions", "staff_positions", "journeys", "edge_usage_minutes")


class _ReadOnlyQueue:
    def __init__(self, queue: Any) -> None:
        self.pending = queue.pending


class _Twin:
    """The world for a Python rule: a plain copy of the namespace and a queue it can't write."""

    def __init__(self, world: Any, plain: dict[str, Any]) -> None:
        table = dict(world.model_state)
        table[KEY] = plain
        self.__dict__.update(model_state=table, _world=world)
        self.__dict__["event_queue"] = _ReadOnlyQueue(world.event_queue)

    def __getattr__(self, name: str) -> Any:
        return getattr(self.__dict__["_world"], name)


def _document(state: dict[str, Any]) -> str:
    return json.dumps(state, sort_keys=True, ensure_ascii=False)


def _order(state: dict[str, Any]) -> list[list[str]]:
    return [list(state[name]) for name in MAPS]


def _count_branches(recorder: Recorder, state: dict[str, Any]) -> None:
    """Which journeys a tick starts with: what the kernel had to walk."""
    seen = recorder.seen
    for journey in state["journeys"].values():
        seen["advance.journeys_in_rooms"] += bool(journey["route_points"])
        seen["advance.journeys_on_edges"] += bool(journey["route_edge_ids"])
        seen["advance.journeys_of_queues"] += "queue_id" in journey
        seen["advance.journeys_of_support"] += "support_request_id" in journey


def prepare(output: str) -> None:
    from sheltersim import native

    rules = native.rules("movement")
    recorder = Recorder(output, "movement")
    unsupported = native.Unsupported
    module: list[Any] = []

    def domain() -> Any:
        if not module:
            from sheltersim.domain import movement

            module.append(movement)
        return module[0]

    def row_check(world: Any, label: str) -> None:
        recorder.seen["row_checks.due"] += 1
        if recorder.seen["row_checks.due"] % ROW_CHECK_EVERY:
            return
        mirror = world._core.mirror(KEY)
        if not mirror.verify():
            recorder.differ(f"{label}: the row and the tree")
        if mirror.row_json() != native.canonical_json(world.model_state[KEY]):
            recorder.differ(f"{label}: the row's bytes and the tree's")
        recorder.seen["row_checks.made"] += 1

    def writer(name: str, kernel: Any) -> Any:
        def run(world: Any, *arguments: Any, **options: Any) -> Any:
            if isinstance(world, _Twin):
                raise unsupported("a shadow's copy")
            recorder.seen[f"{name}.calls"] += 1
            state = world.model_state.get(KEY)
            plain = None if state is None else json.loads(json.dumps(state))
            sequence = world.id_counters.get("event", 0)
            pending = len(world.event_queue.pending()) if name == "advance_movement" else 0
            try:
                result = kernel(world, *arguments, **options)
            except unsupported as reason:
                recorder.reasons[f"{name}: {reason}"] += 1
                raise
            recorder.seen[f"{name}.kernel_ran"] += 1
            if plain is None:
                return result
            if name == "advance_movement" and (
                len(world.event_queue.pending()) != pending
                or world.id_counters.get("event", 0) != sequence
            ):
                recorder.seen["advance.in_order_replans"] += 1
                return result
            where = f"{name} {arguments!r:.100} at {world.minute_index}"
            before = _document(plain)
            if name == "advance_movement":
                _count_branches(recorder, plain)
                recorder.seen["advance.replans_returned"] += len(result)
            try:
                expected = getattr(domain(), name).__wrapped__(
                    _Twin(world, plain), *arguments, **options
                )
            except Exception as raised:  # noqa: BLE001 - only compared with what the kernel did
                recorder.differ(
                    f"{where}: the kernel took what Python turned down: {raised!r:.300}"
                )
                return result
            now = world.model_state[KEY]
            if result != expected:
                recorder.differ(f"{where}: returned {result!r:.200}, not {expected!r:.200}")
            mine, theirs = _document(now), _document(plain)
            if mine != theirs:
                at = next((i for i, pair in enumerate(zip(mine, theirs)) if pair[0] != pair[1]), 0)
                start, end = max(0, at - 150), at + 150
                recorder.differ(f"{where}: ...{mine[start:end]} is not ...{theirs[start:end]}")
            if _order(now) != _order(plain):
                recorder.differ(f"{where}: the order of a map")
            recorder.seen[f"{name}.values_compared"] += values_in(plain)
            recorder.seen[f"{name}.changed"] += before != theirs
            row_check(world, where)
            return result

        return run

    rules.advance_movement = writer("advance_movement", rules.advance_movement)

    validate_kernel = rules.validate

    def validate(world: Any) -> bool:
        recorder.seen["validate.calls"] += 1
        try:
            accepted = validate_kernel(world)
        except unsupported as reason:
            recorder.reasons[f"validate: {reason}"] += 1
            raise
        recorder.seen[f"validate.{'accepted' if accepted else 'refused'}"] += 1
        if accepted:
            try:
                domain().validate_movement_runtime_state(world)
            except Exception as raised:  # noqa: BLE001 - only compared with what the kernel did
                recorder.differ(
                    f"validate: the kernel took what Python turned down: {raised!r:.300}"
                )
        return accepted

    rules.validate = validate
