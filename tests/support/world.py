"""Parts of WorldState, events and validation for tests of one mechanism."""

from __future__ import annotations

from collections.abc import Callable, Iterator
from contextlib import contextmanager
from typing import Any

import pytest

from support.paths import FAILURE_DAY3


@pytest.fixture
def count_validations(monkeypatch: pytest.MonkeyPatch) -> Callable[[Any], list[int]]:
    """Record the id of every object a pydantic model validates from now on.

    For tests of parsed-config caches; monkeypatch.undo() stops the counting.
    """

    def count(model: Any) -> list[int]:
        calls: list[int] = []
        original = model.model_validate.__func__

        def validate(cls, value, *args, **kwargs):
            calls.append(id(value))
            return original(cls, value, *args, **kwargs)

        monkeypatch.setattr(model, "model_validate", classmethod(validate))
        return calls

    return count


def _stress_delta(agent_id: str, minute: int, stress: float) -> Any:
    from sheltersim.policies.psychology import AgentStateDelta

    return AgentStateDelta(
        agent_id=agent_id,
        minute_index=minute,
        toilet_urgency=0.0,
        hunger=0.0,
        fatigue=0.0,
        stress=stress,
        trust=0.5,
    )


@pytest.fixture
def stress_delta() -> Callable[[str, int, float], Any]:
    """Build a psychology update that sets only stress (needs 0, trust 0.5)."""
    return _stress_delta


def _make_event(
    event_id: str,
    *,
    due_minute: int = 0,
    priority: int = 0,
    sequence: int = 0,
    event_type: str = "record",
    payload: dict[str, object] | None = None,
    kind: Any = None,
) -> Any:
    from sheltersim.events.models import EventKind, ScheduledEvent

    return ScheduledEvent(
        event_id=event_id,
        kind=EventKind.EXOGENOUS if kind is None else kind,
        due_minute=due_minute,
        priority=priority,
        sequence=sequence,
        event_type=event_type,
        payload=payload or {},
    )


@pytest.fixture
def make_event() -> Callable[..., Any]:
    """Build a ScheduledEvent: exogenous "record", due at minute 0, priority and sequence 0."""
    return _make_event


class _RecordHandler:
    def __init__(self) -> None:
        self.applied: list[str] = []

    def apply(self, world: Any, event: Any) -> None:
        self.applied.append(event.event_id)


@pytest.fixture
def record_handler() -> type[_RecordHandler]:
    """Make an event handler that changes nothing and lists the ids it applies, in order."""
    return _RecordHandler


@pytest.fixture
def live_checkpoint_world() -> Any:
    """failure_day3 at minute 0 with every kind of runtime state a Checkpoint must keep.

    Two people request toilet_1 (one served, one queued), A003 asks staff for
    support, the model and metric state hold values, and one event is realized
    while the Scenario's events are still pending.
    """
    from sheltersim.core.rng import CounterRNG
    from sheltersim.core.world import WorldState
    from sheltersim.domain.facilities import ToiletServiceManager
    from sheltersim.domain.staff import StaffService
    from sheltersim.events.handlers import EventDispatcher
    from sheltersim.events.models import EventKind, ScheduledEvent
    from sheltersim.experiments.scenario import ScenarioSpec

    world = WorldState.from_scenario(ScenarioSpec.load(FAILURE_DAY3))
    toilet = ToiletServiceManager(world)
    toilet.request("A001", "toilet_1")
    toilet.request("A002", "toilet_1")
    toilet.arbitrate(0, CounterRNG(world.seed))
    staff = StaffService(world)
    staff.request("A003")
    staff.assign_pending(0)
    world.model_state = {"behavior-model": {"adaptive_state": [0.25, 0.75]}}
    world.metric_state["model_accumulator"] = 12.0

    audit_event = ScheduledEvent(
        event_id="checkpoint-audit",
        kind=EventKind.EXOGENOUS,
        due_minute=0,
        priority=0,
        sequence=world.allocate_event_sequence(),
        event_type="unknown_checkpoint_test_event",
    )
    world.schedule_event(audit_event)
    EventDispatcher().dispatch_due(world, 0)
    world.validate()
    assert any(task.kind == "toilet_service" for task in world.active_tasks.values())
    assert any(task.kind == "toilet_queue" for task in world.active_tasks.values())
    assert any(task.kind == "staff_support" for task in world.active_tasks.values())
    assert world.event_queue.pending()
    assert world.event_log.records
    return world


# The attribute an outcome a kernel made carries (native/src/core/errors.rs).
NATIVE_OUTCOME = "_sheltersim_native_outcome"


def kernel_says_python_s_text(
    handler: Any, world: Any, event: Any, expected: tuple[type, str] | None = None
) -> tuple[str, str]:
    """The handler's kernel raises its Python rule's outcome itself, with the same class and text.

    `handler.apply` is the kernel; the outcome must be the kernel's own (not a refusal) and the
    world must be as it was. The handler has no Python rule left (`kernel_only`), so `expected` is
    the Python rule's outcome, written down from it: (EventSuppressed or EventApplicationError,
    text). The class name and text, for the caller. (docs/rust-port/lanes/l8-texts-events.md)
    """
    from sheltersim.events.models import EventApplicationError, EventSuppressed

    def text(state: Any) -> str:
        return state._canonical_text(state._canonical_data())

    assert expected is not None, "a handler with no Python rule needs the outcome it raised"
    before = text(world)
    with pytest.raises((EventSuppressed, EventApplicationError)) as made:
        handler.apply(world, event)
    assert getattr(made.value, NATIVE_OUTCOME, False), f"the kernel refused: {made.value}"
    assert text(world) == before
    assert (type(made.value), str(made.value)) == expected
    return type(made.value).__name__, str(made.value)


def view_of(world: Any, *, include_state_hash: bool = True) -> Any:
    """The `WorldView` a phase makes of the world, by native/src/rules/world_view.rs.

    With `include_state_hash` it is the propose phase's view (`_view_for_proposals`), with its
    audit hash; without, the update phase's (`_view_after_movement`), whose `state_hash` is None.
    Nothing of an earlier view is kept, and nothing changes. A world the kernel can't read raises
    `native.Unsupported`.
    """
    from types import SimpleNamespace

    from sheltersim import native

    holder = SimpleNamespace(
        _view_before_proposals=None, _rust_view_cache=None, _zone_environments={}
    )
    rules = native.rules("world_view")
    if include_state_hash:
        return rules.view_for_proposals(holder, world, world.minute_index)
    return rules.view_after_movement(holder, world)


def decisions_of(world: Any, policy: Any = None, *, rng: Any = None) -> dict[str, Any]:
    """The `ActionProposal` of each person due at the world's minute, by ID, as the propose phase
    makes them: native/src/rules/world_view.rs's view, then native/src/rules/behavior.rs.

    `policy` is the runner's default (`UtilityBehaviorPolicy()`) when None, `rng` CounterRNG of
    the world's seed. Nothing of an earlier view is kept, and nothing changes.
    """
    from types import SimpleNamespace

    from sheltersim import native
    from sheltersim.core.rng import CounterRNG
    from sheltersim.policies.behavior import BehaviorDecisionRunner

    holder = SimpleNamespace(
        decisions=BehaviorDecisionRunner(policy),
        _view_before_proposals=None,
        _rust_view_cache=None,
        _zone_environments={},
    )
    view = native.rules("world_view").view_for_proposals(holder, world, world.minute_index)
    proposals = native.rules("behavior").propose_due(
        holder, view, CounterRNG(world.seed) if rng is None else rng
    )
    return {proposal.agent_id: proposal for proposal in proposals}


def world_digest(world: Any) -> str:
    """16 hex digits of the world as a step left it: its canonical JSON, counters, pending events,
    metric state, the number of log records and the open tasks' IDs.

    For a step whose Python rule is gone (`kernel_only`): the digest that rule's world had, written
    down from it before it went, against the kernel's world. Floats come from libm, so a test
    compares it on Linux only, as the gate's hashes are pinned.
    """
    import hashlib
    import json

    text = json.dumps(
        [
            world._canonical_text(world._canonical_data()),
            world.id_counters,
            [repr(event) for event in world.event_queue.pending()],
            world.metric_state,
            len(world.event_log.records),
            list(world.active_tasks),
        ],
        sort_keys=True,
    )
    return hashlib.sha256(text.encode("utf-8")).hexdigest()[:16]


@contextmanager
def on_the_phases() -> Iterator[None]:
    """Every tick through `phase_handler`, the phases' Python steps, instead of the driver.

    The controller's metrics step is patched with one that calls it unchanged: the driver
    (native/src/rules/tick.rs) then refuses each tick before anything changes, as it does for any
    patch of a step. The steps are the same kernels either way.
    """
    from sheltersim.experiments.runner import OfflineSimulationController

    step = OfflineSimulationController._run_metrics_phase

    def same_step(self: Any, *arguments: Any, **options: Any) -> Any:
        return step(self, *arguments, **options)

    with pytest.MonkeyPatch.context() as patch:
        patch.setattr(OfflineSimulationController, "_run_metrics_phase", same_step)
        yield
