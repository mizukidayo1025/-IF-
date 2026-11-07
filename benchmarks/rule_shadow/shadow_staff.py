"""Compare the kernels of native/src/rules/staff.rs that have a Python rule with it at each call.

A kernel runs on the world, and the Python rule on a twin of the world or on its own copy of it.
The validator is compared on what it accepts: the Python rule must accept that too.
`update_operations`, `assign_operations`, `relief`, `assign_pending` and travel.rs's `route` have
no Python rule left to compare with (lane L10).
"""

from __future__ import annotations

from typing import Any

from shadow_kit import Recorder

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
    # `_route` of travel.rs, with edge capacities or not, has no Python rule left to compare
    # (KERNEL_ONLY_SEGMENTS, lane L10).

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
