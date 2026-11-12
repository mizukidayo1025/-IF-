"""The views of rules/world_view.rs through the phases' Python entries: made when a reader asks.

The phases' steps `_view_after_movement` and `_view_for_proposals` make them, and have no Python
body (lane L10): what the kernel refuses is a KernelRefusal. The Python rule `WorldView.from_world`
went too. What the views read of a world, every field of the views of whole Runs, and the audit
hash of each form of the document are the core's tests (native/core/tests/world_view_reads.rs and
world_view_run.rs, Issue #225). Here are the entries, the Python objects they make lazily, the
values a Python write leaves that a typed field cannot hold, and the mark of the Python rules.
"""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import replace
from typing import Any

import pytest
from support.world import driven_ticks, view_of

from sheltersim import native
from sheltersim.core.engine import TickEngine, TickPhase
from sheltersim.core.kernels import KernelRefusal
from sheltersim.core.world import WorldState
from sheltersim.domain.body_state import create_body_state
from sheltersim.experiments.metrics import MetricsRecorder
from sheltersim.experiments.phases.propose import ProposePhase
from sheltersim.experiments.phases.update import UpdatePhase
from sheltersim.experiments.runner import OfflineSimulationController
from sheltersim.experiments.timeline import _MetricSink
from sheltersim.policies.world_view import WorldView


def _exact(value: Any) -> Any:
    """Equal only with the same types, the same bits, and the same order of each dict's keys."""
    if value is None or type(value) is bool:
        return value
    if type(value) is float:
        return value.hex()
    if type(value) is int:
        return ["int", value]
    if isinstance(value, str):
        return [type(value).__name__, str(value)]
    if isinstance(value, tuple):
        return ["tuple", *(_exact(item) for item in value)]
    if isinstance(value, Mapping):
        return ["map", *([key, _exact(item)] for key, item in value.items())]
    if hasattr(value, "__dataclass_fields__"):
        return [type(value).__name__, *([k, _exact(v)] for k, v in value.__dict__.items())]
    raise TypeError(type(value))


_PARTS = ("agents", "facilities", "zone_members", "group_members")


def _fields(view: WorldView) -> list[Any]:
    """Everything a reader of the view gets, each value by its type and bits."""
    return [
        view.minute_index,
        view.state_hash,
        view.resource_service_enabled,
        view.autonomy_enabled,
        view._has_population_context,
        *(_exact(getattr(view, name)) for name in _PARTS),
        _exact(view.immediate_needs()),
    ]


def _rules() -> Any:
    return native.rules("world_view")


def _controller(world: WorldState) -> OfflineSimulationController:
    return OfflineSimulationController(world, _MetricSink(MetricsRecorder()))


def _world_with_views(scenario: Any) -> tuple[WorldState, OfflineSimulationController]:
    world = WorldState.from_scenario(scenario)
    world.model_state.setdefault("body_state_v1", create_body_state(world))
    return world, _controller(world)


def test_nothing_python_is_made_until_a_reader_asks(generated_scenario: Any) -> None:
    world, controller = _world_with_views(generated_scenario)
    made = _rules().view_after_movement(controller, world)
    assert type(made) is WorldView
    assert made.minute_index == world.minute_index
    assert not {"agents", "facilities", "zone_members", "group_members"} & made.__dict__.keys()
    assert made.state_hash is None
    agents = made.agents
    assert made.__dict__["agents"] is agents
    assert made.agents is agents
    with pytest.raises(AttributeError, match="no attribute 'nothing'"):
        made.nothing  # noqa: B018
    # What a Python rule does to a view works on one, and what it makes is a plain view.
    moved = replace(made, minute_index=7)
    assert moved.minute_index == 7 and moved.agents.keys() == agents.keys()
    assert "_rust_view" not in moved.__dict__


def test_a_tick_with_the_kernels_makes_no_python_view_in_the_update_phase(
    generated_scenario: Any,
) -> None:
    """The prior view is Rust's, so Python didn't make it, and no rule has asked for its people."""
    world = WorldState.from_scenario(generated_scenario)
    controller = _controller(world)
    engine = TickEngine(world, phase_handler=controller)
    seen: list[set[str]] = []

    def before_propose(phase: TickPhase, before: bool) -> None:
        if phase is TickPhase.PROPOSE_ACTIONS and before:
            seen.append(set(controller._view_before_proposals.__dict__))

    engine.phase_observer = before_propose
    ticks = driven_ticks()
    for _ in range(3):
        engine.step()
    assert driven_ticks() - ticks == 3
    assert len(seen) == 3
    # The first tick's social step is Python's: its namespace doesn't exist yet.
    for fields in seen[1:]:
        assert "_rust_view" in fields and "agents" not in fields


def test_a_snapshot_stays_as_it_was_read(generated_scenario: Any) -> None:
    world, controller = _world_with_views(generated_scenario)
    made = _rules().view_after_movement(controller, world)
    expected = _fields(view_of(world, include_state_hash=False))
    agent = next(iter(world.agents.values()))
    agent.stress = 0.5 if agent.stress != 0.5 else 0.25
    world.model_state["body_state_v1"]["agents"][agent.id]["uncertainty"] = 0.625
    assert "agents" not in made.__dict__
    assert _fields(made) == expected


def test_a_python_prior_of_a_proposal_view_is_a_kernel_refusal(generated_scenario: Any) -> None:
    world, controller = _world_with_views(generated_scenario)
    # A view Python made: a copy of the kernel's, without its snapshot.
    prior = replace(view_of(world, include_state_hash=False))
    assert "_rust_view" not in prior.__dict__
    controller._view_before_proposals = prior
    with pytest.raises(KernelRefusal, match="prior view"):
        controller._view_for_proposals(world, world.minute_index)
    assert controller._view_before_proposals is prior


@pytest.mark.parametrize(
    ("change", "reason"),
    [
        (
            lambda world: world.model_state["body_state_v1"]["agents"]["A001"].__setitem__(
                "uncertainty", 0
            ),
            "not a float",
        ),
        (
            lambda world: world.model_state["body_state_v1"]["agents"].__setitem__("A001", 1),
            "not a dict",
        ),
        (lambda world: setattr(next(iter(world.agents.values())), "stress", 0), "tainted"),
        (
            lambda world: setattr(next(iter(world.agents.values())), "zone_id", "nowhere"),
            "unknown zone",
        ),
        (
            lambda world: world.model_state.__setitem__(
                "population_presence_v1", {"people": {"A001": {"household_id": 7}}}
            ),
            "not a str",
        ),
    ],
)
def test_what_a_typed_field_cannot_hold_is_a_kernel_refusal(
    generated_scenario: Any, change: Any, reason: str
) -> None:
    """Values no Run's validated world holds: the step refuses them, before anything is made."""
    world, controller = _world_with_views(generated_scenario)
    change(world)
    with pytest.raises(native.Unsupported, match=reason):
        _rules().view_after_movement(controller, world)
    with pytest.raises(KernelRefusal, match=reason):
        controller._view_after_movement(world)


def test_a_context_that_does_not_parse_is_a_kernel_refusal(generated_scenario: Any) -> None:
    """A Scenario's contexts are validated when it is made; the view doesn't raise the contract's
    own error, as the Python rule did."""
    world, controller = _world_with_views(generated_scenario)
    world.model_configs["population_context"] = {"people": {"A001": {"pet_count": -1}}}
    with pytest.raises(KernelRefusal, match="does not parse"):
        controller._view_after_movement(world)


def test_the_proposal_view_has_the_audit_hash_of_the_python_rule(generated_scenario: Any) -> None:
    """The step's entry reaches the kernel: the hash the Python rule gave for the generated world
    (each form of the document is native/core/tests/world_view_reads.rs)."""
    world = WorldState.from_scenario(generated_scenario)
    made = _rules().view_for_proposals(_controller(world), world, world.minute_index)
    assert made.state_hash == "e62b2a7a8153dc40eb2f4dbf478018e7aa9d9a84d175eb412cf64cb25d3f4fc4"


@pytest.mark.python_rules
def test_the_mark_keeps_the_kernels_of_steps_without_a_python_body(
    generated_scenario: Any,
) -> None:
    assert OfflineSimulationController._view_after_movement is not UpdatePhase._view_after_movement
    assert OfflineSimulationController._view_for_proposals is not ProposePhase._view_for_proposals
    world, controller = _world_with_views(generated_scenario)
    assert "_rust_view" in controller._view_after_movement(world).__dict__
    # The mixin keeps the step's contract, which the controller's kernel carries.
    assert UpdatePhase._view_after_movement.__doc__
    assert (
        OfflineSimulationController._view_after_movement.__doc__
        == UpdatePhase._view_after_movement.__doc__
    )


def test_without_the_mark_the_kernels_run() -> None:
    assert OfflineSimulationController._view_after_movement is not UpdatePhase._view_after_movement
