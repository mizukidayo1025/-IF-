"""Compare the rest of events/handlers.py's kernels with their Python rules at each call.

Each call runs the Python rule on one copy of the world and the kernel on another, and compares
their outcomes and canonical documents as JSON text, in which a float keeps its bits and 1 differs
from 1.0. Every row must write the bytes of its tree on the kernel's copy. Then the kernel runs on
the real world.

A refusal of what the Python rule applied is a difference: the world's validation can turn down a
wrong result, and the Python rule then quietly gets it right, which no hash would ever see.

Here: which requests `_consider_due_complaints` asks the policy about.

The handlers of rules/handlers.rs, information_events.rs, staff_operation_events.rs,
social_interaction.rs and travel_completion.rs, `_reject_room_approach`,
`interrupt_staff_trips_on_closed_routes`, `StaffService.assign_pending` and the receipts' `ingest`
have no Python rule left to compare (`kernel_only`, lane L10).
"""

from __future__ import annotations

import copy
import json
from types import SimpleNamespace
from typing import Any

from shadow_effects import report_stats
from shadow_kit import Recorder, text, values_in


def _outcome(call: Any) -> tuple[str, Any]:
    from sheltersim.events.models import EventApplicationError, EventSuppressed

    try:
        result = call()
    except EventSuppressed as raised:
        return "suppressed", str(raised)
    except EventApplicationError as raised:
        return "failed", str(raised)
    except Exception as raised:  # noqa: BLE001 - only compared with what the kernel does
        return type(raised).__name__, str(raised)
    if isinstance(result, tuple):
        return "applied", [getattr(item, "to_dict", lambda item=item: item)() for item in result]
    return "applied", result


def _worlds(world: Any) -> tuple[Any, Any]:
    """Two copies. A world in the middle of a phase need not pass `to_dict`'s validation."""
    from sheltersim.core.world import StateInvariantError, WorldState

    try:
        document = world.to_dict()
    except StateInvariantError:
        return copy.deepcopy(world), copy.deepcopy(world)
    return WorldState.from_dict(document), WorldState.from_dict(document)


def compare_worlds(recorder: Recorder, label: str, python_world: Any, kernel_world: Any) -> None:
    # Without the world's validation: a world in the middle of a phase need not pass it.
    python_after = json.loads(python_world._canonical_text(python_world._canonical_data()))
    kernel_after = json.loads(kernel_world._canonical_text(kernel_world._canonical_data()))
    if text(python_after) != text(kernel_after):
        for key, value in python_after.items():
            if text(value) != text(kernel_after.get(key)):
                recorder.differ(f"{label}: {key} differs")
    else:
        recorder.seen[f"{label}.values_compared"] += values_in(python_after)
    for name, mirror in kernel_world.model_state._mirrors.items():
        if name in kernel_world.model_state and not mirror.verify():
            recorder.differ(f"{label}: the row of {name} is not its tree")


def prepare(output: str) -> None:
    from sheltersim import native

    recorder = Recorder(output, "handlers_rest")
    report_stats(recorder, native.rules("handlers"))
    if not _comparing():
        return
    # The driver calls its kernels directly, past these stand-ins: alone, this shadow would see no
    # tick and report nothing while every hash passes.
    native.rules("tick").python_path_only(True)

    # `care_receipts.ingest` has no Python rule left to compare (`kernel_only`, lane L10).

    staff = native.rules("staff")
    complaints_kernel = staff.consider_due_complaints

    def consider_due_complaints(controller: Any, minute: Any) -> Any:
        from sheltersim.experiments.phases.arbitrate import ArbitratePhase

        policy = controller.social_policy
        if policy is None:
            return complaints_kernel(controller, minute)
        recorder.seen["complaints.calls"] += 1
        expected: list[tuple[str, str]] = []

        def note(_world: Any, *, agent_id: str, request_id: str) -> None:
            expected.append((agent_id, request_id))

        ArbitratePhase._consider_due_complaints(
            SimpleNamespace(
                world=controller.world,
                social_policy=SimpleNamespace(
                    limits=policy.limits, consider_complaint_response=note
                ),
            ),
            minute,
        )
        actual: list[tuple[str, str]] = []

        def forward(world: Any, *, agent_id: str, request_id: str) -> Any:
            actual.append((agent_id, request_id))
            return policy.consider_complaint_response(
                world, agent_id=agent_id, request_id=request_id
            )

        result = complaints_kernel(
            SimpleNamespace(
                world=controller.world,
                social_policy=SimpleNamespace(
                    limits=policy.limits, consider_complaint_response=forward
                ),
            ),
            minute,
        )
        recorder.seen["complaints.kernel_ran"] += 1
        recorder.seen["complaints.asked"] += len(actual)
        # A decision may settle a later request of the minute, which the note above cannot see.
        if actual != expected[: len(actual)] or any(pair not in expected for pair in actual):
            recorder.differ(f"complaints at {minute}: {actual!r} and {expected!r}")
        return result

    staff.consider_due_complaints = consider_due_complaints


def _comparing() -> bool:
    import os

    return os.environ.get("SHELTERSIM_DISPATCH_COUNT_ONLY") != "1"
