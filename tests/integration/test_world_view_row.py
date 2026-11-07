"""The views of rules/world_view.rs: every field the bits the Python rule gave, made when a reader
asks.

The phases' steps `_view_after_movement` and `_view_for_proposals` make them, and have no Python
body (lane L10): what the kernel refuses is a KernelRefusal. The Python rule `WorldView.from_world`
went too: the views of whole Runs are compared with digests written down from it (on Linux, as the
gate's hashes: the floats come from libm), and a view the kernel kept, refreshed or read before a
change with a new view of the same world (`support.world.view_of`).

Model version 2 (step 4, L11: the pinned libm crate and the information events' draw keys)
changed some of these results: those digests were re-recorded from the kernels of that model
and are no longer the deleted Python rules'.
"""

from __future__ import annotations

import copy
import hashlib
import json
from collections.abc import Mapping
from dataclasses import replace
from typing import Any

import pytest
from support.world import view_of

from sheltersim import native
from sheltersim.core.engine import TickEngine, TickPhase
from sheltersim.core.kernels import KernelRefusal
from sheltersim.core.world import WorldState
from sheltersim.domain.body_state import create_body_state
from sheltersim.experiments.metrics import MetricsRecorder
from sheltersim.experiments.phases.propose import ProposePhase
from sheltersim.experiments.phases.update import UpdatePhase
from sheltersim.experiments.runner import OfflineSimulationController
from sheltersim.experiments.scenario import ScenarioSpec
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


def _assert_same(made: WorldView, other: WorldView) -> None:
    assert made.minute_index == other.minute_index
    assert made.state_hash == other.state_hash
    assert made.resource_service_enabled is other.resource_service_enabled
    assert made.autonomy_enabled is other.autonomy_enabled
    assert made._has_population_context is other._has_population_context
    for name in _PARTS:
        assert _exact(getattr(made, name)) == _exact(getattr(other, name)), name
    assert made.immediate_needs() == other.immediate_needs()


def _rules() -> Any:
    return native.rules("world_view")


def _controller(world: WorldState) -> OfflineSimulationController:
    return OfflineSimulationController(world, _MetricSink(MetricsRecorder()))


def _views_of_a_run(scenario: ScenarioSpec) -> tuple[str, int]:
    """A digest of the views of a Run, in order: each update phase's, which the controller keeps
    for the propose phase, and a new view of the world as the propose phase starts. The count."""
    world = WorldState.from_scenario(scenario)
    controller = _controller(world)
    engine = TickEngine(world, phase_handler=controller)
    texts: list[str] = []

    def watch(phase: TickPhase, before: bool) -> None:
        if phase is TickPhase.UPDATE_AGENT_STATE and not before:
            made = controller._view_before_proposals
            assert "_rust_view" in made.__dict__
            texts.append(json.dumps(_fields(made)))
        elif phase is TickPhase.PROPOSE_ACTIONS and before:
            texts.append(json.dumps(_fields(view_of(world))))

    engine.phase_observer = watch
    engine.run(scenario.duration_minutes)
    return hashlib.sha256("\n".join(texts).encode("utf-8")).hexdigest()[:16], len(texts)


def test_each_view_of_a_run_has_the_bits_of_the_python_rule(generated_scenario: Any) -> None:
    document = generated_scenario.model_dump(mode="json")
    document["duration_minutes"] = 120
    digest, count = _views_of_a_run(ScenarioSpec.model_validate(document))
    assert count == 240
    assert digest == "f1bc3977da991e96"


def test_views_with_false_claims_have_the_bits_of_the_python_rule(
    generated_previews: Any, information_spread_scenario: Any, information_spread_config: Any
) -> None:
    document = copy.deepcopy(generated_previews()["scenario"])
    events = information_spread_scenario(duration=60, correction=True)
    document["duration_minutes"] = 60
    document["scheduled_events"] = events.model_dump(mode="json")["scheduled_events"]
    document["scheduled_events"][0]["payload"]["origin_agent_id"] = "A001"
    configs = document["model_configs"]
    configs["information_spread"] = configs["information_spread"] | information_spread_config()
    digest, count = _views_of_a_run(ScenarioSpec.model_validate(document))
    assert count == 120
    assert digest == "77460590f6646c41"


def _world_with_views(scenario: Any) -> tuple[WorldState, OfflineSimulationController]:
    world = WorldState.from_scenario(scenario)
    world.model_state.setdefault("body_state_v1", create_body_state(world))
    return world, _controller(world)


def test_a_household_overrides_the_profile_group(generated_scenario: Any) -> None:
    world, controller = _world_with_views(generated_scenario)
    people = {agent_id: {"household_id": "H"} for agent_id in list(world.agents)[:2]}
    third = list(world.agents)[2]
    people[third] = {"household_id": None}
    world.model_state["population_presence_v1"] = {"people": people}
    made = _rules().view_after_movement(controller, world)
    assert made.agents[list(world.agents)[0]].group_id == "H"
    assert made.agents[third].group_id is None
    assert made.group_members["H"] == tuple(sorted(people))[:2]


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
    generated_scenario: Any, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The prior view is Rust's, so Python didn't make it, and no rule has asked for its people."""
    world = WorldState.from_scenario(generated_scenario)
    controller = _controller(world)
    engine = TickEngine(world, phase_handler=controller)
    seen: list[set[str]] = []
    run_propose = OfflineSimulationController._run_propose_phase

    def before_propose(self: Any, world_: Any, minute: int, rng: Any) -> None:
        seen.append(set(self._view_before_proposals.__dict__))
        run_propose(self, world_, minute, rng)

    monkeypatch.setattr(OfflineSimulationController, "_run_propose_phase", before_propose)
    for _ in range(3):
        engine.step()
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


def test_the_proposal_view_reads_again_what_the_update_phase_writes(
    generated_scenario: Any,
) -> None:
    world, controller = _world_with_views(generated_scenario)
    controller._view_before_proposals = _rules().view_after_movement(controller, world)
    prior = view_of(world, include_state_hash=False)
    agent = next(iter(world.agents.values()))
    agent.stress = 0.5 if agent.stress != 0.5 else 0.25
    agent.trust = 0.75 if agent.trust != 0.75 else 0.25
    agent.needs.hunger = 0.5 if agent.needs.hunger != 0.5 else 0.25
    world.model_state["body_state_v1"]["agents"][agent.id]["uncertainty"] = 0.625
    made = _rules().view_for_proposals(controller, world, world.minute_index)
    assert controller._view_before_proposals is None
    _assert_same(made, view_of(world))
    assert made.agents[agent.id].stress != prior.agents[agent.id].stress


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
            "plain dict",
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


def test_a_changed_context_is_seen_at_the_next_view(generated_scenario: Any) -> None:
    world, controller = _world_with_views(generated_scenario)
    assert _rules().view_after_movement(controller, world).agents["A001"].roles == ("alone",)
    record = world.model_configs["population_context"]["people"]["A001"]
    record["roles"] = ["parent"]
    second = _rules().view_after_movement(controller, world)
    assert second.agents["A001"].roles == ("parent",)
    record["roles"] = ["child"]
    third = _rules().view_after_movement(controller, world)
    assert third.agents["A001"].roles == ("child",)
    _assert_same(third, view_of(world, include_state_hash=False))


def test_a_changed_zone_modifier_or_a_new_cache_is_seen_at_the_next_view(
    generated_scenario: Any,
) -> None:
    world, controller = _world_with_views(generated_scenario)
    zone = next(iter(world.agents.values())).zone_id
    _rules().view_after_movement(controller, world)
    world.model_state["zone_environment_v1"]["modifiers"]["m"] = {
        "zone_id": zone,
        "start_minute": 0,
        "op": "set_lighting",
        "parameters": {"level": 0.25},
        "start_event_id": "e",
        "source_id": "s",
        "assumption_ids": [],
    }
    made = _rules().view_after_movement(controller, world)
    _assert_same(made, view_of(world, include_state_hash=False))
    agent = next(a for a in made.agents.values() if a.zone_id == zone)
    assert agent.lighting_level == 0.25 and agent.condition_modifier_event_ids == ("e",)
    # What the runner does when model_configs changed.
    world.model_configs["zone_environment"]["noise_rest_efficiency_loss"] = 0.25
    controller._zone_environments = {}
    made = _rules().view_after_movement(controller, world)
    _assert_same(made, view_of(world, include_state_hash=False))


def test_stressors_outages_and_sanitation_are_read_as_python_reads_them(
    generated_scenario: Any,
) -> None:
    world, controller = _world_with_views(generated_scenario)
    facility = next(iter(world.facilities.values()))
    zone = next(iter(world.agents.values())).zone_id
    zones = [zone, *sorted(set(world.zones) - {zone})]
    world.model_state["infrastructure_v1"] = {
        "environmental_stressors": {
            "b": {
                "active": True,
                "zone_id": zones[0],
                "stress_per_minute": 0.1,
                "start_event_id": "z",
                "source_id": "second",
            },
            "a": {
                "active": True,
                "zone_id": zones[0],
                "stress_per_minute": 0.2,
                "start_event_id": "y",
                "source_id": "first",
            },
            "c": {
                "active": False,
                "zone_id": zones[0],
                "stress_per_minute": 9.0,
                "start_event_id": "x",
                "source_id": "off",
            },
            "d": {
                "active": True,
                "zone_id": "no such zone",
                "stress_per_minute": 9.0,
                "start_event_id": "w",
                "source_id": "elsewhere",
            },
        },
        "outages": {
            "o": {"active": True, "affected_facility_ids": [facility.id], "start_event_id": "o2"},
            "p": {"active": False, "affected_facility_ids": [facility.id], "start_event_id": "o1"},
        },
    }
    facility.kind = "Water_Station"
    world.model_state["operational_events_v1"] = {
        "water_outages": {
            "by_zone": {
                "outage_event_id": "o3",
                "active": True,
                "target_kind": "zone",
                "target_id": facility.zone_id,
            },
            "by_id": {
                "outage_event_id": "o0",
                "active": True,
                "target_kind": "facility",
                "target_id": facility.id,
            },
            "other": {
                "outage_event_id": "o9",
                "active": True,
                "target_kind": "facility",
                "target_id": "another",
            },
        }
    }
    staff = world.model_state.setdefault("staff_operations_v1", {})
    staff.setdefault("zones", {}).setdefault(zones[0], {})["sanitation"] = 0.25
    made = _rules().view_after_movement(controller, world)
    # What the Python rule gave.
    assert {key: value.facility_event_context_ids for key, value in made.facilities.items()} == {
        key: ("o0", "o2", "o3") if key == facility.id else () for key in world.facilities
    }
    for agent in made.agents.values():
        assert agent.zone_id == zone
        assert agent.environmental_stress_per_minute == float.fromhex("0x1.3333333333334p-2")
        assert agent.environmental_stressor_event_ids == ("y", "z")
        assert agent.environmental_assumption_source_id == "first"
        assert agent.sanitation_level == 0.25


# The Python rule's audit hashes, by (schema_version, information, life context).
_HASHES = {
    (None, False, False): "5dd4adee1a9ebcf1f2f194a859e16be0e40c4e854ae5ba07f8cabeddf27e9302",
    (None, False, True): "0ec91f0ca399ce626a6e54bf11fb93068fcb9fc3c65e15aded1e7ea0dcb26883",
    (None, True, False): "17d67b8e26a07746c88b42d7f4783e74d195c2c0b09a6643783fb7bce14561e5",
    (None, True, True): "e62b2a7a8153dc40eb2f4dbf478018e7aa9d9a84d175eb412cf64cb25d3f4fc4",
    (6, False, False): "5dd4adee1a9ebcf1f2f194a859e16be0e40c4e854ae5ba07f8cabeddf27e9302",
    (6, False, True): "0ec91f0ca399ce626a6e54bf11fb93068fcb9fc3c65e15aded1e7ea0dcb26883",
    (6, True, False): "17d67b8e26a07746c88b42d7f4783e74d195c2c0b09a6643783fb7bce14561e5",
    (6, True, True): "e62b2a7a8153dc40eb2f4dbf478018e7aa9d9a84d175eb412cf64cb25d3f4fc4",
}


def test_the_rust_view_hash_covers_each_form_of_the_document(generated_scenario: Any) -> None:
    for schema_version in (None, 6):
        for information in (False, True):
            for life_context in (False, True):
                world = WorldState.from_scenario(generated_scenario)
                controller = _controller(world)
                if schema_version is not None:
                    world.schema_version = schema_version
                    world.model_configs.setdefault("resource_service", {"a": 1.5})
                if not life_context:
                    world.model_state.pop("body_state_v1", None)
                    world.model_configs.pop("population_context", None)
                    world.model_state.pop("population_context_v1", None)
                assert "information_spread_v1" in world.model_state
                if not information:
                    world.model_state.pop("information_spread_v1", None)
                made = _rules().view_for_proposals(controller, world, world.minute_index)
                assert made.state_hash == _HASHES[schema_version, information, life_context]


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
