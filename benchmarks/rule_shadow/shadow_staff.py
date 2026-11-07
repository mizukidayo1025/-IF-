"""Compare the kernels of native/src/rules/staff.rs that have a Python rule with it at each call.

A kernel runs on the world, and the Python rule on a twin of the world or on its own copy of it.
The validator is compared on what it accepts: the Python rule must accept that too.
`update_operations`, `assign_operations`, `relief` and `assign_pending` have no Python rule left to
compare with (lane L10, `kernel_only`).
"""

from __future__ import annotations

import json
from typing import Any

from shadow_handlers_rest import _outcome, _worlds, compare_worlds
from shadow_kit import Recorder, text, values_in

KEY = "staff_operations_v1"
ROW_CHECK_EVERY = 25


class _WouldStart(Exception):
    """The Python rule reached what only a rule that starts something reaches."""


class _Twin:
    """The world for a Python rule: its other parts are the world's, and it can't start anything."""

    def __init__(self, world: Any, **parts: Any) -> None:
        self.__dict__.update(parts, _world=world)

    def __getattr__(self, name: str) -> Any:
        return getattr(self.__dict__["_world"], name)

    def allocate_event_sequence(self) -> int:
        raise _WouldStart

    def schedule_event(self, _event: Any) -> None:
        raise _WouldStart


def prepare(output: str) -> None:
    from sheltersim import native

    # The driver calls these kernels without going through the module.
    native.rules("tick").python_path_only(True)
    rules = native.rules("staff")
    recorder = Recorder(output, "staff")
    unsupported = native.Unsupported
    module: dict[str, Any] = {}

    def operations() -> Any:
        if "operations" not in module:
            from sheltersim.domain import staff_operations as loaded

            module["operations"] = loaded
        return module["operations"]

    snapshot_kernel = rules.wait_snapshot

    travel = native.rules("travel")
    route_kernel = travel.route

    def route(controller: Any, origin: Any, destination: Any, agent_id: Any) -> Any:
        if not controller.route_capacities:
            return route_kernel(controller, origin, destination, agent_id)
        recorder.seen["route_capacity.calls"] += 1
        import copy
        import dataclasses

        from sheltersim.experiments.phases.propose import ProposePhase

        python_world, kernel_world = _worlds(controller.world)

        def on(world: Any) -> Any:
            copied = copy.copy(controller)
            copied.world = world
            return copied

        def plain(found: Any) -> Any:
            return None if found is None else dataclasses.asdict(found)

        wanted = _outcome(
            lambda: plain(ProposePhase._route(on(python_world), origin, destination, agent_id))
        )
        actual = _outcome(
            lambda: plain(route_kernel(on(kernel_world), origin, destination, agent_id))
        )
        if actual[0] == "Unsupported":
            recorder.reasons[f"route_capacity: {actual[1]}"] += 1
        elif text(actual) != text(wanted):
            recorder.differ(f"route_capacity: {actual!r:.300} and {wanted!r:.300}")
        else:
            compare_worlds(recorder, "route_capacity", python_world, kernel_world)
            recorder.seen["route_capacity.none"] += actual[1] is None
        return route_kernel(controller, origin, destination, agent_id)

    travel.route = route

    def wait_snapshot(world: Any, active_support: Any, sample_minute: Any) -> Any:
        recorder.seen["snapshot.calls"] += 1
        try:
            result = snapshot_kernel(world, active_support, sample_minute)
        except unsupported as reason:
            recorder.reasons[f"snapshot: {reason}"] += 1
            raise
        recorder.seen["snapshot.kernel_ran"] += 1
        from sheltersim.experiments import metrics

        expected = metrics._support_wait_snapshot.__wrapped__(world, active_support, sample_minute)
        if text(expected) != text(result):
            recorder.differ(
                f"snapshot at {sample_minute}: {text(result)[:300]} and {text(expected)[:300]}"
            )
        else:
            recorder.seen["snapshot.values_compared"] += values_in(expected[0]) + len(expected[1])
            for detail in expected[0].values():
                for reason_code in detail["reason_codes"]:
                    recorder.seen[f"snapshot.{reason_code}"] += 1
            recorder.seen["snapshot.no_qualified_people"] += len(expected[1])
        return result

    rules.wait_snapshot = wait_snapshot

    # A minute's metrics read the same waits inside `snapshot_fields`, past this module.
    metric_rules = native.rules("metrics")
    fields_kernel = metric_rules.snapshot_fields

    def snapshot_fields(world: Any, unsettled: Any, sample_minute: Any) -> Any:
        active = [item for item in unsettled if item.status.value in ("pending", "assigned")]
        if active:
            wait_snapshot(world, active, sample_minute)
        return fields_kernel(world, unsettled, sample_minute)

    metric_rules.snapshot_fields = snapshot_fields

    # domain/staff_operations.py registers `validate` when it is imported, later than this.
    state_rules = native.state("staff_operations")
    rust_validate = state_rules.validate

    def validator(world: Any) -> bool:
        accepted = rust_validate(world)
        recorder.seen["validator_accepted" if accepted else "validator_refused"] += 1
        try:
            operations().validate_staff_operations_state(world)
        except ValueError as error:
            if accepted:
                recorder.differ(f"the validator took what Python turned down: {error}")
        else:
            if not accepted:
                recorder.seen["validator_refused_what_python_passes"] += 1
        return accepted

    state_rules.validate = validator
    _prepare_common(native, Recorder(output, "common"))


SHARED = "shared_operations_v1"


def _comparable(state: dict[str, Any], tail: int) -> dict[str, Any]:
    """The namespace with the last items of its history, in plain containers."""
    history = state["history"]
    return {
        **json.loads(json.dumps({key: value for key, value in state.items() if key != "history"})),
        "history": json.loads(json.dumps(history[-tail:] if tail else [])),
    }


def _prepare_common(native: Any, recorder: Recorder) -> None:
    rules = native.rules("common_operations")
    state_rules = native.state("shared_operations")
    unsupported = native.Unsupported
    record_kernel = rules.record_shared_operation
    tick_kernel = state_rules.validate_tick
    module: list[Any] = []

    def common() -> Any:
        if not module:
            from sheltersim.domain import common_operations

            module.append(common_operations)
        return module[0]

    def record_shared_operation(world: Any, record: Any) -> None:
        recorder.seen["record.calls"] += 1
        state = world.model_state.get(SHARED)
        if not isinstance(state, dict) or not isinstance(state.get("history"), list):
            recorder.seen["record.no_namespace"] += 1
            record_kernel(world, record)
            return
        # The Python rule reads the recent records only, unless the list is at its limit.
        size = len(state["history"])
        tail = size if size >= 19_999 else min(size, 64)
        before = _comparable(state, tail)
        try:
            record_kernel(world, record)
        except unsupported as reason:
            recorder.reasons[f"record: {reason}"] += 1
            raise
        recorder.seen["record.kernel_ran"] += 1
        recorder.seen[f"record.{record.status}"] += 1
        recorder.seen[f"record.{type(record).__name__}"] += 1
        recorder.seen["record.with_state_changes"] += bool(getattr(record, "state_changes", ()))
        table = dict(world.model_state)
        table[SHARED] = before
        try:
            common().record_shared_operation.__wrapped__(_Twin(world, model_state=table), record)
        except ValueError as error:
            recorder.differ(f"record: the kernel took what Python turned down: {error}")
            return
        state = world.model_state[SHARED]
        after = _comparable(state, len(before["history"]))
        if text(before) != text(after):
            recorder.differ(
                f"record of {record.event_id}: {text(before)[-400:]} and {text(after)[-400:]}"
            )
        recorder.seen["record.values_compared"] += values_in(before)
        if (
            recorder.seen["record.calls"] % ROW_CHECK_EVERY == 0
            and not world._core.mirror(SHARED).verify()
        ):
            recorder.differ(f"record of {record.event_id}: the row and the tree")

    inside = [False]

    def validate_tick(world: Any) -> bool:
        if inside[0]:
            return False
        accepted = tick_kernel(world)
        recorder.seen["tick_validator_accepted" if accepted else "tick_validator_refused"] += 1
        inside[0] = True
        try:
            common().validate_shared_operations_state(world, validate_event_history=False)
        except ValueError as error:
            if accepted:
                recorder.differ(f"the tick validator took what Python turned down: {error}")
        else:
            if not accepted:
                recorder.seen["tick_validator_refused_what_python_passes"] += 1
        finally:
            inside[0] = False
        return accepted

    rules.record_shared_operation = record_shared_operation
    state_rules.validate_tick = validate_tick
