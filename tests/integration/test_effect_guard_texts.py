"""An EffectProgram marker's guard in the Rust dispatch loop, through the phase's Python entry.

`EventDispatcher.dispatch_one` asked the guard (`should_apply_effect_guard`) before an event's
handler. The loop in native/core/src/rules/dispatch.rs asks it in Rust, for every handler (lane
P9-1c): a false guard is a suppressed event, with the Python rule's text (saved as the record's
`detail`). The guard's cases (a guard the contract turns down, one without its target, one that
holds, an int past 64 bits) are the core's tests (native/core/tests/effect_guard_dispatch.rs,
Issue #225), on the same world, with the same digests of what the step saved. Here one event goes
through the controller's `_run_dispatch_phase`: what it saved is the digest taken once from the
Python step (45a9bc9), pinned on every OS, as the gate's hashes are.
"""

from __future__ import annotations

import hashlib
import json
import sys
from collections import Counter
from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest

from sheltersim import native
from sheltersim.core.world import WorldState
from sheltersim.events.models import EventKind, EventStatus, ScheduledEvent
from sheltersim.experiments.runner import OfflineSimulationController

KEY = "effect_program_v1"


class Sink:
    """What the controller writes to, which these tests don't read."""

    def __getattr__(self, name: str) -> Callable[..., None]:
        return lambda *arguments, **options: None


def _gate() -> Any:
    root = str(Path(__file__).resolve().parents[2] / "benchmarks")
    if root not in sys.path:
        sys.path.insert(0, root)
    import gate_scenarios

    return gate_scenarios


@pytest.fixture(scope="module")
def scenario() -> Any:
    return _gate().generated(evacuees=20, minutes=120)


def _counts() -> Counter[str]:
    """The dispatch's counts, with the guard's refusals (`effects.stats`) as `guard_refused`."""
    counts = Counter(native.rules("dispatch").stats()[0])
    reasons = native.rules("effects").stats()[1]
    counts["guard_refused"] = sum(
        times for reason, times in reasons.items() if reason.startswith("dispatch_guard:")
    )
    return counts


def _saved(controller: OfflineSimulationController) -> dict[str, Any]:
    world = controller.world
    return {
        "records": [item.decode() for item in world.event_log.items_bytes(0, len(world.event_log))],
        "pending": [event.to_dict() for event in world.event_queue.pending()],
        "counters": dict(world.id_counters),
        # Not `canonical_json()`: its check turns down a marker of no scheduled program.
        "world": world._canonical_text(world._canonical_data()),
    }


def _marked(event_type: str, payload: dict[str, Any], guard: Any) -> ScheduledEvent:
    return ScheduledEvent(
        event_id="guarded-1",
        kind=EventKind.INTERVENTION,
        due_minute=0,
        priority=0,
        sequence=900_001,
        event_type=event_type,
        payload=payload,
        source_id="TEST-GUARD",
        causation_id=None,
        provenance={KEY: {"guard": guard, "program_id": "no-such-program"}},
    )


def _dispatched(
    scenario: Any, event: ScheduledEvent, python_saved: str
) -> tuple[Any, Counter[str], Counter[str]]:
    """The event's record from the Rust loop, which saves what the Python step saved: the digest
    `python_saved` of `_saved`. A crafted marker of no program makes a world `validate()` turns
    down, which neither the guard nor the dispatch loop reads."""
    controller = OfflineSimulationController(WorldState.from_scenario(scenario), Sink())
    controller.world.schedule_event(event)
    before = _counts()
    controller._run_dispatch_phase(controller.world, 0)
    after = _counts()
    text = json.dumps(_saved(controller), sort_keys=True)
    assert hashlib.sha256(text.encode("utf-8")).hexdigest()[:16] == python_saved
    record = controller.world.event_log.find(event.event_id)
    return record, before, after


def _grew(before: Counter[str], after: Counter[str], *names: str) -> None:
    for name in names:
        assert after[name] == before[name] + 1, name


FACILITY = {"facility_id": "toilet_01", "working_units": 0}


def test_a_false_guard_suppresses_the_event_in_rust(scenario: Any) -> None:
    guard = {"operator": "lt", "field": "simulation.minute", "value": 0}
    record, before, after = _dispatched(
        scenario, _marked("effect_program_apply", FACILITY, guard), "4e2ff7aa6f0fc988"
    )
    assert (record.status, record.detail) == (
        EventStatus.SUPPRESSED,
        "EffectProgram guard evaluated false",
    )
    _grew(before, after, "guard_stopped")
    assert after["events_foreign"] == before["events_foreign"]
