"""The EffectPrograms of the core's program test (#304): their dump and their issues on worlds.

`native/core/tests/programs.rs` (cargo test) and `native/wasm/tests/node_programs.mjs` (Node) read
each case here: a program's JSON (as an operator or the reading of a text gives it), and, where the
contract takes it, the sha256 of the canonical JSON of `program.model_dump(mode="json")` (the
approval's hash, `approved_program_hash`) and the issues `validate_effect_program` gives on a world
at a minute. The core must give the same dump (`sheltersim_core::program::EffectProgram`) and the
same issues (`program::validate`), or refuse what the contract refuses. `programs_fixture` builds
`programs.json`; `benchmarks/core_fixtures.py programs` writes it.

A program without issues is also scheduled (#305): on a new run of its world at its minute,
`schedule_effect_program` as a fork's `BranchRuntime.apply_effect_program` calls it, with the
`command_log` entry it appends and the world's digests (`core_tick.world_digests`) after it and at
the end of the Run. The core's `PlainRun::apply_effect_program` (WASM
`Simulation.applyEffectProgram`) must give the same; a program with issues is refused with
Python's text and the world left as it was.

The worlds are `small_replay` and `generated` of `tick_core.json` (no floor plan and no
population model; a floor plan where added toilets are placed, a program already scheduled) and
`operations`: a generated Scenario with the gate's outages, absence and shipment
(`world_order.OPERATIONS`), so that a new outage meets pending ones. The programs use the six
operators the offline reading of a text makes (`facility_outage`, `add_staff`, `add_toilet`,
`water_outage`, `add_evacuees`, `depart_evacuees`); a program of another operator of the registry
is refused by the core (`unsupported`), as the browser does not check it.
"""

from __future__ import annotations

import copy
import hashlib
from typing import Any

from sheltersim.core.world_export import export_world
from sheltersim.storage.projects import canonical
from support.core_fixtures import run_world
from support.core_tick import core_tick_run, generated_run

OPERATIONS_EVACUEES = 30

_PRIMITIVES: dict[str, tuple[str, str, list[str]]] = {
    "facility_outage": ("facility", "facility_unit", ["facility_capacity"]),
    "add_staff": ("zone", "staff_member", ["staff_operations"]),
    "add_toilet": ("zone", "toilet_stall", ["facility_capacity"]),
    "water_outage": ("facility", "water_supply", ["operational_events_v1", "resource_service_v1"]),
    "add_evacuees": ("zone", "evacuee", ["population_dynamics_v1"]),
    "depart_evacuees": ("zone", "evacuee", ["population_dynamics_v1"]),
}


def effect(op: str, target: str, start: int, number: int = 1, **fields: Any) -> dict[str, Any]:
    """An effect as the offline reading writes it, with only the fields it sets."""
    kind, units, policies = _PRIMITIVES.get(op, ("zone", "evacuee", []))
    parameters = {
        "facility_outage": {"units": 1},
        "add_staff": {"count": 2},
        "add_toilet": {"count": 1},
        "water_outage": {},
        "add_evacuees": {"count": 5},
        "depart_evacuees": {"count": 3},
    }.get(op, {})
    item: dict[str, Any] = {
        "effect_id": f"effect-{number}-{op}",
        "op": op,
        "target_selector": {"kind": kind, "target_id": target},
        "start_minute": start,
        "parameters": parameters,
        "units": units,
        "source_id": "offline-parser-h06",
        "policy_dependencies": policies,
    }
    item.update(fields)
    return item


def program(*effects: dict[str, Any], start: int | None = None, **fields: Any) -> dict[str, Any]:
    """A program of these effects, as the offline reading writes it (its defaults left out)."""
    item: dict[str, Any] = {
        "program_id": "nl-h06-fixture",
        "narrative": "Offline parser proposal; validate the selected mechanism and target before "
        "approval.",
        "source_text": "Day 1 02:00 toilet_01 outage",
        "simulation_start_minute": min(e["start_minute"] for e in effects)
        if start is None
        else start,
        "effects": list(effects),
        "support_mode": "native",
        "compiler_provenance": {
            "compiler_kind": "offline_rule",
            "compiler_version": "h06-offline-v1",
        },
    }
    item.update(fields)
    return item


def _cases() -> list[tuple[str, str, int, dict[str, Any]]]:
    """(name, world, minute, program JSON)."""
    out = "facility_outage"
    cases: list[tuple[str, str, int, dict[str, Any]]] = [
        ("outage", "generated", 0, program(effect(out, "toilet_01", 120))),
        (
            "outage_full",
            "generated",
            0,
            program(
                effect(out, "toilet_01", 30, end_minute=90, guard=None, reversible=True),
                schema_version=1,
                assumptions=[
                    {
                        "assumption_id": "a1",
                        "description": "仮定",
                        "source_id": "S1",
                        "units": "minute",
                        "value": 1.0,
                    }
                ],
            ),
        ),
        (
            "outage_too_many_units",
            "generated",
            0,
            program(effect(out, "toilet_01", 10, parameters={"units": 5})),
        ),
        ("outage_unknown", "generated", 0, program(effect(out, "toilet_99", 10))),
        (
            "outage_stacked",
            "generated",
            0,
            program(
                effect(out, "toilet_01", 10, 1, end_minute=100),
                effect(out, "toilet_01", 50, 2, end_minute=120),
            ),
        ),
        (
            "outage_meets_pending",
            "operations",
            0,
            program(effect(out, "toilet_01", 60, end_minute=200)),
        ),
        ("outage_before_now", "operations", 10, program(effect(out, "toilet_01", 5), start=5)),
        ("outage_after_end", "operations", 10, program(effect(out, "toilet_01", 400))),
        (
            "outage_end_past_run",
            "operations",
            10,
            program(effect(out, "toilet_01", 20, end_minute=500)),
        ),
        ("staff", "operations", 10, program(effect("add_staff", "sleeping", 20))),
        (
            "staff_bound",
            "operations",
            10,
            program(effect("add_staff", "sleeping", 20, parameters={"count": 100})),
        ),
        ("staff_unknown_zone", "operations", 10, program(effect("add_staff", "nowhere", 20))),
        (
            "staff_one_shot",
            "operations",
            10,
            program(effect("add_staff", "sleeping", 20, end_minute=40)),
        ),
        (
            "toilets",
            "generated",
            0,
            program(effect("add_toilet", "toilet", 30, parameters={"count": 3})),
        ),
        (
            "toilets_many",
            "generated",
            0,
            program(effect("add_toilet", "toilet", 30, parameters={"count": 20})),
        ),
        (
            "toilets_two_zones",
            "operations",
            0,
            program(
                effect("add_toilet", "sleeping", 30, 1, parameters={"count": 2}),
                effect("add_toilet", "corridor", 40, 2, parameters={"count": 2}),
            ),
        ),
        ("toilets_no_plan", "small_replay", 0, program(effect("add_toilet", "toilet", 30))),
        (
            "water_station",
            "operations",
            10,
            program(effect("water_outage", "water_station_02", 20, end_minute=80)),
        ),
        ("water_no_end", "operations", 10, program(effect("water_outage", "water_station_02", 20))),
        (
            "water_not_station",
            "operations",
            10,
            program(effect("water_outage", "toilet_01", 20, end_minute=80)),
        ),
        (
            "water_zone",
            "operations",
            10,
            program(
                effect(
                    "water_outage",
                    "food",
                    20,
                    end_minute=80,
                    target_selector={"kind": "zone", "target_id": "food"},
                )
            ),
        ),
        (
            "water_zone_without_station",
            "operations",
            10,
            program(
                effect(
                    "water_outage",
                    "sleeping",
                    20,
                    end_minute=80,
                    target_selector={"kind": "zone", "target_id": "sleeping"},
                )
            ),
        ),
        (
            "water_small",
            "small_replay",
            0,
            program(effect("water_outage", "toilet_1", 5, end_minute=20)),
        ),
        ("arrivals", "operations", 10, program(effect("add_evacuees", "sleeping", 30))),
        (
            "arrivals_admission",
            "operations",
            10,
            program(
                effect(
                    "add_evacuees",
                    "sleeping",
                    30,
                    parameters={"count": 2, "admission_zone_ids": ["sleeping", "nowhere"]},
                )
            ),
        ),
        (
            "arrivals_route",
            "operations",
            10,
            program(
                effect(
                    "add_evacuees",
                    "sleeping",
                    30,
                    parameters={
                        "count": 2,
                        "approach_route_id": "no-route",
                        "admission_zone_ids": ["quiet"],
                    },
                )
            ),
        ),
        (
            "arrivals_bound",
            "operations",
            10,
            program(
                *(
                    effect("add_evacuees", "sleeping", 30, index, parameters={"count": 1000})
                    for index in range(1, 11)
                )
            ),
        ),
        ("arrivals_no_model", "small_replay", 0, program(effect("add_evacuees", "sleeping", 5))),
        ("departures", "operations", 10, program(effect("depart_evacuees", "sleeping", 30))),
        (
            "departures_no_model",
            "small_replay",
            0,
            program(effect("depart_evacuees", "sleeping", 5)),
        ),
        (
            "mode_clarification",
            "operations",
            0,
            program(effect(out, "toilet_01", 200), support_mode="clarification_required"),
        ),
        (
            "mode_approximate",
            "operations",
            0,
            program(effect(out, "toilet_01", 200), support_mode="approximate"),
        ),
        (
            "wrong_fields",
            "operations",
            0,
            program(
                effect(
                    out,
                    "toilet_01",
                    200,
                    units="evacuee",
                    policy_dependencies=["resource_service_v1", "facility_capacity"],
                    target_selector={"kind": "zone", "target_id": "toilet_01"},
                )
            ),
        ),
        (
            "candidates",
            "operations",
            0,
            program(
                effect(
                    out,
                    "x",
                    200,
                    target_selector={
                        "kind": "facility",
                        "candidate_ids": ["toilet_01", "quiet_support"],
                    },
                )
            ),
        ),
        (
            "guarded",
            "operations",
            0,
            program(
                effect(
                    "add_staff",
                    "sleeping",
                    200,
                    guard={
                        "operator": "and",
                        # One field with an issue: Python reads the fields as a set, whose order
                        # changes between processes.
                        "operands": [
                            {"operator": "gt", "field": "target.capacity", "value": 1},
                            {"operator": "lt", "field": "simulation.minute", "value": 2.5},
                            {"operator": "eq", "field": "simulation.minute", "value": True},
                        ],
                    },
                )
            ),
        ),
        ("unknown_operator", "operations", 0, program(effect("teleport", "sleeping", 200))),
        (
            "already_scheduled",
            "generated",
            0,
            program(effect(out, "toilet_01", 200), program_id="fixture-facility-moves"),
        ),
        ("start_after_run", "operations", 0, program(effect(out, "toilet_01", 400))),
        ("other_operator", "operations", 0, program(effect("add_temporary_space", "sleeping", 20))),
        (
            "parameters_refused",
            "operations",
            0,
            program(effect(out, "toilet_01", 20, parameters={"units": 0})),
        ),
        # The contract refuses these.
        ("no_effects", "operations", 0, {**program(effect(out, "toilet_01", 20)), "effects": []}),
        ("extra_field", "operations", 0, program(effect(out, "toilet_01", 20), extra=1)),
        ("bad_id", "operations", 0, program(effect(out, "toilet 01", 20))),
        ("end_before_start", "operations", 0, program(effect(out, "toilet_01", 20, end_minute=20))),
        (
            "both_targets",
            "operations",
            0,
            program(
                effect(
                    out,
                    "toilet_01",
                    20,
                    target_selector={"kind": "facility", "target_id": "a", "candidate_ids": ["b"]},
                )
            ),
        ),
        (
            "undeclared_assumption",
            "operations",
            0,
            program(effect(out, "toilet_01", 20, assumption_ids=["a9"])),
        ),
        ("starts_early", "operations", 0, program(effect(out, "toilet_01", 20), start=30)),
        (
            "bad_guard",
            "operations",
            0,
            program(
                effect(
                    out,
                    "toilet_01",
                    20,
                    guard={"operator": "gt", "field": "simulation.minute", "value": "x"},
                )
            ),
        ),
        ("bad_mode", "operations", 0, program(effect(out, "toilet_01", 20), support_mode="magic")),
        ("schema_2", "operations", 0, program(effect(out, "toilet_01", 20), schema_version=2)),
    ]
    return cases


def operations_run() -> tuple[Any, Any, Any]:
    """The world of `operations` at minute 0, its controller and its engine."""
    from sheltersim.experiments.initial_scenario import (
        InitialScenarioBuilder,
        InitialScenarioRequest,
    )
    from support.world_order import OPERATIONS

    request = InitialScenarioRequest(
        municipality="合成市",
        disaster="地震",
        evacuees=OPERATIONS_EVACUEES,
        duration_days=0.25,
        seed=2026,
        operational_conditions=OPERATIONS,
    )
    return run_world(InitialScenarioBuilder().build(request).scenario)


def outcome(world: Any, document: dict[str, Any]) -> dict[str, Any]:
    """What Python makes of a program on a world: its dump's sha256 and the issues, or that the
    contract refuses it."""
    from sheltersim.events.effects import EffectProgram, validate_effect_program

    try:
        parsed = EffectProgram.model_validate(copy.deepcopy(document))
    except ValueError:
        return {"refused": True}
    dump = canonical(parsed.model_dump(mode="json")).encode()
    return {
        "dump_sha256": hashlib.sha256(dump).hexdigest(),
        "issues": list(validate_effect_program(world, parsed)),
    }


# The runs of the worlds from minute 0, by name.
RUNS = {"small_replay": core_tick_run, "generated": generated_run}


def scheduled(world_name: str, minute: int, document: dict[str, Any]) -> dict[str, Any]:
    """What `BranchRuntime.apply_effect_program` does with a program that has no issues (#305), on
    a new run of the world at the minute: the `command_log` entry it appends, the world's digests
    after the program is scheduled, and at the end of the world's Run."""
    from sheltersim.events.effects import EffectProgram, schedule_effect_program
    from support.core_tick import world_digests

    world, _, engine = (operations_run if world_name == "operations" else RUNS[world_name])()
    while world.minute_index < minute:
        engine.step()
    receipt = schedule_effect_program(world, EffectProgram.model_validate(copy.deepcopy(document)))
    command = {
        "command": "effect_program",
        "program_id": receipt.program_id,
        "program_hash": receipt.program_hash,
        "event_ids": list(receipt.event_ids),
        "minute": world.minute_index,
    }
    digests = world_digests(world)
    while world.minute_index < world.duration_minutes:
        engine.step()
    return {"command": command, "digests": digests, "end_digests": world_digests(world)}


def programs_fixture() -> dict[str, Any]:
    """`programs.json`: the world `operations` at minute 0 and each case's program and outcome."""
    runs = {
        "small_replay": core_tick_run(),
        "generated": generated_run(),
        "operations": operations_run(),
    }
    entry = export_world(runs["operations"][0], runs["operations"][1])
    cases = []
    for name, world_name, minute, document in sorted(_cases(), key=lambda item: (item[1], item[2])):
        world, _, engine = runs[world_name]
        while world.minute_index < minute:
            engine.step()
        assert world.minute_index == minute, (name, world.minute_index)
        found = outcome(world, document)
        if found.get("issues") == [] and all(
            item["op"] in _PRIMITIVES for item in document["effects"]
        ):
            found["scheduled"] = scheduled(world_name, minute, document)
        unsupported = (
            any(
                item["op"] not in _PRIMITIVES and item["op"] != "teleport"
                for item in document.get("effects", [])
            )
            or name == "parameters_refused"
        )
        cases.append(
            {
                "name": name,
                "world": world_name,
                "minute": minute,
                "program": document,
                # The same as text, for a reader whose JSON writes 1.0 as 1 (Node).
                "program_text": canonical(document),
                **found,
            }
            | ({"unsupported": True} if unsupported and not found.get("refused") else {})
        )
    issues = {case["name"]: case.get("issues") for case in cases}
    for name in ("outage", "staff", "toilets", "water_station", "arrivals", "departures"):
        assert issues[name] == [], (name, issues[name])
    for name, found in issues.items():
        refused = name in {
            "no_effects",
            "extra_field",
            "bad_id",
            "end_before_start",
            "both_targets",
            "undeclared_assumption",
            "starts_early",
            "bad_guard",
            "bad_mode",
            "schema_2",
        }
        assert (found is None) == refused, (name, found)
    assert sum("scheduled" in case for case in cases) >= 10, cases
    return {
        "worlds": {"operations": entry},
        "files": {"small_replay": "tick_core", "generated": "tick_core", "operations": "programs"},
        "cases": cases,
    }
