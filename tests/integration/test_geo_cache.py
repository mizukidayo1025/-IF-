"""The kernels' floor plan is kept with the configs, by the overlay's stamp (Issue #152, lane 7-2).

native/src/rules/movement_geometry.rs parses facility_geometry and spatial_constraints once for a
stamp of model_configs (the configs' section `geometry`) and keeps the plans (`Geo`) built from
them with the overlay's facility moves and partitions, by the overlay's stamp, and, when that
changed, by the canonical bytes of the two parts. A plan never follows the overlay's order but in
the order of the facility points, which no rule reads. A parse error is raised again by a second
parse, with the same class and text (policy E14).
"""

from __future__ import annotations

import copy
from typing import Any

import pytest
import sheltersim_native

from sheltersim import native
from sheltersim.contracts.extensions import FacilityGeometry
from sheltersim.contracts.spatial import SpatialConstraints
from sheltersim.core.world import WorldState
from sheltersim.domain.geometry_overlay import GEOMETRY_OVERLAY_KEY, parsed_facility_geometry
from sheltersim.domain.movement import route_is_physically_clear
from sheltersim.experiments.runner import run_scenario

GEOMETRY = native.rules("movement_geometry")


def _world(generated_scenario: Any) -> WorldState:
    world = WorldState.from_scenario(generated_scenario)
    assert "facility_geometry" in world.model_configs
    world.model_configs.setdefault("spatial_constraints", {})
    return world


def _summary(world: WorldState) -> dict[str, Any]:
    return GEOMETRY.geo_summary_of(world)


def _overlay(world: WorldState) -> dict[str, Any]:
    return world.model_state.setdefault(
        GEOMETRY_OVERLAY_KEY,
        {"facility_points": {}, "partitions": {}, "route_modifiers": {}, "history": []},
    )


def _partition(world: WorldState, corner: float) -> dict[str, Any]:
    zone = world.model_configs["facility_geometry"]["zones"][0]
    x = min(point["x"] for point in zone["polygon"]) + corner
    y = min(point["y"] for point in zone["polygon"]) + corner
    return {
        "zone_id": zone["zone_id"],
        "polygon_m": [
            {"x": x, "y": y},
            {"x": x + 0.5, "y": y},
            {"x": x + 0.5, "y": y + 0.5},
        ],
        "source_ids": ["ASM-TEST"],
    }


def _moves(world: WorldState) -> dict[str, Any]:
    points = world.model_configs["facility_geometry"]["facility_points"]
    facility_id, point = next(iter(points.items()))
    return {
        facility_id: {"point_m": {"x": point["x"] + 0.25, "y": point["y"]}},
        "installed-z": {"point_m": {"x": point["x"], "y": point["y"] + 0.25}},
        "installed-a": {"point_m": {"x": point["x"] + 0.25, "y": point["y"] + 0.25}},
    }


def test_the_configs_are_parsed_once_for_a_stamp(
    generated_scenario: Any, count_validations: Any
) -> None:
    world = _world(generated_scenario)
    facility = count_validations(FacilityGeometry)
    spatial = count_validations(SpatialConstraints)
    # The world's kernels read the configs as it was made: a write gives them a new stamp.
    world.model_configs["spatial_constraints"] = dict(world.model_configs["spatial_constraints"])
    edges = [route["edge_id"] for route in world.model_configs["facility_geometry"]["routes"]]
    first = [route_is_physically_clear(world, edge) for edge in edges]
    for _ in range(3):
        assert [route_is_physically_clear(world, edge) for edge in edges] == first
    assert (len(facility), len(spatial)) == (1, 1)
    assert GEOMETRY.geo_plans_of(world) == 1
    # A write in place is another stamp: parsed again at the next read, and seen.
    world.model_configs["spatial_constraints"]["obstacles"] = [
        {
            "id": "ob-test",
            "polygon": [
                {"x": 0.0, "y": 0.0},
                {"x": 1000.0, "y": 0.0},
                {"x": 1000.0, "y": 1000.0},
                {"x": 0.0, "y": 1000.0},
            ],
        }
    ]
    assert not any(route_is_physically_clear(world, edge) for edge in edges)
    assert (len(facility), len(spatial)) == (2, 2)
    del world.model_configs["spatial_constraints"]["obstacles"]
    assert [route_is_physically_clear(world, edge) for edge in edges] == first


def test_a_plan_is_kept_by_the_overlay_stamp_then_by_its_parts(generated_scenario: Any) -> None:
    world = _world(generated_scenario)
    overlay = _overlay(world)
    plain = _summary(world)
    assert GEOMETRY.geo_plans_of(world) == 1
    # A write the plan does not read: a new stamp, the same parts, the same plan.
    overlay["history"].append({"note": "not read by the plan"})
    overlay["route_modifiers"]["edge"] = {"base_closed": False, "modifiers": {}}
    assert _summary(world) == plain
    assert GEOMETRY.geo_plans_of(world) == 1
    overlay["partitions"]["p-1"] = _partition(world, 0.5)
    walled = _summary(world)
    assert walled["blocked"] != plain["blocked"]
    assert walled["obstacle_count"] == plain["obstacle_count"] + 1
    assert GEOMETRY.geo_plans_of(world) == 2
    del overlay["partitions"]["p-1"]
    assert _summary(world) == plain
    assert GEOMETRY.geo_plans_of(world) == 2
    overlay["facility_points"].update(_moves(world))
    moved = _summary(world)
    assert moved["service_points"] != plain["service_points"] or not plain["service_points"]
    assert GEOMETRY.geo_plans_of(world) == 3
    # Without the overlay, the plan has no constraints but the Scenario's.
    del world.model_state[GEOMETRY_OVERLAY_KEY]
    assert _summary(world) == plain


def test_the_plan_does_not_follow_the_overlay_order(generated_scenario: Any) -> None:
    """Only the order of the facility points follows the overlay, as {**points, **moves}."""
    summaries = []
    for reverse in (False, True):
        world = _world(generated_scenario)
        overlay = _overlay(world)
        moves = list(_moves(world).items())
        partitions = [("p-b", _partition(world, 0.5)), ("p-a", _partition(world, 1.5))]
        if reverse:
            moves.reverse()
            partitions.reverse()
        overlay["facility_points"].update(moves)
        overlay["partitions"].update(partitions)
        summary = _summary(world)
        signed = list(world.model_configs["facility_geometry"]["facility_points"])
        added = [key for key, _ in moves if key not in signed]
        assert [key for key, _ in summary["facility_points"]] == signed + added
        summaries.append(summary)
        # The cache key is the canonical bytes: the other order on the same configs is a hit,
        # and keeps the order the plan was built with.
        overlay["facility_points"] = dict(reversed(list(overlay["facility_points"].items())))
        assert _summary(world) == summary
    first, second = summaries
    assert dict(first.pop("facility_points")) == dict(second.pop("facility_points"))
    assert first == second


def test_a_parse_error_is_raised_again_with_its_class_and_text(generated_scenario: Any) -> None:
    world = _world(generated_scenario)
    expected = [
        (route.route_id, route.travel_minutes)
        for route in parsed_facility_geometry(
            world.model_configs["facility_geometry"]
        ).arrival_approach_routes
    ]
    assert GEOMETRY.approach_routes_of(world) == expected
    required = GEOMETRY.independent_queues_of(world)
    assert isinstance(required, bool)
    world.model_configs["facility_geometry"]["width_m"] = -1.0
    with pytest.raises(ValueError) as python:
        FacilityGeometry.model_validate(world.model_configs["facility_geometry"])
    for read in (GEOMETRY.approach_routes_of, GEOMETRY.independent_queues_of):
        for _ in range(2):
            with pytest.raises(ValueError) as relayed:
                read(world)
            assert type(relayed.value) is type(python.value)
            assert str(relayed.value) == str(python.value)
    # The plan is the Python rule's to refuse.
    with pytest.raises(native.Unsupported, match="the Python rule's parse raises"):
        GEOMETRY.geo_summary_of(world)
    world.model_configs["facility_geometry"]["width_m"] = generated_scenario.model_configs[
        "facility_geometry"
    ]["width_m"]
    assert GEOMETRY.independent_queues_of(world) is required
    # No config: no approach routes to read, and the parse of {} raises.
    world.model_configs.pop("facility_geometry")
    with pytest.raises(ValueError) as python:
        FacilityGeometry.model_validate({})
    with pytest.raises(ValueError) as relayed:
        GEOMETRY.approach_routes_of(world)
    assert str(relayed.value) == str(python.value)
    assert GEOMETRY.independent_queues_of(world) is False


def test_configs_no_mirror_follows_build_a_plan_at_every_read(generated_scenario: Any) -> None:
    world = _world(generated_scenario)
    world.model_configs = copy.deepcopy(dict(world.model_configs))
    first = _summary(world)
    assert _summary(world) == first
    assert GEOMETRY.geo_plans_of(world) == 0


def test_a_run_finds_no_stale_plan(generated_scenario: Any, tmp_path: Any) -> None:
    scenario = generated_scenario.model_copy(update={"duration_minutes": 120})
    sheltersim_native.watch_config_writes(True)
    try:
        run_scenario(scenario, tmp_path)
        assert sheltersim_native.stale_geometry_hits() == 0
    finally:
        sheltersim_native.watch_config_writes(False)
