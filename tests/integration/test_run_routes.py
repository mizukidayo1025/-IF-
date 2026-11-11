"""The kernels' route graphs are kept in each controller's Run, by epoch (#152, lanes 7-3, 7-4).

Each controller (a Run, its continuation, each Branch) makes its own `RunCore`
(`native/src/core/run.rs`) and gives it the route capacities it read. `_refresh_space` changes the
epoch, at the controller's start and after a dispatch that applied a geometry or structure event,
and nowhere else. It builds the people's and the staff's routers from the world's zone and edge
tables right then, with `ZoneGraph`'s checks and words (lane 7-4); the Python graphs it still makes
are the Python readers'. A graph is a snapshot: an edge closed without a refresh is not seen until
the next one, as the Python graph never saw it. Where the tables cannot be read, and after
`refresh_routes()` without a world, the kernels read the Python graphs when they first ask.
"""

from __future__ import annotations

import copy
from typing import Any

import pytest

from sheltersim import native
from sheltersim.core.engine import TickEngine
from sheltersim.core.kernels import KernelRefusal
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
    # Both are built from the tables at the refresh.
    assert core.routes_built() == (1, 1)
    assert _toilet_route(controller) == (FIRST_LEG, "route_corridor_toilet")
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
    # Each refresh built both again.
    assert core.routes_built() == (3, 3)
    assert _toilet_route(controller) == (FIRST_LEG, "route_corridor_toilet")


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
    assert core.epoch == 2 and core.routes_built() == (2, 2)
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


# ---- From the tables (lane 7-4) -----------------------------------------------------------------


def _controller(scenario: Any) -> Any:
    return OfflineSimulationController(
        WorldState.from_scenario(scenario), _MetricSink(MetricsRecorder())
    )


def _narrowed(scenario: Any) -> ScenarioSpec:
    """The generated plan with one route too narrow for anyone (`route_is_physically_clear`)."""
    document = copy.deepcopy(scenario.model_dump(mode="json"))
    routes = document["model_configs"]["facility_geometry"]["routes"]
    routes[len(routes) // 2]["width_m"] = 0.3
    return ScenarioSpec.model_validate(document)


def _held(core: Any, which: str, origin: str, destination: str) -> Any:
    route = core.held_route(which, origin, destination)
    return None if route is None else (tuple(route[0]), tuple(route[1]), route[2])


def _python(router: Any, origin: str, destination: str) -> Any:
    route = router.route(origin, destination)
    return None if route is None else (route.zone_ids, route.edge_ids, route.travel_minutes)


@pytest.mark.parametrize("narrowed", [False, True])
def test_the_tables_give_the_python_graphs_routes(generated_scenario, narrowed) -> None:
    scenario = _narrowed(generated_scenario) if narrowed else generated_scenario
    controller = _controller(scenario)
    world, core = controller.world, controller._run_core
    staff_edges = {edge.id for edge in controller.staff_space.graph.edges}
    assert (len(staff_edges) < len(world.zone_edges)) is narrowed
    differs = 0
    for origin in sorted(world.zones):
        for destination in sorted(world.zones):
            people = _held(core, "people", origin, destination)
            assert people == _python(controller.space, origin, destination)
            staff = _held(core, "staff", origin, destination)
            assert staff == _python(controller.staff_space, origin, destination)
            differs += people != staff
    assert (differs > 0) is narrowed


def _refused(world: WorldState, capacities: Any) -> BaseException:
    from sheltersim.domain.zones import ZoneGraph

    with pytest.raises(Exception) as python:
        ZoneGraph.from_states(world.zones, world.zone_edges, edge_capacities=capacities)
    return python.value


@pytest.mark.parametrize(
    "change",
    [
        "capacity_of_no_edge",
        "capacity_too_large",
        "negative_zone_capacity",
        "nan_display",
        "zone_key",
        "edge_to_no_zone",
        "edge_to_itself",
        "zero_minutes",
        "edge_id",
    ],
)
def test_a_refresh_raises_zone_graph_s_error(generated_scenario, change) -> None:
    controller = _controller(generated_scenario)
    world, core = controller.world, controller._run_core
    capacities: dict[Any, Any] = {}
    first_zone = next(iter(world.zones))
    edge = world.zone_edges[FIRST_LEG]
    if change == "capacity_of_no_edge":
        capacities = {"no_such_edge": 1}
    elif change == "capacity_too_large":
        capacities = {FIRST_LEG: 101}
    elif change == "negative_zone_capacity":
        world.zones[first_zone].capacity = -1
    elif change == "nan_display":
        world.zones[first_zone].display_y = float("nan")
    elif change == "zone_key":
        world.zones[first_zone].id = "elsewhere"
    elif change == "edge_to_no_zone":
        edge.to_zone_id = "nowhere"
    elif change == "edge_to_itself":
        edge.to_zone_id = edge.from_zone_id
    elif change == "zero_minutes":
        edge.travel_minutes = 0
    else:
        edge.id = "-leg"
    expected = _refused(world, capacities)
    if capacities:
        core = native.run_core(capacities)
    epoch = core.epoch
    # The kernels' refresh alone, which reads the tables: the class and the words of ZoneGraph.
    with pytest.raises(type(expected)) as raised:
        core.refresh_routes(world)
    assert str(raised.value) == str(expected)
    assert core.epoch == epoch + 1 and core.routes_built() == (None, None)


def test_the_staff_graph_s_plan_refuses_as_route_is_physically_clear(generated_scenario) -> None:
    from sheltersim.domain.movement import route_is_physically_clear

    controller = _controller(generated_scenario)
    world, core = controller.world, controller._run_core
    world.model_configs["facility_geometry"] = {"not": "a plan"}
    with pytest.raises(KernelRefusal) as python:
        route_is_physically_clear(world, FIRST_LEG, mobility="independent")
    with pytest.raises(KernelRefusal) as raised:
        core.refresh_routes(world)
    assert str(raised.value) == str(python.value)
    # The people's router was built before the plan was read.
    assert core.routes_built() == (core.epoch, None)


def test_rows_that_do_not_fit_leave_the_routers_to_the_python_graphs(generated_scenario) -> None:
    controller = _controller(generated_scenario)
    world, core = controller.world, controller._run_core
    store = world._core.entities()
    zone = next(iter(world.zones.values()))
    zone.capacity = 2.5
    assert "not an int" in store.zones_tainted
    controller._refresh_space()
    assert core.routes_built() == (None, None)
    # Read from controller.space when first asked, as before lane 7-4.
    assert _toilet_route(controller) == (FIRST_LEG, "route_corridor_toilet")
    assert core.routes_built() == (core.epoch, None)
    zone.capacity = 3
    assert store.refit(world) and store.zones_tainted is None
    controller._refresh_space()
    assert core.routes_built() == (core.epoch, core.epoch)
