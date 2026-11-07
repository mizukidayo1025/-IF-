"""Compare the kernels of native/src/rules/staff.rs that have a Python rule with it at each call.

A kernel runs on the world, and the Python rule on a twin of the world or on its own copy of it.
The validator is compared on what it accepts: the Python rule must accept that too.
`update_operations`, `assign_operations`, `relief` and `assign_pending` have no Python rule left to
compare with (lane L10, `kernel_only`).
"""

from __future__ import annotations

from typing import Any

from shadow_handlers_rest import _outcome, _worlds, compare_worlds
from shadow_kit import Recorder, text

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
    recorder = Recorder(output, "staff")
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

    # `wait_snapshot` (experiments/metrics.py's `_support_wait_snapshot`) and the minute's
    # `snapshot_fields` around it have no Python rule left to compare (`kernel_only`, lane L10).

    # domain/staff_operations.py registers `validate` when it is imported, later than this.
    state_rules = native.state("staff_operations")
    rust_validate = state_rules.validate

    def validator(world: Any) -> bool:
        accepted = rust_validate(world)
        recorder.seen["validator_accepted" if accepted else "validator_refused"] += 1
        try:
            from sheltersim.domain import staff_operations

            staff_operations.validate_staff_operations_state(world)
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


def _prepare_common(native: Any, recorder: Recorder) -> None:
    state_rules = native.state("shared_operations")
    tick_kernel = state_rules.validate_tick
    module: list[Any] = []

    def common() -> Any:
        if not module:
            from sheltersim.domain import common_operations

            module.append(common_operations)
        return module[0]

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

    # `record_shared_operation` has no Python rule left to compare (`kernel_only`, lane L10).
    state_rules.validate_tick = validate_tick
