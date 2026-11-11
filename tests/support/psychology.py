"""One minute of each person's psychology, as the update phase runs it.

The steps are native/core/src/rules/psychology.rs and rules/trace.rs alone: the Python rules of
policies/psychology.py are gone (lane L10). `psychology_step` runs the kernel of
`_psychology_update` on a world, with the view a tick makes, and with a trace, the kernel of
`_record_psychology_trace`; the test reads the deltas and the document the Run's sink is given.
Nothing is written to the world: `apply_deltas` writes the deltas as `_apply_state_deltas` does.
"""

from __future__ import annotations

import copy
from dataclasses import dataclass
from types import SimpleNamespace
from typing import Any

from sheltersim import native
from sheltersim.core.engine import TraceLevel
from sheltersim.core.world import WorldState


class TraceKeeper:
    """A Run's sink of the psychology trace: the documents and contributions it is given."""

    def __init__(self, level: TraceLevel = TraceLevel.FULL) -> None:
        self.trace_level = level
        self.documents: list[dict[str, Any]] = []
        self.contributions: list[list[dict[str, Any]]] = []

    def record_psychology_trace(self, document: Any, *, agent_contributions: Any = ()) -> None:
        self.documents.append(copy.deepcopy(document))
        self.contributions.append(copy.deepcopy(list(agent_contributions)))


@dataclass(frozen=True)
class PsychologyStep:
    """What the step gave: the deltas in ID order, and with a trace the minute's document (with the
    model's `policy_version`) and `agent_contributions`."""

    deltas: tuple[Any, ...]
    document: dict[str, Any] | None = None
    contributions: list[dict[str, Any]] | None = None

    def delta(self, agent_id: str) -> Any:
        return next(delta for delta in self.deltas if delta.agent_id == agent_id)

    def person(self, agent_id: str) -> dict[str, Any]:
        """A person's part of the document."""
        assert self.document is not None
        return next(item for item in self.document["agents"] if item["agent_id"] == agent_id)

    def contribution(self, agent_id: str) -> dict[str, Any]:
        """A person's `agent_contributions` item, whose components are the model's own."""
        assert self.contributions is not None
        return next(item for item in self.contributions if item["agent_id"] == agent_id)


def controller_for(world: WorldState, policy: Any, sink: Any = None) -> SimpleNamespace:
    """What the kernels read of the controller: the world, the model and the sink. Without a Run's
    core, the view keeps nothing from one view to the next."""
    return SimpleNamespace(
        world=world,
        psychology=policy,
        artifacts=sink if sink is not None else TraceKeeper(TraceLevel.OFF),
    )


def view_of(world: WorldState) -> Any:
    """The view `_view_after_movement` makes of the world."""
    return native.rules("world_view").view_after_movement(controller_for(world, None), world)


def psychology_step(
    world: WorldState,
    policy: Any,
    *,
    trace_level: TraceLevel = TraceLevel.OFF,
    view: Any = None,
    final_deltas: Any = None,
) -> PsychologyStep:
    """`_psychology_update` at the world's minute, then with a trace `_record_psychology_trace`
    with `final_deltas` (by default the deltas themselves, as without a social model)."""
    sink = TraceKeeper(trace_level)
    controller = controller_for(world, policy, sink)
    if view is None:
        view = view_of(world)
    minute = world.minute_index
    result = native.rules("psychology").update_psychology(controller, view, minute)
    if trace_level is TraceLevel.OFF:
        return PsychologyStep(result.deltas)
    final = result.deltas if final_deltas is None else final_deltas
    native.rules("trace").record_psychology_trace(controller, result, final, minute)
    (document,) = sink.documents
    (contributions,) = sink.contributions
    return PsychologyStep(result.deltas, document, contributions)


def apply_deltas(world: WorldState, deltas: Any) -> None:
    """`_apply_state_deltas`: write the deltas to the people and their body records."""
    native.rules("psychology").apply_deltas(None, world, tuple(deltas))
