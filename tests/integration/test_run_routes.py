"""The kernels' route graphs are kept in each controller's Run, by epoch (Issue #152, lane 7-3).

Each controller (a Run, its continuation, each Branch) makes its own `RunCore`
(`native/src/core/run.rs`) and gives it the route capacities it read. `_refresh_space` changes the
epoch, at the controller's start and after a dispatch that applied a geometry or structure event,
and nowhere else. The kernels build the people's and the staff's routers from the graphs
`_refresh_space` made when they first ask in an epoch. A graph is a snapshot: an edge closed
without a refresh is not seen until the next one, as the Python graph never saw it.
"""

from __future__ import annotations

import copy
from typing import Any

import pytest

from sheltersim.core.engine import TickEngine
from sheltersim.core.world import WorldState
from sheltersim.events.effects import (
    EffectProgram,
    SupportMode,
    TargetSelector,
    TimedAction,
    schedule_effect_program,
)
from sheltersim.experiments.metrics import MetricsRecorder
from sheltersim.experiments.runner import OfflineSimulationController
from sheltersim.experiments.scenario import ScenarioSpec
from sheltersim.experiments.timeline import _MetricSink
from sheltersim.storage.projects import digest

FIRST_LEG = "route_sleeping_corridor"
CLOSE_AT, OPEN_AT = 3, 9


def _close_first_leg() -> EffectProgram:
    """`set_route_access` of the sleeping area's leg, from CLOSE_AT to OPEN_AT."""
    return EffectProgram(
        program_id="program-close-first-leg",
        narrative="close the first leg",
        source_text="close the first leg",
        simulation_start_minute=CLOSE_AT,
        effects=[
            TimedAction(
                effect_id="close-first-leg",
                op="set_route_access",
                target_selector=TargetSelector(kind="route", target_id=FIRST_LEG),
                start_minute=CLOSE_AT,
                end_minute=OPEN_AT,
                parameters={"accessible": False},
                units="boolean",
                source_id="ASM-RUN-ROUTES-TEST-V1",
                assumption_ids=[],
                policy_dependencies=["geometry_overlay_v1"],
                reversible=True,
            )
        ],
        assumptions=[],
        support_mode=SupportMode.NATIVE,
        compiler_provenance={"compiler_kind": "offline_rule", "compiler_version": "test-v1"},
    )


@pytest.fixture
def refreshes(monkeypatch: pytest.MonkeyPatch) -> dict[int, list[int]]:
    """The minutes at which each controller (by id) ran `_refresh_space`."""
    seen: dict[int, list[int]] = {}
    original = OfflineSimulationController._refresh_space

    def counted(self: Any) -> None:
        seen.setdefault(id(self), []).append(self.world.minute_index)
        original(self)

    monkeypatch.setattr(OfflineSimulationController, "_refresh_space", counted)
    return seen


def _toilet_route(controller: Any) -> tuple[str, ...]:
    """The edges of the kernel's route from the sleeping area to the toilet."""
    agent_id = next(
        agent_id
        for agent_id, agent in sorted(controller.world.agents.items())
        if agent.zone_id == "sleeping"
    )
    route = controller._route("sleeping", "toilet", agent_id)
    return () if route is None else route.edge_ids


def test_the_epoch_changes_where_the_space_is_refreshed(generated_scenario, refreshes) -> None:
    world = WorldState.from_scenario(generated_scenario)
    world.minute_index = 0
    schedule_effect_program(world, _close_first_leg())
    controller = OfflineSimulationController(world, _MetricSink(MetricsRecorder()))
    core = controller._run_core
    assert refreshes[id(controller)] == [0]
    assert core.epoch == 1
    # Nothing is built until a kernel asks.
    assert core.routes_built() == (None, None)
    assert _toilet_route(controller) == (FIRST_LEG, "route_corridor_toilet")
    assert core.routes_built() == (1, None)
    engine = TickEngine(world, phase_handler=controller)
    epochs = []
    for _ in range(OPEN_AT + 2):
        engine.step()
        epochs.append(core.epoch)
        assert core.epoch == len(refreshes[id(controller)])
        if core.epoch == 2:
            # The generated plan has one way to the toilet.
            assert _toilet_route(controller) == ()
    # The event's start and its end each refreshed once, in the minute they applied.
    assert refreshes[id(controller)] == [0, CLOSE_AT, OPEN_AT]
    assert epochs == [1] * CLOSE_AT + [2] * (OPEN_AT - CLOSE_AT) + [3, 3]
    # Each epoch's routers are built again when asked: the staff's by the tick's assignment.
    assert core.routes_built()[1] in (None, 3)
    assert _toilet_route(controller) == (FIRST_LEG, "route_corridor_toilet")
    assert core.routes_built()[0] == 3


def test_a_closed_edge_is_seen_after_the_next_refresh(generated_scenario, refreshes) -> None:
    world = WorldState.from_scenario(generated_scenario)
    controller = OfflineSimulationController(world, _MetricSink(MetricsRecorder()))
    core = controller._run_core
    before = _toilet_route(controller)
    assert before == (FIRST_LEG, "route_corridor_toilet")
    world.zone_edges[FIRST_LEG].closed = True
    # The graphs of the last refresh, the Python one and the kernel's, still have the edge.
    assert controller.space.route("sleeping", "toilet").edge_ids == before
    assert _toilet_route(controller) == before
    assert core.epoch == 1
    controller._refresh_space()
    assert core.epoch == 2 and core.routes_built() == (None, None)
    assert controller.space.route("sleeping", "toilet") is None
    assert _toilet_route(controller) == ()
    world.zone_edges[FIRST_LEG].closed = False
    assert _toilet_route(controller) == ()
    controller._refresh_space()
    assert _toilet_route(controller) == before
    assert refreshes[id(controller)] == [0, 0, 0]


def test_each_run_continuation_and_branch_keeps_its_own_routes(
    tmp_path, generated_previews, saved_timeline, refreshes, monkeypatch
) -> None:
    monkeypatch.setenv("SHELTERSIM_LLM_MODE", "offline")
    controllers: list[Any] = []
    original = OfflineSimulationController.__init__

    def recorded(self: Any, *arguments: Any, **options: Any) -> None:
        original(self, *arguments, **options)
        controllers.append(self)

    monkeypatch.setattr(OfflineSimulationController, "__init__", recorded)
    document = copy.deepcopy(generated_previews(duration_days=60 / 1440)["scenario"])
    _, timeline = saved_timeline(tmp_path / "projects", ScenarioSpec.model_validate(document))
    timeline.advance("main", 2, checkpoint_interval=2)
    program = _close_first_leg()
    timeline.fork(
        "main",
        2,
        "closed",
        program=program,
        approved_program_hash=digest(program.model_dump(mode="json")),
    )
    timeline.advance("closed", OPEN_AT - 2, checkpoint_interval=OPEN_AT)
    timeline.advance("main", OPEN_AT - 2, checkpoint_interval=OPEN_AT)
    assert len(controllers) >= 3
    cores = [controller._run_core for controller in controllers]
    assert len({id(core) for core in cores}) == len(cores)
    for controller in controllers:
        assert controller._run_core.epoch == len(refreshes[id(controller)])
    # The Branch's own refresh, when its event applied, changed no other Run's epoch.
    assert any(CLOSE_AT in refreshes[id(controller)] for controller in controllers)
    before = [core.epoch for core in cores]
    controllers[-1]._refresh_space()
    assert [core.epoch for core in cores] == [*before[:-1], before[-1] + 1]
