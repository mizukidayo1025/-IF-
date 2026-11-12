"""The proposal view made from the update phase's view equals a view read from the world.

The proposal view is rules/world_view.rs's refresh of the update phase's (`_view_for_proposals`,
which has no Python body since lane L10); `support.world.view_of` reads the world anew. These tests
check both the values and the audit hash, bit for bit, at every tick of Scenarios with and without
messages and beliefs, and with a full trace.
"""

from __future__ import annotations

import copy
import struct
from collections.abc import Iterator
from contextlib import contextmanager

from support.world import driven_ticks, proposal_view, view_of, watching_phases

from sheltersim import native
from sheltersim.core.engine import TickPhase, TraceLevel
from sheltersim.domain.information import INFORMATION_SPREAD_STATE_KEY
from sheltersim.experiments.runner import run_scenario
from sheltersim.experiments.scenario import ScenarioSpec
from sheltersim.policies.behavior import WorldView


def _same_bits(x: object, y: object) -> bool:
    if isinstance(x, float) and isinstance(y, float):
        return struct.pack("<d", x) == struct.pack("<d", y)
    return x == y and type(x) is type(y)


def _assert_same_views(made: WorldView, read: WorldView) -> None:
    assert made.state_hash == read.state_hash
    assert made.minute_index == read.minute_index
    assert made.resource_service_enabled == read.resource_service_enabled
    assert made.autonomy_enabled == read.autonomy_enabled
    assert dict(made.facilities) == dict(read.facilities)
    assert dict(made.zone_members) == dict(read.zone_members)
    assert dict(made.group_members) == dict(read.group_members)
    assert list(made.agents) == list(read.agents)
    for agent_id, agent in made.agents.items():
        other = read.agents[agent_id]
        assert agent.__dict__.keys() == other.__dict__.keys()
        for name, value in agent.__dict__.items():
            if name == "needs":
                for need in value.__dataclass_fields__:
                    assert _same_bits(getattr(value, need), getattr(other.needs, need))
            else:
                assert _same_bits(value, getattr(other, name)), (agent_id, name)


@contextmanager
def _compared_ticks(worlds: list | None = None) -> Iterator[list[WorldView]]:
    """Each proposal view a Run makes from the update phase's, before the propose phase, compared
    with a new view; the world of each goes to `worlds`."""
    views: list[WorldView] = []
    compared = [0]

    def checked(engine, phase, before):
        if phase is not TickPhase.PROPOSE_ACTIONS or not before:
            return
        compared[0] += 1
        world = engine.world
        refreshed = engine.phase_handler._view_before_proposals is not None
        made = proposal_view(engine.phase_handler, world)
        _assert_same_views(made, view_of(world))
        if refreshed:
            views.append(made)
        if worlds is not None:
            worlds.append(world)

    ticks = driven_ticks()
    with watching_phases(checked):
        yield views
    assert driven_ticks() - ticks == compared[0]


def test_the_proposal_view_equals_a_read_view_at_every_tick(generated_scenario, tmp_path) -> None:
    document = generated_scenario.model_dump(mode="json")
    document["duration_minutes"] = 240
    with _compared_ticks() as views:
        run_scenario(ScenarioSpec.model_validate(document), tmp_path, trace_level=TraceLevel.OFF)
    assert [view.minute_index for view in views] == list(range(240))


def test_the_proposal_view_equals_a_read_view_with_messages_and_beliefs(
    generated_previews,
    information_spread_scenario,
    information_spread_config,
    tmp_path,
) -> None:
    document = copy.deepcopy(generated_previews()["scenario"])
    events = information_spread_scenario(duration=60, correction=True)
    document["duration_minutes"] = 60
    document["scheduled_events"] = events.model_dump(mode="json")["scheduled_events"]
    document["scheduled_events"][0]["payload"]["origin_agent_id"] = "A001"
    configs = document["model_configs"]
    configs["information_spread"] = configs["information_spread"] | information_spread_config()
    worlds: list = []
    with _compared_ticks(worlds) as views:
        run_scenario(ScenarioSpec.model_validate(document), tmp_path, trace_level=TraceLevel.OFF)
    assert len(views) == 60
    state = worlds[-1].model_state[INFORMATION_SPREAD_STATE_KEY]
    assert state["messages"] and any(state["beliefs"].values())


def test_a_change_after_the_update_phase_would_be_seen_only_through_a_read_view(
    generated_scenario,
) -> None:
    """The proposal view takes the fields the update phase writes from the world."""
    from sheltersim.core.world import WorldState
    from sheltersim.domain.body_state import create_body_state
    from sheltersim.experiments.metrics import MetricsRecorder
    from sheltersim.experiments.runner import OfflineSimulationController
    from sheltersim.experiments.timeline import _MetricSink

    world = WorldState.from_scenario(generated_scenario)
    world.model_state.setdefault("body_state_v1", create_body_state(world))
    controller = OfflineSimulationController(world, _MetricSink(MetricsRecorder()))
    before = view_of(world).state_hash
    controller._view_before_proposals = controller._view_after_movement(world)
    agent_id = next(iter(world.agents))
    agent = world.agents[agent_id]
    agent.stress = 0.5 if agent.stress != 0.5 else 0.25
    agent.trust = 0.75 if agent.trust != 0.75 else 0.25
    agent.needs.hunger = 0.5 if agent.needs.hunger != 0.5 else 0.25
    world.model_state["body_state_v1"]["agents"][agent_id]["uncertainty"] = 0.625
    refreshed = controller._view_for_proposals(world, world.minute_index)
    _assert_same_views(refreshed, view_of(world))
    assert refreshed.state_hash != before


def test_a_config_edited_between_ticks_is_seen_by_the_next_phases(generated_scenario) -> None:
    """Parses and views are kept between ticks only while model_configs keeps its values."""
    from sheltersim.core.engine import TickEngine
    from sheltersim.core.world import WorldState
    from sheltersim.domain.geometry_overlay import _CONFIG_SCOPE, effective_geometry
    from sheltersim.experiments.metrics import MetricsRecorder
    from sheltersim.experiments.runner import OfflineSimulationController
    from sheltersim.experiments.timeline import _MetricSink

    world = WorldState.from_scenario(generated_scenario)
    controller = OfflineSimulationController(world, _MetricSink(MetricsRecorder()))
    engine = TickEngine(world, phase_handler=controller)
    engine.step()
    # The kernels read the configs' section geometry, kept by the stamp of model_configs; the
    # scope keeps what the Python rules parsed.
    parsed = dict(controller._parsed_configs)
    engine.step()
    assert controller._parsed_configs.keys() == parsed.keys()

    facility_id, point = next(
        iter(world.model_configs["facility_geometry"]["facility_points"].items())
    )
    moved_x = point["x"] + 0.125
    point["x"] = moved_x
    seen = []

    def watch(phase, before):
        assert _CONFIG_SCOPE.get() is controller._parsed_configs
        seen.append(effective_geometry(world).facility_points[facility_id].x)

    engine.phase_observer = watch
    ticks = driven_ticks()
    engine.step()
    assert driven_ticks() - ticks == 1
    assert len(seen) == 14 and all(value == moved_x for value in seen)
    kernel_points = dict(native.rules("movement_geometry").geo_summary_of(world)["facility_points"])
    assert kernel_points[facility_id][0] == moved_x
    # The zones' conditions are kept in the Run, read again for the configs' new stamp.
    assert controller._run_core.views_kept()[1] == len(world.zones)


def test_a_tick_with_kept_inputs_gives_the_state_of_one_with_none(generated_scenario) -> None:
    """Twenty ticks with all that the controller keeps, against dropping it at each phase."""
    from sheltersim.core.engine import TickEngine
    from sheltersim.core.world import WorldState
    from sheltersim.experiments.metrics import MetricsRecorder
    from sheltersim.experiments.runner import OfflineSimulationController
    from sheltersim.experiments.timeline import _MetricSink

    def run(drop: bool) -> dict:
        world = WorldState.from_scenario(generated_scenario)
        controller = OfflineSimulationController(world, _MetricSink(MetricsRecorder()))

        def forget(phase, before):
            if before:
                controller._parsed_configs.clear()
                controller._model_selections.clear()
                controller._run_core.forget_views()
                controller._view_before_proposals = None

        engine = TickEngine(world, phase_handler=controller)
        if drop:
            engine.phase_observer = forget
        ticks = driven_ticks()
        for _ in range(120):
            engine.step()
        assert driven_ticks() - ticks == 120
        return world.to_dict()

    assert run(False) == run(True)


def test_requests_of_one_kind_get_the_reasons_they_get_alone(generated_scenario) -> None:
    """The snapshot reads the reasons of one kind once for all its requests; each request's are
    what `support_wait_reason_codes` gives it alone."""
    from sheltersim.core.engine import TickEngine
    from sheltersim.core.world import WorldState
    from sheltersim.domain.models import SupportRequestStatus
    from sheltersim.domain.staff import support_wait_reason_codes
    from sheltersim.experiments import metrics
    from sheltersim.experiments.runner import OfflineSimulationController
    from sheltersim.experiments.timeline import _MetricSink

    document = generated_scenario.model_dump(mode="json")
    document["duration_minutes"] = 360
    world = WorldState.from_scenario(ScenarioSpec.model_validate(document))
    engine = TickEngine(
        world,
        phase_handler=OfflineSimulationController(world, _MetricSink(metrics.MetricsRecorder())),
    )
    compared = 0
    for _ in range(360):
        engine.step()
        active = [
            request
            for request in world.unsettled_support_requests()
            if request.status in (SupportRequestStatus.PENDING, SupportRequestStatus.ASSIGNED)
        ]
        details, _ = metrics._support_wait_snapshot(world, active, world.minute_index)
        for detail in details.values():
            alone = support_wait_reason_codes(
                world, detail["request_id"], minute=world.minute_index
            )
            assert detail["reason_codes"] == list(alone)
            compared += 1
    assert compared


def test_swapped_configs_do_not_pile_up_in_the_controller(generated_scenario) -> None:
    """Copy-and-swap handlers replace model_configs; only the current object's parses stay."""
    from copy import deepcopy

    from sheltersim.core.engine import TickEngine
    from sheltersim.core.world import WorldState
    from sheltersim.experiments.metrics import MetricsRecorder
    from sheltersim.experiments.runner import OfflineSimulationController
    from sheltersim.experiments.timeline import _MetricSink

    world = WorldState.from_scenario(generated_scenario)
    controller = OfflineSimulationController(world, _MetricSink(MetricsRecorder()))
    engine = TickEngine(world, phase_handler=controller)
    engine.step()
    parsed = len(controller._parsed_configs)
    for _ in range(3):
        world.model_configs = deepcopy(world.model_configs)
        engine.step()
    assert len(controller._parsed_configs) == parsed
    assert [known for known, _ in controller._model_selections] == [world.model_configs]
    assert controller._model_selections[0][0] is world.model_configs


def test_the_view_before_proposals_is_the_run_s_scratch(generated_scenario) -> None:
    """`_view_before_proposals` reads and writes the Run's scratch (Issue #152 lane P9-2a,
    decision D4): the update phase's snapshot comes back as a new `WorldView` of it at each read,
    a view Python set comes back as itself, None empties it, and the controller keeps none of it."""
    from dataclasses import replace

    from sheltersim.core.engine import TickEngine, TickPhase
    from sheltersim.core.world import WorldState
    from sheltersim.experiments.metrics import MetricsRecorder
    from sheltersim.experiments.runner import OfflineSimulationController
    from sheltersim.experiments.timeline import _MetricSink

    world = WorldState.from_scenario(generated_scenario)
    controller = OfflineSimulationController(world, _MetricSink(MetricsRecorder()))
    assert controller._view_before_proposals is None
    assert "_view_before_proposals" not in controller.__dict__

    made = controller._view_after_movement(world)
    controller._view_before_proposals = made
    first = controller._view_before_proposals
    second = controller._view_before_proposals
    assert first is not made and first is not second
    assert "_rust_view" in first.__dict__ and "agents" not in first.__dict__
    assert first.__dict__["_rust_view"] is not made.__dict__["_rust_view"]
    _assert_same_views(replace(first, state_hash=None), replace(made, state_hash=None))
    assert "_view_before_proposals" not in controller.__dict__

    python_view = replace(view_of(world, include_state_hash=False))
    controller._view_before_proposals = python_view
    assert controller._view_before_proposals is python_view
    controller._view_before_proposals = None
    assert controller._view_before_proposals is None

    # A tick's update phase leaves its view in the scratch; the propose phase takes it.
    engine = TickEngine(world, phase_handler=controller)
    seen: list[tuple[TickPhase, bool]] = []

    def watch(phase: TickPhase, before: bool) -> None:
        if not before and phase in (TickPhase.UPDATE_AGENT_STATE, TickPhase.PROPOSE_ACTIONS):
            view = controller._view_before_proposals
            seen.append((phase, view is not None and view.minute_index == world.minute_index))

    engine.phase_observer = watch
    engine.step()
    assert seen == [(TickPhase.UPDATE_AGENT_STATE, True), (TickPhase.PROPOSE_ACTIONS, False)]
