"""The fixtures of the core's tests of the handlers' texts (Issue #225, lane M1).

A handler's suppressed and failed texts are saved: the dispatcher writes them to the event's record
as `detail`. `native/core/tests/event_texts.rs`, `social_interaction_texts.rs` and
`travel_completion_texts.rs` check that the core's handler (`dispatch::apply_alone`, the kernel the
PyO3 entry of each handler calls) raises each one itself, with the class and the text the Python
rule raised, and changes nothing. They were the cases of the pytest files of the same names, which
wrote the classes and texts down from the Python rules before those went (lane L10); the worlds
and the events are built here as those files built them.

A fixture file is `{"worlds": {name: world}, "cases": {group: [case, ...]}}`, and what a test reads
besides (`canonical_sha256`, the digest of a world's `canonical_json()`; `dispatched`):

- A world is `world_export.export_world`'s entry, or a variant of another world of the same file,
  `{"base": name, "changes": [...]}` (`changes`): a world that Python edited (a person moved, a
  request opened, a namespace taken out), or one of the same Scenario at another minute, written
  as the changes from the world that needs the fewest, to keep the file small.
  `native/core/tests/support/cases.rs` (`resolve`) makes the entry again.
- A case is one event given to one handler of one world: `world`, `handler` (`handler_of`, or
  `table`), the event (`ScheduledEvent.to_dict()`), and either the outcome the Python rule raised,
  `outcome`: `[class, text]` with the class `Suppressed` (EventSuppressed) or `Failed`
  (EventApplicationError), or `after`: the world the Python rule left when the event applied (the
  writes the pytest file made on a copy to compare).
- A group holds the cases of one test function of the pytest file, by its name without `test_`.

The worlds of the gate's generated Scenario have 8 people here, not the 20 of the pytest files, so
that the files stay small; each still reaches the cases those files checked. The contact whose
sender does not know the claim is taken at minute 40 with `claim-true` (`_unaware_contact`).
"""

from __future__ import annotations

import copy
import hashlib
import itertools
import json
import sys
from pathlib import Path
from typing import Any

from sheltersim.core.world_export import export_world

# The people of the gate's generated Scenario in these worlds (the pytest files had 20).
EVACUEES = 8
# Suppressed: the texts the handlers raise as EventSuppressed; the others are EventApplicationError.
SUPPRESSIONS = frozenset(
    {
        "zero-unit facility outage has no effect",
        "zero-unit facility recovery has no effect",
        "staff unavailability was already applied",
        "staff restoration belongs to an older unavailability",
        "contact sender no longer knows the message",
        "contact sender belief changed before transmission",
        "sampled contact basis is no longer current",
        "recipient already has an equally strong belief",
        "staff relief is stale or its replacement is no longer available",
        "social action participants are no longer co-located",
        "family support target is no longer in the same household",
        "the selected person no longer has a higher immediate need",
        "complaint response context is no longer current",
        "complaint response stress candidate is no longer current",
        "cooperation participants or relationship changed",
        "cooperation stress candidate is no longer current",
        "information reaction contact is no longer local",
        "information state is no longer enabled",
        "information claim is no longer current",
        "information claim has already been verified or rejected",
    }
)
_DROP = object()


# ---- The file -----------------------------------------------------------------------------------


def changes(base: Any, made: Any, path: list[Any] | None = None) -> list[Any]:
    """What makes `made` of `base`: `["set", path, value]` (an index one past a list's end appends)
    and `["del", path]` (a key of a dict), in this order."""
    path = path or []
    out: list[Any] = []
    if isinstance(base, dict) and isinstance(made, dict):
        out += [["del", [*path, key]] for key in base if key not in made]
        for key, value in made.items():
            if key in base:
                out += changes(base[key], value, [*path, key])
            else:
                out.append(["set", [*path, key], value])
    elif isinstance(base, list) and isinstance(made, list) and 0 < len(base) <= len(made):
        for index, value in enumerate(made):
            if index < len(base):
                out += changes(base[index], value, [*path, index])
            else:
                out.append(["set", [*path, index], value])
    elif type(base) is not type(made) or base != made:
        out.append(["set", path, made])
    return out


def variant(name: str, base: Any, made: Any, *, dotted: bool = False) -> dict[str, Any]:
    """`made` as a variant of the world (or other value) `name`, which is `base`: its `changes`.
    With `dotted`, each change that sets a value at a path `base` has, or at a new key of a dict,
    is in `set` instead, by its path joined with dots (`"world.agents.3.stress"`), which a file
    written with an indent holds in one line; the others stay in `changes`, which come first."""
    found = changes(base, made)
    if not dotted:
        return {"base": name, "changes": found}
    kept: list[Any] = []
    settable: dict[str, Any] = {}
    for change in found:
        path = change[1]
        if change[0] == "set" and path and _dottable(base, path):
            settable[".".join(str(key) for key in path)] = change[2]
        else:
            kept.append(change)
    out: dict[str, Any] = {"base": name}
    if kept:
        out["changes"] = kept
    if settable:
        out["set"] = settable
    return out


def _dottable(base: Any, path: list[Any]) -> bool:
    """Whether a set at this path can be written with dots: no key has a dot or is all digits,
    and the path ends at a key or an index `base` has (not one past a list's end)."""
    node = base
    for depth, key in enumerate(path):
        last = depth == len(path) - 1
        if isinstance(node, list):
            if not isinstance(key, int) or key >= len(node):
                return False
        elif not isinstance(node, dict) or not isinstance(key, str):
            return False
        elif "." in key or key.isdigit() or (key not in node and not last):
            return False
        if not last:
            node = node[key]
    return True


class Fixture:
    """The worlds and the cases of one file. With `dotted`, a variant writes its changes in the
    shorter form of `variant`."""

    def __init__(self, *, dotted: bool = False) -> None:
        self.dotted = dotted
        self.worlds: dict[str, Any] = {}
        self.entries: dict[str, Any] = {}
        self.cases: dict[str, list[Any]] = {}
        self.canonical: dict[str, str] = {}

    def canonical_digest(self, name: str, world: Any) -> None:
        """Keep the sha256 of the world's `canonical_json()`, which the core's tests write again."""
        self.canonical[name] = hashlib.sha256(world.canonical_json().encode("utf-8")).hexdigest()

    def world(self, name: str, world: Any, controller: Any, *, full: bool = False) -> str:
        """Add a world: whole if `full`, else as the variant of the world added before whose changes
        are the shortest. A world equal to one added is that one."""
        return self.add(name, export_world(world, controller, validate=False), full=full)

    def add(self, name: str, entry: dict[str, Any], *, full: bool = False) -> str:
        """Add an entry of `export_world` (which a builder may have changed), as `world` does."""
        entry = json.loads(json.dumps(entry))
        # Compared as text: `==` takes True for 1 and 1 for 1.0.
        text = json.dumps(entry, sort_keys=True)
        for known, other in self.entries.items():
            if json.dumps(other, sort_keys=True) == text:
                return known
        assert name not in self.entries, name
        if full:
            self.worlds[name] = entry
        else:
            variants = (
                variant(base, other, entry, dotted=self.dotted)
                for base, other in self.entries.items()
            )
            self.worlds[name] = min(variants, key=lambda variant: len(json.dumps(variant)))
        self.entries[name] = entry
        return name

    def says(self, group: str, world: str, handler: Any, event: Any, text: str) -> None:
        """The case of an event the Python rule turned down with `text`."""
        kind = "Suppressed" if text in SUPPRESSIONS else "Failed"
        self._add(group, world, handler, event, outcome=[kind, text])

    def applies(self, group: str, world: str, handler: Any, event: Any, after: str) -> None:
        """The case of an event the Python rule applied, leaving the world `after`."""
        self._add(group, world, handler, event, after=after)

    def _add(self, group: str, world: str, handler: Any, event: Any, **expected: Any) -> None:
        case = {"world": world, "handler": handler, "event": event.to_dict(), **expected}
        self.cases.setdefault(group, []).append(json.loads(json.dumps(case)))

    def document(self) -> dict[str, Any]:
        return {"worlds": self.worlds, "cases": self.cases, "canonical_sha256": self.canonical}


def handler_of(handler: Any) -> dict[str, Any]:
    """A handler made in the test: its class and the values the Run reads of it (`Spec` of the
    core's `run::dispatch`)."""
    kind = type(handler)
    attrs = {
        name: getattr(handler, name)
        for name in ("expire", "cooldown_minutes", "require_colocated", "operation")
        if hasattr(handler, name)
    }
    return {"class": [kind.__module__, kind.__qualname__], "attrs": attrs}


def table(event_type: str, **attrs: Any) -> dict[str, Any]:
    """The controller's handler of this event type, with some values the Run reads replaced."""
    return {"table": event_type, "attrs": attrs}


# ---- Worlds -------------------------------------------------------------------------------------


def _gate() -> Any:
    root = str(Path(__file__).resolve().parents[2] / "benchmarks")
    if root not in sys.path:
        sys.path.insert(0, root)
    import gate_scenarios

    return gate_scenarios


def _run(scenario: Any, minutes: int) -> tuple[Any, Any]:
    from sheltersim.core.engine import TickEngine
    from sheltersim.core.world import WorldState
    from sheltersim.experiments.metrics import MetricsRecorder
    from sheltersim.experiments.runner import OfflineSimulationController
    from sheltersim.experiments.timeline import _MetricSink

    world = WorldState.from_scenario(scenario)
    controller = OfflineSimulationController(world, _MetricSink(MetricsRecorder()))
    engine = TickEngine(world, phase_handler=controller)
    for _ in range(minutes):
        engine.step()
    return world, controller


def _event(world: Any, event_type: str, payload: dict[str, Any], **fields: Any) -> Any:
    from sheltersim.events.models import EventKind, ScheduledEvent

    payload = {key: value for key, value in payload.items() if value is not _DROP}
    return ScheduledEvent(
        event_id=fields.pop("event_id", f"test-{event_type.replace('_', '-')}"),
        kind=fields.pop("kind", EventKind.EXOGENOUS),
        due_minute=fields.pop("due_minute", world.minute_index),
        priority=0,
        sequence=fields.pop("sequence", 900_001),
        event_type=event_type,
        payload=payload,
        source_id=fields.pop("source_id", "SRC-TEST"),
        causation_id=fields.pop("causation_id", None),
        provenance=fields.pop("provenance", {"source_type": "assumption", "source_id": "SRC-TEST"}),
    )


# The operational conditions of the operational world.
CONDITIONS: dict[str, Any] = {
    "scheduled": [
        {"kind": "power_outage", "component_id": "grid-main", "start_minute": 20, "end_minute": 60},
        {
            "kind": "equipment_failure",
            "facility_id": "water_station_main",
            "units_out": 1,
            "start_minute": 30,
            "end_minute": 70,
        },
        {"kind": "extreme_heat", "zone_id": "sleeping", "start_minute": 10, "end_minute": 50},
        {
            "kind": "staff_unavailable",
            "staff_id": "staff_001",
            "start_minute": 15,
            "end_minute": 45,
            "repeat_every_minutes": 40,
        },
        {
            "kind": "resource_delivery",
            "resource_id": "food_meal_units",
            "quantity": 40,
            "due_minute": 60,
        },
    ],
    "initial_resources": {
        "status": "entered",
        "food_meal_units": 20,
        "water_ml": 8_000,
        "source_kind": "operator",
    },
    "infrastructure": {
        "dependency_edges": [
            {"dependent_id": "toilet_01", "dependency_id": "panel-main"},
            {"dependent_id": "panel-main", "dependency_id": "grid-main"},
        ],
        "heat_stress_per_minute": 0.001,
        "cold_stress_per_minute": 0.001,
        "source_id": "GATE-SYNTHETIC-INPUT",
    },
}


def generated_world() -> tuple[Any, Any]:
    """The generated Scenario at minute 5."""
    return _run(_gate().generated(evacuees=EVACUEES, minutes=120), 5)


def operational_world() -> tuple[Any, Any]:
    """At minute 25: the power outage and the heat are active, staff_001 is away until 45."""
    gate = _gate()
    scenario = gate.generated(evacuees=EVACUEES, minutes=120, operational_conditions=CONDITIONS)
    return _run(scenario, 25)


def rumor_world() -> tuple[Any, Any]:
    """At minute 40: three claims, heard by many, no correction yet."""
    gate = _gate()
    return _run(gate.with_rumors(gate.generated(evacuees=EVACUEES, minutes=120)), 40)


# ---- integration/test_event_texts.py ------------------------------------------------------------


def _facility_changes(fixture: Fixture, small: str, world: Any) -> None:
    from sheltersim.events.handlers import FacilityOutageHandler, FacilityRecoveryHandler

    outage, recovery = handler_of(FacilityOutageHandler()), handler_of(FacilityRecoveryHandler())
    facility_id = sorted(world.facilities)[0]
    for handler, field, payload, text in (
        (outage, "units_out", {"facility_id": "F"}, "facility outage payload fields do not match"),
        (
            outage,
            "units_out",
            {"facility_id": 3, "units_out": 1},
            "facility outage requires a facility_id",
        ),
        (
            outage,
            "units_out",
            {"facility_id": "", "units_out": 1},
            "facility outage requires a facility_id",
        ),
        (
            outage,
            "units_out",
            {"facility_id": "F", "units_out": True},
            "facility outage units_out must be an integer",
        ),
        (
            outage,
            "units_out",
            {"facility_id": "F", "units_out": "1"},
            "facility outage units_out must be an integer",
        ),
        (
            outage,
            "units_out",
            {"facility_id": "F", "units_out": -1},
            "facility outage units_out cannot be negative",
        ),
        (
            outage,
            "units_out",
            {"facility_id": "nowhere", "units_out": 1},
            "facility 'nowhere' does not exist",
        ),
        (
            outage,
            "units_out",
            {"facility_id": "F", "units_out": 0},
            "zero-unit facility outage has no effect",
        ),
        (
            outage,
            "units_out",
            {"facility_id": "F", "units_out": 10_000},
            "facility outage exceeds currently working capacity",
        ),
        (
            recovery,
            "units_restored",
            {"facility_id": "F"},
            "facility recovery payload fields do not match",
        ),
        (
            recovery,
            "units_restored",
            {"facility_id": None, "units_restored": 1},
            "facility recovery requires a facility_id",
        ),
        (
            recovery,
            "units_restored",
            {"facility_id": "F", "units_restored": 1.0},
            "facility recovery units_restored must be an integer",
        ),
        (
            recovery,
            "units_restored",
            {"facility_id": "F", "units_restored": -2},
            "facility recovery units_restored cannot be negative",
        ),
        (
            recovery,
            "units_restored",
            {"facility_id": "it's", "units_restored": 1},
            """facility "it's" does not exist""",
        ),
        (
            recovery,
            "units_restored",
            {"facility_id": "F", "units_restored": 0},
            "zero-unit facility recovery has no effect",
        ),
        (
            recovery,
            "units_restored",
            {"facility_id": "F", "units_restored": 10_000},
            "facility recovery exceeds installed capacity",
        ),
    ):
        payload = {key: (facility_id if value == "F" else value) for key, value in payload.items()}
        event_type = "facility_outage" if field == "units_out" else "facility_restored"
        fixture.says(
            "a_facility_change_s_text_is_python_s",
            small,
            handler,
            _event(world, event_type, payload),
            text,
        )


def _outage_under_infrastructure(fixture: Fixture, name: str, world: Any) -> None:
    from sheltersim.events.handlers import FacilityOutageHandler, FacilityRecoveryHandler

    group = "an_outage_under_an_infrastructure_outage_is_python_s_to_fail"
    affected = next(
        record["affected_facility_ids"][0]
        for record in world.model_state["infrastructure_v1"]["outages"].values()
        if record["active"]
    )
    event = _event(world, "facility_outage", {"facility_id": affected, "units_out": 1})
    fixture.says(
        group,
        name,
        handler_of(FacilityOutageHandler()),
        event,
        "facility outage conflicts with an active infrastructure outage",
    )
    event = _event(world, "facility_restored", {"facility_id": affected, "units_restored": 1})
    fixture.says(
        group,
        name,
        handler_of(FacilityRecoveryHandler()),
        event,
        "facility recovery conflicts with an active infrastructure outage",
    )


def _small_cases(fixture: Fixture, small: str, world: Any) -> None:
    """The cases on the small world of single handlers."""
    from sheltersim.events.handlers import (
        EffectProgramPopulationHandler,
        ProximityContactHandler,
        ResourceLedgerHandler,
        StaffSupportCompletionHandler,
    )

    fixture.says(
        "a_proximity_contact_without_its_model_is_python_s_to_fail",
        small,
        handler_of(ProximityContactHandler()),
        _event(world, "proximity_contact_update", {}),
        "proximity contact provenance or time is invalid",
    )
    payload = {
        "warehouse_id": "main",
        "resource_id": "water_ml",
        "quantity": 1,
        "shipment_id": "none",
    }
    fixture.says(
        "a_delivery_of_an_unknown_shipment_is_python_s_to_fail",
        small,
        handler_of(ResourceLedgerHandler("delivery")),
        _event(world, "resource_delivery", payload),
        "shipment delivery does not match operational state",
    )
    group = "population_primitive_texts_are_python_s"
    handler = handler_of(EffectProgramPopulationHandler())
    for payload, text in (
        ({"op": "teleport"}, "population primitive operation is invalid"),
        ({"op": 7}, "population primitive operation is invalid"),
        ({"op": "depart_evacuees"}, "population primitive payload fields do not match"),
        (
            {
                "program_id": "p",
                "effect_id": "e",
                "op": "depart_evacuees",
                "zone_id": "z",
                "count": 1,
            },
            "population primitive is not in the approved program",
        ),
    ):
        fixture.says(group, small, handler, _event(world, "population_primitive", payload), text)
    group = "a_staff_support_completion_s_texts_are_python_s"
    handler = handler_of(StaffSupportCompletionHandler())
    ids = {"request_id": "r", "agent_id": "A001", "staff_id": "staff_001", "task_id": "nothing"}
    for change, text in (
        ({"task_id": ""}, "staff completion requires request, Agent, staff, and task IDs"),
        ({"agent_id": 3}, "staff completion requires request, Agent, staff, and task IDs"),
        ({}, "staff completion does not match an active due task"),
    ):
        event = _event(world, "staff_support_complete", {**ids, **change})
        fixture.says(group, small, handler, event, text)


def _delay_texts(fixture: Fixture, small: str, operational: str, worlds: dict[str, Any]) -> None:
    from sheltersim.events.handlers import DeliveryDelayHandler

    group = "a_delay_s_texts_are_python_s"
    handler = handler_of(DeliveryDelayHandler())
    world = worlds[operational]
    invalid = "delivery delay payload fields or source are invalid"
    fixture.says(
        group,
        operational,
        handler,
        _event(world, "delivery_delayed", {"shipment_id": "x"}),
        invalid,
    )
    event = _event(world, "delivery_delayed", {"shipment_id": "x", "to_minute": 30}, source_id=None)
    fixture.says(group, operational, handler, event, invalid)
    small_world = worlds[small]
    event = _event(small_world, "delivery_delayed", {"shipment_id": "x", "to_minute": 30})
    if "operational_events_v1" not in small_world.model_state:
        fixture.says(
            group, small, handler, event, "delivery delay requires operational_events_v1 state"
        )
    event = _event(world, "delivery_delayed", {"shipment_id": "nothing", "to_minute": 30})
    fixture.says(
        group, operational, handler, event, "delivery delay references an unknown shipment"
    )
    shipment_id = sorted(world.model_state["operational_events_v1"]["shipments"])[0]
    # A Scenario's to_minute is any int: past 64 bits, past the Run's end or before the delivery.
    for to_minute in (True, 26, 10_000, 10**20, -(10**20)):
        payload = {"shipment_id": shipment_id, "to_minute": to_minute}
        fixture.says(
            group,
            operational,
            handler,
            _event(world, "delivery_delayed", payload),
            "shipment delay must move a not-yet-due delivery forward",
        )


def _water_texts(fixture: Fixture, name: str, world: Any) -> None:
    from sheltersim.events.handlers import WaterOutageHandler, WaterRestorationHandler

    group = "water_texts_are_python_s"
    outage = handler_of(WaterOutageHandler())
    restoration = handler_of(WaterRestorationHandler())
    event = _event(world, "water_outage", {"outage_id": "w", "zone_id": "z", "x": 1})
    fixture.says(group, name, outage, event, "water outage payload fields or source are invalid")
    event = _event(world, "water_outage", {"outage_id": "w", "zone_id": "z"})
    fixture.says(group, name, outage, event, "water outage does not match operational state")
    event = _event(world, "water_restored", {"outage_id": "w"}, source_id=None)
    fixture.says(
        group, name, restoration, event, "water restoration payload fields or source are invalid"
    )
    event = _event(world, "water_restored", {"outage_id": "w"})
    fixture.says(group, name, restoration, event, "water restoration does not match its outage")


def _staff_absence_texts(fixture: Fixture, name: str, world: Any) -> None:
    from sheltersim.events.handlers import StaffRestorationHandler, StaffUnavailabilityHandler

    group = "staff_absence_texts_are_python_s"
    absence = handler_of(StaffUnavailabilityHandler())
    event = _event(world, "staff_unavailable", {"staff_id": "staff_001"})
    fixture.says(
        group, name, absence, event, "staff unavailability payload fields or source are invalid"
    )
    event = _event(world, "staff_unavailable", {"staff_id": "nobody", "until_minute": 50})
    fixture.says(group, name, absence, event, "staff unavailability references unknown staff")
    for until in (False, 25, 10_000):
        event = _event(world, "staff_unavailable", {"staff_id": "staff_001", "until_minute": until})
        fixture.says(group, name, absence, event, "staff unavailable until_minute is invalid")
    status = world.model_state["operational_events_v1"]["staff_status"]["staff_001"]
    event = _event(
        world,
        "staff_unavailable",
        {"staff_id": "staff_001", "until_minute": 50},
        event_id=status["unavailability_event_id"],
    )
    fixture.says(group, name, absence, event, "staff unavailability was already applied")

    restoration = handler_of(StaffRestorationHandler())
    event = _event(world, "staff_restored", {"staff_id": "staff_001"})
    fixture.says(
        group, name, restoration, event, "staff restoration payload fields or source are invalid"
    )
    event = _event(world, "staff_restored", {"staff_id": "nobody", "unavailability_event_id": "x"})
    fixture.says(group, name, restoration, event, "staff restoration references unknown staff")
    event = _event(
        world, "staff_restored", {"staff_id": "staff_001", "unavailability_event_id": "old"}
    )
    fixture.says(
        group, name, restoration, event, "staff restoration belongs to an older unavailability"
    )
    payload = {
        "staff_id": "staff_001",
        "unavailability_event_id": status["unavailability_event_id"],
    }
    event = _event(world, "staff_restored", payload, event_id="not-the-token")
    fixture.says(
        group, name, restoration, event, "staff restoration event ID does not match its token"
    )


def _infrastructure_texts(
    fixture: Fixture, small: str, operational: str, worlds: dict[str, Any]
) -> None:
    from sheltersim.events.handlers import InfrastructureEventHandler

    group = "infrastructure_texts_are_python_s"
    handler = handler_of(InfrastructureEventHandler())
    world = worlds[operational]

    def says(event: Any, text: str, name: str = operational) -> None:
        fixture.says(group, name, handler, event, text)

    says(_event(world, "solar_flare", {}), "unsupported infrastructure event type 'solar_flare'")
    says(
        _event(worlds[small], "power_outage", {"outage_id": "o"}),
        "power_outage requires the opted-in infrastructure model",
        small,
    )
    says(
        _event(world, "power_outage", {"outage_id": "nothing"}),
        "infrastructure outage does not match scenario state",
    )
    state = world.model_state["infrastructure_v1"]
    active_id, active = next(
        (key, item) for key, item in state["outages"].items() if item["active"]
    )
    says(
        _event(
            world, active["event_type"], {"outage_id": active_id}, event_id=active["start_event_id"]
        ),
        "infrastructure outage is already active or lacks source",
    )
    other = "equipment_failure" if active["event_type"] == "power_outage" else "power_outage"
    says(
        _event(world, other, {"outage_id": active_id}, event_id=active["start_event_id"]),
        "infrastructure outage event type does not match scenario state",
    )
    says(
        _event(world, "power_restored", {"outage_id": active_id}),
        "infrastructure restoration does not match an active outage",
    )
    says(
        _event(
            world,
            "extreme_heat",
            {"stressor_id": "nothing", "zone_id": "sleeping", "until_minute": 40},
        ),
        "environmental stressor payload or duration is invalid",
    )
    says(
        _event(
            world, "environmental_stressor_end", {"stressor_id": "nothing", "zone_id": "sleeping"}
        ),
        "environmental end event does not match an active stressor",
    )
    says(
        _event(world, "supply_route_interruption", {"outage_id": "nothing"}),
        "supply route interruption payload or duration is invalid",
    )
    says(
        _event(world, "supply_route_restored", {"outage_id": "nothing", "route_id": "r"}),
        "route restoration does not match an active interruption",
    )


def _information_off(fixture: Fixture, small: str, world: Any, controller: Any) -> None:
    from sheltersim.events.handlers import InformationRumorHandler

    world = copy.deepcopy(world)
    world.model_configs["information_spread"] = {
        **world.model_configs["information_spread"],
        "enabled": False,
    }
    name = fixture.world("small_information_off", world, controller)
    fixture.says(
        "an_information_event_without_the_model_is_python_s_to_fail",
        name,
        handler_of(InformationRumorHandler()),
        _event(world, "information_rumor", {}),
        "information spread is not enabled for this run",
    )


def _rumor_texts(fixture: Fixture, name: str, world: Any) -> None:
    from sheltersim.events.handlers import InformationRumorHandler

    handler = handler_of(InformationRumorHandler())
    base = {"message_id": "claim-new", "truth_status": "true", "origin_agent_id": "A001"}
    for change, text, fields in (
        ({"x": 1}, "information_rumor payload fields do not match", {}),
        ({}, "information_rumor requires a source_id", {"source_id": None}),
        ({"message_id": 5}, "information_rumor requires stable message and Agent IDs", {}),
        ({"truth_status": "maybe"}, "information_rumor truth_status is invalid", {}),
        ({"truth_status": None}, "information_rumor truth_status is invalid", {}),
        ({"message_id": "claim-false"}, "information message ID already exists", {}),
        ({"origin_agent_id": "nobody"}, "information_rumor references an unknown Agent", {}),
    ):
        event = _event(world, "information_rumor", {**base, **change}, **fields)
        fixture.says("rumor_texts_are_python_s", name, handler, event, text)


def _correction_texts(fixture: Fixture, name: str, world: Any) -> None:
    from sheltersim.events.handlers import InformationCorrectionHandler

    handler = handler_of(InformationCorrectionHandler())
    base = {"message_id": "claim-false", "zone_id": "sleeping", "belief": "rejected"}
    for change, text, fields in (
        ({"x": 1}, "information_correction payload fields do not match", {}),
        ({}, "information_correction requires a source_id", {"source_id": None}),
        ({"zone_id": 5}, "information_correction requires stable message and Zone IDs", {}),
        ({"belief": "maybe"}, "information_correction belief is invalid", {}),
        ({"belief": "heard"}, "information_correction must verify or reject a claim", {}),
        ({"zone_id": "nowhere"}, "information_correction references an unknown Zone", {}),
        ({"message_id": "nothing"}, "information_correction references an unknown message", {}),
        ({}, "information_correction must reference its rumor event", {"causation_id": "other"}),
        ({"belief": "verified"}, "information correction conflicts with message truth", {}),
    ):
        fields.setdefault("causation_id", "rumor-false")
        event = _event(world, "information_correction", {**base, **change}, **fields)
        fixture.says("correction_texts_are_python_s", name, handler, event, text)


def _contact_basis(world: Any, sender_id: str, recipient_id: str) -> str | None:
    """The basis a contact's rule takes as current: same or open adjacent zone, family first."""
    from sheltersim.domain.population_lifecycle import current_household_id

    sender, recipient = world.agents[sender_id], world.agents[recipient_id]
    same_zone = sender.zone_id == recipient.zone_id
    adjacent = any(
        not edge.closed
        and {edge.from_zone_id, edge.to_zone_id} == {sender.zone_id, recipient.zone_id}
        for edge in world.zone_edges.values()
    )
    if not same_zone and not adjacent:
        return None
    household = current_household_id(world, sender_id)
    if household is not None and household == current_household_id(world, recipient_id):
        return "family"
    return "same_zone" if same_zone else "adjacent_zone"


def _contact(world: Any, sender: str, recipient: str, message_id: str, **change: Any) -> Any:
    from sheltersim.contracts.policies import selected_models
    from sheltersim.events.models import EventKind

    state = world.model_state["information_spread_v1"]
    known = state["beliefs"][sender][message_id]
    payload = {
        "message_id": message_id,
        "sender_agent_id": sender,
        "recipient_agent_id": recipient,
        "belief": known["belief"],
        "contact_basis": _contact_basis(world, sender, recipient),
        "contact_minutes": 5,
    }
    provenance = {
        "source_type": "assumption",
        "source_id": "ASM-INFORMATION-SPREAD-V1",
        "policy_version": selected_models(world.model_configs)["information"],
        "transmission_probability": 0.5,
    }
    fields = {
        "kind": EventKind.ENDOGENOUS,
        "source_id": state["messages"][message_id]["source_id"],
        "causation_id": known["last_event_id"],
        "provenance": provenance,
    }
    for key, value in change.items():
        if key in payload:
            payload[key] = value
        elif key in provenance and key not in fields:
            provenance[key] = value
        else:
            fields[key] = value
    return _event(world, "information_contact_update", payload, **fields)


def _contact_texts(fixture: Fixture, rumor: str, world: Any) -> None:
    from sheltersim.events.handlers import InformationContactHandler
    from sheltersim.events.models import EventKind

    group = "contact_texts_are_python_s"
    handler = handler_of(InformationContactHandler())
    beliefs = world.model_state["information_spread_v1"]["beliefs"]
    message_id = "claim-false"
    pairs = [
        (sender, recipient)
        for sender in sorted(beliefs)
        if message_id in beliefs[sender]
        for recipient in sorted(world.agents)
        if recipient != sender and world.agents[recipient].zone_id == world.agents[sender].zone_id
    ]
    sender, recipient = pairs[0]
    fresh = next((r for s, r in pairs if s == sender and message_id not in beliefs[r]), None)
    heard = next((r for s, r in pairs if s == sender and message_id in beliefs[r]), None)
    cases = [
        ({"contact_minutes": _DROP}, "information_contact_update payload fields do not match"),
        ({"kind": EventKind.EXOGENOUS}, "information contact updates must be endogenous"),
        (
            {"due_minute": world.minute_index + 1},
            "information contact update must run at its due minute",
        ),
        (
            {"source_type": "observed"},
            "information contact provenance is missing its assumption ID",
        ),
        ({"recipient_agent_id": 5}, "information contact requires stable claim and Agent IDs"),
        ({"recipient_agent_id": sender}, "information contact requires two different Agents"),
        ({"recipient_agent_id": "nobody"}, "information contact references unknown state"),
        ({"belief": "maybe"}, "information contact belief is invalid"),
        ({"belief": "verified"}, "contact sender belief changed before transmission"),
        ({"contact_basis": "elsewhere"}, "sampled contact basis is no longer current"),
        ({"contact_minutes": 0}, "information contact duration is below its configured minimum"),
        ({"contact_minutes": True}, "information contact duration is below its configured minimum"),
        ({"transmission_probability": 0}, "information contact probability is invalid"),
        ({"transmission_probability": "x"}, "information contact probability is invalid"),
        ({"source_id": "OTHER"}, "information contact provenance does not match sender state"),
        ({"causation_id": "other"}, "information contact provenance does not match sender state"),
    ]
    for change, text in cases:
        event = _contact(world, sender, fresh or recipient, message_id, **change)
        fixture.says(group, rumor, handler, event, text)
    assert heard is not None
    event = _contact(world, sender, heard, message_id)
    fixture.says(group, rumor, handler, event, "recipient already has an equally strong belief")


def _unaware_contact(fixture: Fixture, rumor: str, world: Any) -> None:
    """A contact whose sender does not know the claim. The pytest file took a world of the same
    Scenario at minute 8, where some did not know `claim-false` yet; here everyone knows it, and
    the claim is `claim-true`, which some do not know yet."""
    from sheltersim.events.handlers import InformationContactHandler

    message_id = "claim-true"
    beliefs = world.model_state["information_spread_v1"]["beliefs"]
    unaware = next(agent for agent in sorted(world.agents) if message_id not in beliefs[agent])
    origin = next(agent for agent in sorted(world.agents) if message_id in beliefs[agent])
    event = _contact(
        world, origin, unaware, message_id, sender_agent_id=unaware, recipient_agent_id=origin
    )
    fixture.says(
        "contact_texts_are_python_s",
        rumor,
        handler_of(InformationContactHandler()),
        event,
        "contact sender no longer knows the message",
    )


def _staff_operation_texts(fixture: Fixture, small: str, world: Any) -> None:
    from sheltersim.domain.staff_operations import (
        STAFF_RELIEF_POLICY_VERSION,
        STAFF_RELIEF_SOURCE_ID,
    )
    from sheltersim.events.handlers import (
        StaffOperationArrivalHandler,
        StaffOperationCompletionHandler,
        StaffOperationReliefHandler,
    )
    from sheltersim.events.models import EventKind

    group = "staff_operation_texts_are_python_s"

    def says(handler: Any, event: Any, text: str) -> None:
        fixture.says(group, small, handler_of(handler), event, text)

    payload = {"staff_id": "staff_001", "task_id": "nothing", "target_zone_id": "sleeping"}
    arrival = StaffOperationArrivalHandler()
    says(
        arrival,
        _event(world, "staff_operation_complete", payload),
        "staff operation arrival handler received another event",
    )
    says(
        arrival,
        _event(world, "staff_operation_arrive", payload),
        "staff travel arrival does not match an active operation",
    )
    says(
        arrival,
        _event(world, "staff_operation_arrive", {**payload, "staff_id": 4}),
        "staff travel arrival does not match an active operation",
    )
    completion = StaffOperationCompletionHandler()
    payload = {**payload, "task_kind": "cleaning", "target_id": None}
    says(
        completion,
        _event(world, "staff_operation_arrive", payload),
        "staff operation completion handler received another event",
    )
    says(
        completion,
        _event(world, "staff_operation_complete", payload),
        "staff operation completion does not match active work",
    )
    relief = StaffOperationReliefHandler()
    payload = {
        "task_id": "nothing",
        "previous_staff_id": "staff_001",
        "replacement_staff_id": "staff_002",
        "reason_code": "x",
    }
    says(
        relief,
        _event(world, "staff_operation_relief", payload),
        "staff operation relief provenance or payload is invalid",
    )
    provenance = {
        "source_type": "assumption",
        "source_id": STAFF_RELIEF_SOURCE_ID,
        "policy_version": STAFF_RELIEF_POLICY_VERSION,
    }
    event = _event(
        world,
        "staff_operation_relief",
        payload,
        kind=EventKind.ENDOGENOUS,
        source_id=STAFF_RELIEF_POLICY_VERSION,
        provenance=provenance,
    )
    says(relief, event, "staff relief is stale or its replacement is no longer available")


def _facility_modifier_texts(fixture: Fixture, small: str, world: Any) -> None:
    from sheltersim.events.effects import EffectProgramFacilityHandler

    facility_id = sorted(world.facilities)[0]
    base = {"program_id": "p", "effect_id": "e", "modifier_id": "m", "facility_id": facility_id}
    for handler, payload, text in (
        (EffectProgramFacilityHandler(), base, "effect program apply payload fields do not match"),
        (
            EffectProgramFacilityHandler(expire=True),
            {**base, "working_units": 1},
            "effect program expire payload fields do not match",
        ),
        (
            EffectProgramFacilityHandler(),
            {**base, "working_units": 1},
            "effect program event does not reference a scheduled effect",
        ),
        (
            EffectProgramFacilityHandler(expire=True),
            base,
            "effect program event does not reference a scheduled effect",
        ),
    ):
        fixture.says(
            "an_effect_s_facility_modifier_texts_are_python_s",
            small,
            handler_of(handler),
            _event(world, "facility_availability", payload),
            text,
        )


def _loop_texts(fixture: Fixture, small: str, world: Any) -> None:
    """The dispatch loop's own handlers (the controller's): a walk, a toilet visit, a timeout."""
    group = "the_loop_s_handlers_texts_are_python_s"
    walk = {
        "agent_id": "A001",
        "task_id": "nothing",
        "destination_zone_id": "sleeping",
        "purpose": "return",
    }
    for change, text in (
        ({"purpose": ""}, "travel completion requires Agent, task, destination, and purpose"),
        ({"agent_id": None}, "travel completion requires Agent, task, destination, and purpose"),
        ({}, "travel completion does not match an active task"),
    ):
        event = _event(world, "agent_travel_complete", {**walk, **change})
        fixture.says(group, small, table("agent_travel_complete"), event, text)
    visit = {"facility_id": "toilet_01", "agent_id": "A001", "task_id": "nothing"}
    for change, text in (
        ({"task_id": ""}, "toilet completion requires facility_id, agent_id, and task_id"),
        ({}, "toilet completion does not match an active due task"),
    ):
        event = _event(world, "toilet_service_complete", {**visit, **change})
        fixture.says(group, small, table("toilet_service_complete"), event, text)
    for payload, text in (
        ({"request_id": 3, "agent_id": "A001"}, "support timeout requires request_id and agent_id"),
        (
            {"request_id": "nothing", "agent_id": "A001"},
            "support request 'nothing' does not match its Agent",
        ),
    ):
        event = _event(world, "support_request_timeout", payload)
        fixture.says(group, small, table("support_request_timeout"), event, text)


def _typed_social_first_texts(fixture: Fixture, small: str, world: Any) -> None:
    from sheltersim.events.handlers import SocialInteractionHandler
    from sheltersim.events.models import EventKind

    group = "a_typed_social_event_s_first_texts_are_python_s"
    handler = handler_of(SocialInteractionHandler())
    provenance = {
        "source_id": "ASM-EVENT-TRIGGERED-SOCIAL-INTERACTIONS-V1",
        "source_type": "assumption",
        "policy_version": "social-v1",
    }

    def typed(payload: dict[str, Any], **fields: Any) -> Any:
        fields = {
            "kind": EventKind.ENDOGENOUS,
            "source_id": "social-v1",
            "provenance": provenance,
            **fields,
        }
        return _event(world, "social_interaction", payload, **fields)

    for interaction in (
        "reassurance_exchange",
        "complaint_response",
        "cooperation",
        "information_reaction",
    ):
        payload = {
            "interaction_type": interaction,
            "agent_id": "A001",
            "participant_agent_id": "A002",
        }
        for fields in (
            {"kind": EventKind.EXOGENOUS},
            {"due_minute": world.minute_index + 1},
            {"source_id": "other"},
        ):
            fixture.says(
                group,
                small,
                handler,
                typed(payload, **fields),
                "typed social interaction provenance or time is invalid",
            )
    payload = {"interaction_type": "cooperation", "agent_id": "A001"}
    fixture.says(
        group, small, handler, typed(payload), "typed social interaction requires a reason code"
    )
    for agent in ("nobody", 7):
        payload = {"interaction_type": "cooperation", "agent_id": agent, "reason_short": "x"}
        fixture.says(
            group,
            small,
            handler,
            typed(payload),
            "typed social interaction references an unknown Agent",
        )


def _outage_later_texts(fixture: Fixture, name: str, world: Any) -> None:
    from sheltersim.events.handlers import InfrastructureEventHandler

    group = "an_outage_s_later_texts_are_python_s"
    handler = handler_of(InfrastructureEventHandler())
    outages = world.model_state["infrastructure_v1"]["outages"]
    waiting_id, waiting = next(
        (key, item) for key, item in sorted(outages.items()) if not item["active"]
    )
    kind = waiting["event_type"]
    target = "component_id" if kind == "power_outage" else "facility_id"
    base = {"outage_id": waiting_id, target: waiting.get("target_id", "x"), "until_minute": 100}
    if kind != "power_outage":
        base["units_out"] = waiting.get("units_out") or 1
    start = {"event_id": waiting["start_event_id"]}
    for payload in (
        {"outage_id": waiting_id},
        {**base, "until_minute": world.minute_index},
        {**base, "until_minute": True},
    ):
        event = _event(world, kind, payload, **start)
        fixture.says(
            group, name, handler, event, "infrastructure outage payload or duration is invalid"
        )
    event = _event(world, kind, {**base, target: "nowhere"}, **start)
    fixture.says(
        group, name, handler, event, "infrastructure outage has no valid affected facilities"
    )
    active_id, active = next((key, item) for key, item in sorted(outages.items()) if item["active"])
    wrong = "equipment_repaired" if active["event_type"] == "power_outage" else "power_restored"
    event = _event(
        world,
        wrong,
        {"outage_id": active_id},
        event_id=active["restore_event_id"],
        causation_id=active["start_event_id"],
    )
    fixture.says(
        group, name, handler, event, "infrastructure restoration type does not match its cause"
    )


# ---- integration/test_social_interaction_texts.py -----------------------------------------------

SOCIAL_SOURCE = "ASM-EVENT-TRIGGERED-SOCIAL-INTERACTIONS-V1"
UNKNOWN_AGENT = "typed social interaction references an unknown Agent"
NO_REASON = "typed social interaction requires a reason code"
CONTEXT = "social dialogue state context is invalid"


class _Social:
    """The social cases of one file: its fixture, the worlds' controllers, and the log records'
    sequences, which go on from one test to the next as in the pytest file."""

    def __init__(self, fixture: Fixture, sequences: itertools.count) -> None:
        from sheltersim.events.handlers import SocialInteractionHandler

        self.fixture = fixture
        self.sequences = sequences
        self.handler = handler_of(SocialInteractionHandler())

    def says(self, group: str, name: str, event: Any, text: str) -> None:
        self.fixture.says(group, name, self.handler, event, text)

    def applies(self, group: str, name: str, event: Any, after: str) -> None:
        self.fixture.applies(group, name, self.handler, event, after)

    def contact(self, world: Any, sender: str, recipient: str, **fields: Any) -> str:
        """An applied information contact a minute ago, which cooperation and reactions answer."""
        from sheltersim.events.models import EventKind, EventLogRecord, EventStatus, ScheduledEvent

        event_id = fields.pop("event_id", "contact-test")
        minute = fields.pop("minute", world.minute_index - 1)
        contact = ScheduledEvent(
            event_id=event_id,
            kind=EventKind.ENDOGENOUS,
            due_minute=minute,
            priority=0,
            sequence=next(self.sequences),
            event_type=fields.pop("event_type", "information_contact_update"),
            payload={"sender_agent_id": sender, "recipient_agent_id": recipient},
            source_id="SRC-TEST",
            provenance={"source_type": "assumption", "source_id": "SRC-TEST"},
        )
        status = fields.pop("status", EventStatus.APPLIED)
        world.event_log.append(EventLogRecord(event=contact, realized_minute=minute, status=status))
        return event_id


def _social(world: Any, payload: dict[str, Any], **fields: Any) -> Any:
    from sheltersim.events.models import EventKind, ScheduledEvent

    payload = {key: value for key, value in payload.items() if value is not _DROP}
    return ScheduledEvent(
        event_id=fields.pop("event_id", "social-test"),
        kind=fields.pop("kind", EventKind.ENDOGENOUS),
        due_minute=fields.pop("due_minute", world.minute_index),
        priority=0,
        sequence=900_001,
        event_type="social_interaction",
        payload=payload,
        source_id=fields.pop("source_id", "social-v1"),
        causation_id=fields.pop("causation_id", None),
        provenance=fields.pop(
            "provenance",
            {
                "source_id": SOCIAL_SOURCE,
                "source_type": "assumption",
                "policy_version": "social-v1",
            },
        ),
    )


def _relief(world: Any, actor: str) -> float:
    """The Python rule's relief of a social action: the transfer times the actor's affect signal."""
    state = world.model_state.get("body_state_v1", {})
    body = state.get("agents", {}).get(actor, {})
    signal = min(
        1.0, max(-1.0, body.get("positive_affect", 0.5) - body.get("negative_affect", 0.0))
    )
    transfer = state.get("config", {}).get("social_action_affect_transfer", 0.02)
    return transfer * signal


def _pair(world: Any) -> tuple[str, str]:
    """Two people in one zone and one household."""
    first, second = sorted(world.agents)[:2]
    world.agents[second].zone_id = world.agents[first].zone_id
    _household(world, first, "h1")
    _household(world, second, "h1")
    return first, second


def _household(world: Any, agent_id: str, household: str) -> None:
    presence = world.model_state.get("population_presence_v1")
    if presence is not None:
        presence["people"].setdefault(agent_id, {})["household_id"] = household
    else:
        world.agents[agent_id].profile.group_id = household


def _other_zone(world: Any, zone: str) -> str:
    return next(name for name in sorted(world.zones) if name != zone)


def _open_request(world: Any, agent_id: str, *, age: int) -> str:
    from sheltersim.domain.models import Activity, SupportRequestState

    for key in list(world.active_tasks):
        if world.active_tasks[key].agent_id == agent_id:
            del world.active_tasks[key]
    agent = world.agents[agent_id]
    agent.activity = Activity.SEEKING_HELP
    agent.target_id = None
    agent.support_request_id = "support-t1"
    world.support_requests["support-t1"] = SupportRequestState(
        "support-t1", agent_id, world.minute_index - age, world.minute_index + 50
    )
    return "support-t1"


ACTIONS = {
    "reassurance_exchange": ("offer_reassurance", "emotional_check_in"),
    "family_care": ("provide_family_support", "family_care"),
    "voluntary_support": ("offer_practical_help", "mutual_aid"),
}
FIELDS = "autonomous social action payload fields do not match"
APART = "social action participants are no longer co-located"
TOPIC = "social action does not match its typed topic"
REASON = "social action reason code is not allowed"
DURATION = "social action duration must be within 1..1440 minutes"
HOUSEHOLD = "family support target is no longer in the same household"
NEED = "the selected person no longer has a higher immediate need"


def _action(world: Any, kind: str, actor: str, participant: str, **change: Any) -> Any:
    action, topic = ACTIONS[kind]
    payload = {
        "interaction_type": kind,
        "agent_id": actor,
        "participant_agent_id": participant,
        "zone_id": world.agents[actor].zone_id,
        "topic_id": topic,
        "action": action,
        "reason_short": "autonomous_need_choice",
        "interaction_duration_minutes": 5,
    }
    return _social(world, {**payload, **change})


def _context(world: Any, ids: list[str], **change: Any) -> dict[str, Any]:
    """A captured context, with some participant values replaced."""
    from sheltersim.domain.social_dialogue import capture_social_state_context

    context = copy.deepcopy(capture_social_state_context(world, ids))
    for path, value in change.items():
        person, *keys = path.split(".")
        target = context["participants"][ids[int(person)]]
        for key in keys[:-1]:
            target = target[key]
        target[keys[-1]] = value
    return context


def _untyped_texts(social: _Social, small: str, base: Any, controller: Any) -> None:
    world = copy.deepcopy(base)
    owner, other = sorted(world.agents)[:2]
    request_id = _open_request(world, owner, age=0)
    name = social.fixture.world("small_request_open", world, controller)
    request = {"agent_id": owner, "request_id": request_id, "action": "offer_reassurance"}
    ids = "social interaction requires Agent, request, and action IDs"
    unknown = "social interaction references an unknown request or Agent"
    for change, text in (
        ({"agent_id": _DROP}, ids),
        ({"agent_id": "", "request_id": "nothing"}, ids),
        ({"request_id": 5}, ids),
        ({"action": None}, ids),
        ({"action": ""}, ids),
        ({"request_id": "nothing", "action": "dance"}, unknown),
        ({"agent_id": other}, unknown),
        ({"agent_id": "nobody"}, unknown),
        ({"action": "dance"}, "social interaction action is not allowed"),
        ({"interaction_type": None, "action": "dance"}, "social interaction action is not allowed"),
    ):
        social.says(
            "an_untyped_social_event_s_texts_are_python_s",
            name,
            _social(world, {**request, **change}),
            text,
        )


def _action_texts(social: _Social, small: str, base: Any, controller: Any) -> None:
    group = "a_social_action_s_texts_come_in_python_s_order"
    world = copy.deepcopy(base)
    actor, participant = _pair(world)
    name = social.fixture.world("small_pair", world, controller)
    zone = world.agents[actor].zone_id
    both = _context(world, [actor, participant])
    alone = _context(world, [actor])

    def changed(**change: Any) -> dict[str, Any]:
        return _context(world, [actor, participant], **change)

    stale = {**both, "captured_minute": world.minute_index + 1}
    cases: list[tuple[dict[str, Any], str]] = [
        # The reason code and the Agent come before everything else.
        ({"reason_short": None, "agent_id": "nobody"}, NO_REASON),
        ({"reason_short": _DROP, "extra": 1}, NO_REASON),
        ({"agent_id": "nobody"}, UNKNOWN_AGENT),
        ({"agent_id": "nobody", "extra": 1}, UNKNOWN_AGENT),
        (
            {"agent_id": "nobody", "conversation_state_context": {"schema_version": 2}},
            UNKNOWN_AGENT,
        ),
        ({"agent_id": 5, "action": "other"}, UNKNOWN_AGENT),
        ({"agent_id": _DROP}, UNKNOWN_AGENT),
        # Then the dialogue context.
        ({"conversation_state_context": {"schema_version": 2}, "extra": 1}, CONTEXT),
        ({"conversation_state_context": "x", "participant_agent_id": "nobody"}, CONTEXT),
        ({"conversation_state_context": None}, CONTEXT),
        ({"conversation_state_context": [1]}, CONTEXT),
        ({"conversation_state_context": stale}, CONTEXT),
        ({"conversation_state_context": {**both, "schema_version": True}, "action": "x"}, TOPIC),
        ({"conversation_state_context": {**both, "schema_version": 1.0}, "action": "x"}, TOPIC),
        ({"conversation_state_context": {**both, "schema_version": "1"}}, CONTEXT),
        ({"conversation_state_context": {**both, "captured_minute": 1.0}}, CONTEXT),
        ({"conversation_state_context": {**both, "captured_minute": -1}}, CONTEXT),
        ({"conversation_state_context": {**both, "participants": []}}, CONTEXT),
        ({"conversation_state_context": {**both, "extra": 1}}, CONTEXT),
        ({"conversation_state_context": alone}, CONTEXT),
        ({"conversation_state_context": changed(**{"0.stress": 2.0})}, CONTEXT),
        ({"conversation_state_context": changed(**{"0.stress": True})}, CONTEXT),
        ({"conversation_state_context": changed(**{"0.stress": "0.5"})}, CONTEXT),
        ({"conversation_state_context": changed(**{"1.trust": None})}, CONTEXT),
        ({"conversation_state_context": changed(**{"1.needs": {}})}, CONTEXT),
        ({"conversation_state_context": changed(**{"1.needs.hunger": 1.5})}, CONTEXT),
        ({"conversation_state_context": changed(**{"1.needs.hunger": 2})}, CONTEXT),
        ({"conversation_state_context": changed(**{"0.affect": {"x": 0.1}})}, CONTEXT),
        ({"conversation_state_context": changed(**{"0.affect": 0.5})}, CONTEXT),
        # A context Python accepts on its slow path goes on to the later checks.
        ({"conversation_state_context": changed(**{"0.stress": 1}), "reason_short": "x"}, REASON),
        (
            {
                "conversation_state_context": changed(**{"1.needs.hunger": 0}),
                "interaction_duration_minutes": 0,
            },
            DURATION,
        ),
        ({"conversation_state_context": changed(**{"0.affect": None}), "action": "x"}, TOPIC),
        # Its participants: the actor, and the other person only if that is a str.
        ({"conversation_state_context": alone, "participant_agent_id": 5}, APART),
        ({"conversation_state_context": alone, "participant_agent_id": actor}, APART),
        ({"conversation_state_context": both, "participant_agent_id": 5}, CONTEXT),
        # Then the payload's fields.
        ({"extra": 1, "action": "other"}, FIELDS),
        ({"topic_id": _DROP}, FIELDS),
        ({"interaction_duration_minutes": _DROP, "reason_short": "x"}, FIELDS),
        ({"participant_agent_id": _DROP}, FIELDS),
        ({"conversation_state_context": both, "zone_id": _DROP}, FIELDS),
        # Then the two people.
        ({"participant_agent_id": "nobody", "action": "other"}, APART),
        ({"participant_agent_id": actor, "reason_short": "x"}, APART),
        ({"participant_agent_id": 5}, APART),
        ({"zone_id": _other_zone(world, zone), "action": "other"}, APART),
        ({"zone_id": None}, APART),
        # Then the action and topic, the reason code and the duration.
        ({"action": "other", "reason_short": "x"}, TOPIC),
        ({"topic_id": "mutual_aid", "interaction_duration_minutes": 0}, TOPIC),
        ({"action": 5}, TOPIC),
        ({"action": None}, TOPIC),
        ({"reason_short": "x", "interaction_duration_minutes": 0}, REASON),
        ({"reason_short": 5}, REASON),
        ({"interaction_duration_minutes": 0}, DURATION),
        ({"interaction_duration_minutes": 1441}, DURATION),
        ({"interaction_duration_minutes": True}, DURATION),
        ({"interaction_duration_minutes": 5.0}, DURATION),
        ({"interaction_duration_minutes": "5"}, DURATION),
        ({"interaction_duration_minutes": -(2**70)}, DURATION),
    ]
    for change, text in cases:
        social.says(
            group, name, _action(world, "reassurance_exchange", actor, participant, **change), text
        )
    # The Python rule's writes of a reassurance exchange: the participant is relieved.
    relief = _relief(world, actor)
    assert relief != 0.0
    twin = copy.deepcopy(world)
    person = twin.agents[participant]
    person.stress = min(1.0, max(0.0, person.stress - relief))
    person.trust = min(1.0, max(0.0, person.trust + relief * 0.5))
    after = social.fixture.world("small_pair_reassured", twin, controller)
    social.applies(group, name, _action(world, "reassurance_exchange", actor, participant), after)
    event = _action(
        world, "reassurance_exchange", actor, participant, conversation_state_context=both
    )
    social.applies(group, name, event, after)


def _moved_texts(social: _Social, small: str, base: Any, controller: Any) -> None:
    world = copy.deepcopy(base)
    actor, participant = _pair(world)
    world.agents[participant].zone_id = _other_zone(world, world.agents[actor].zone_id)
    name = social.fixture.world("small_pair_apart", world, controller)
    for kind in ACTIONS:
        social.says(
            "a_social_action_whose_participant_moved_is_suppressed_first",
            name,
            _action(world, kind, actor, participant, reason_short="x"),
            APART,
        )


def _family_texts(social: _Social, small: str, base: Any, controller: Any) -> None:
    group = "family_care_and_voluntary_support_texts_come_after_the_duration"
    world = copy.deepcopy(base)
    actor, participant = _pair(world)
    _household(world, participant, "h2")
    name = social.fixture.world("small_pair_households", world, controller)
    social.says(group, name, _action(world, "family_care", actor, participant), HOUSEHOLD)
    event = _action(world, "family_care", actor, participant, interaction_duration_minutes=0)
    social.says(group, name, event, DURATION)
    world.agents[actor].stress = 0.9
    world.agents[participant].stress = 0.1
    for need in ("toilet_urgency", "hunger", "fatigue", "hydration_need"):
        setattr(world.agents[participant].needs, need, 0.1)
    name = social.fixture.world("small_pair_households_calm", world, controller)
    social.says(group, name, _action(world, "voluntary_support", actor, participant), NEED)
    event = _action(world, "voluntary_support", actor, participant, action="x")
    social.says(group, name, event, TOPIC)


def _complaint_texts(social: _Social, small: str, base: Any, controller: Any) -> None:
    from sheltersim.domain.models import SupportRequestStatus

    group = "complaint_response_texts_are_python_s"
    world = copy.deepcopy(base)
    owner, other = sorted(world.agents)[:2]
    request_id = _open_request(world, owner, age=4)
    world.agents[owner].stress = 0.5
    name = social.fixture.world("small_request_waited", world, controller)
    zone = world.agents[owner].zone_id
    request = {
        "interaction_type": "complaint_response",
        "agent_id": owner,
        "request_id": request_id,
        "zone_id": zone,
        "stress_threshold": 0.1,
        "complaint_wait_minutes": 3,
        "action": "acknowledge_complaint",
        "reason_short": "long_wait",
    }
    fields = "complaint response payload fields do not match"
    thresholds = "complaint response thresholds are invalid"
    stale = "complaint response context is no longer current"
    calm = "complaint response stress candidate is no longer current"
    reason = "complaint response reason code is not allowed"
    action = "complaint response action is not allowed"
    cases: list[tuple[dict[str, Any], dict[str, Any], str]] = [
        ({"agent_id": "nobody", "extra": 1}, {}, UNKNOWN_AGENT),
        ({"extra": 1, "stress_threshold": 2}, {}, fields),
        ({"complaint_wait_minutes": _DROP}, {}, fields),
        ({"participant_agent_id": other}, {}, fields),
        ({"stress_threshold": 2, "request_id": "nothing"}, {}, thresholds),
        ({"stress_threshold": -0.1}, {}, thresholds),
        ({"stress_threshold": True}, {}, thresholds),
        ({"stress_threshold": "0.5"}, {}, thresholds),
        ({"stress_threshold": None}, {}, thresholds),
        ({"complaint_wait_minutes": 0}, {}, thresholds),
        ({"complaint_wait_minutes": 2.5}, {}, thresholds),
        ({"complaint_wait_minutes": True}, {}, thresholds),
        ({"request_id": "nothing", "reason_short": "x"}, {}, stale),
        ({"request_id": 5}, {}, stale),
        ({"agent_id": other}, {}, stale),
        ({"zone_id": _other_zone(world, zone)}, {}, stale),
        ({}, {"causation_id": "other"}, stale),
        ({"complaint_wait_minutes": 10}, {}, stale),
        ({"stress_threshold": 0.9, "reason_short": "x"}, {}, calm),
        ({"stress_threshold": 1}, {}, calm),
        ({"reason_short": "x", "action": "dance"}, {}, reason),
        ({"reason_short": 5}, {}, reason),
        ({"action": "dance"}, {}, action),
        ({"action": None}, {}, action),
    ]
    for change, event_fields, text in cases:
        event_fields = {"causation_id": request_id, **event_fields}
        social.says(group, name, _social(world, {**request, **change}, **event_fields), text)
    world.support_requests[request_id].status = SupportRequestStatus.RESOLVED
    name = social.fixture.world("small_request_resolved", world, controller)
    social.says(group, name, _social(world, request, causation_id=request_id), stale)


def _cooperation_texts(social: _Social, small: str, base: Any, controller: Any) -> None:
    from sheltersim.events.models import EventStatus

    group = "cooperation_texts_are_python_s"
    world = copy.deepcopy(base)
    actor, participant = _pair(world)
    world.agents[actor].stress = 0.5
    world.agents[participant].stress = 0.6
    zone = world.agents[actor].zone_id
    trigger = social.contact(world, actor, participant)
    reverse = social.contact(world, participant, actor, event_id="contact-reverse")
    early = social.contact(world, actor, participant, event_id="contact-early", minute=0)
    failed = social.contact(
        world, actor, participant, event_id="contact-failed", status=EventStatus.FAILED
    )
    name = social.fixture.world("small_pair_contacts", world, controller)
    request = {
        "interaction_type": "cooperation",
        "agent_id": actor,
        "participant_agent_id": participant,
        "zone_id": zone,
        "stress_threshold": 0.1,
        "relationship": "same_group",
        "trigger_event_id": trigger,
        "action": "offer_mutual_support",
        "reason_short": "shared_group_contact",
    }
    fields = "cooperation payload fields do not match"
    threshold = "cooperation stress threshold is invalid"
    two = "cooperation requires two different known Agents"
    changed = "cooperation participants or relationship changed"
    contact = "cooperation does not match an applied local contact"
    calm = "cooperation stress candidate is no longer current"
    reason = "cooperation reason code is not allowed"
    action = "cooperation action is not allowed"
    cases: list[tuple[dict[str, Any], dict[str, Any], str]] = [
        ({"extra": 1, "stress_threshold": 2}, {}, fields),
        ({"relationship": _DROP}, {}, fields),
        ({"stress_threshold": 1.5, "participant_agent_id": "nobody"}, {}, threshold),
        ({"stress_threshold": None}, {}, threshold),
        ({"stress_threshold": False}, {}, threshold),
        ({"participant_agent_id": "nobody", "zone_id": "x"}, {}, two),
        ({"participant_agent_id": actor}, {}, two),
        ({"participant_agent_id": 5}, {}, two),
        ({"zone_id": "x", "trigger_event_id": "other"}, {}, changed),
        ({"relationship": "friends"}, {}, changed),
        ({"relationship": None}, {}, changed),
        ({}, {"causation_id": "other"}, changed),
        ({"trigger_event_id": 5}, {}, changed),
        ({"trigger_event_id": "other", "reason_short": "x"}, {"causation_id": "other"}, contact),
        ({"trigger_event_id": reverse}, {"causation_id": reverse}, contact),
        ({"trigger_event_id": early}, {"causation_id": early}, contact),
        ({"trigger_event_id": failed}, {"causation_id": failed}, contact),
        ({"stress_threshold": 0.95, "reason_short": "x"}, {}, calm),
        ({"reason_short": "x", "action": "dance"}, {}, reason),
        ({"reason_short": None}, {}, NO_REASON),
        ({"action": "dance"}, {}, action),
        ({"action": 5}, {}, action),
    ]
    for change, event_fields, text in cases:
        payload = {**request, **change}
        event_fields = {"causation_id": payload.get("trigger_event_id"), **event_fields}
        if not isinstance(event_fields["causation_id"], str):
            event_fields["causation_id"] = trigger
        social.says(group, name, _social(world, payload, **event_fields), text)
    moved = copy.deepcopy(world)
    moved.agents[participant].zone_id = _other_zone(moved, zone)
    moved_name = social.fixture.world("small_pair_contacts_apart", moved, controller)
    social.says(group, moved_name, _social(moved, request, causation_id=trigger), changed)
    other = copy.deepcopy(world)
    _household(other, participant, "h2")
    other_name = social.fixture.world("small_pair_contacts_households", other, controller)
    social.says(group, other_name, _social(other, request, causation_id=trigger), changed)
    # The Python rule's writes of a cooperation: both are a little calmer and trust a little more.
    twin = copy.deepcopy(world)
    for person in (twin.agents[actor], twin.agents[participant]):
        person.stress = max(0.0, person.stress - 0.02)
        person.trust = min(1.0, person.trust + 0.01)
    after = social.fixture.world("small_pair_cooperated", twin, controller)
    social.applies(group, name, _social(world, request, causation_id=trigger), after)


def _reaction_texts(social: _Social, rumor: str, base: Any, controller: Any) -> None:
    from sheltersim.domain.models import InformationSpreadConfig

    group = "information_reaction_texts_are_python_s"
    world = copy.deepcopy(base)
    state = world.model_state["information_spread_v1"]
    assert state["messages"]["claim-false"]["truth_status"] == "false"
    listener = next(
        agent
        for agent in sorted(state["beliefs"])
        if state["beliefs"][agent].get("claim-false", {}).get("belief") == "heard"
    )
    speaker = next(agent for agent in sorted(world.agents) if agent != listener)
    world.agents[speaker].zone_id = world.agents[listener].zone_id
    zone = world.agents[listener].zone_id
    trigger = social.contact(world, speaker, listener)
    reverse = social.contact(world, listener, speaker, event_id="contact-reverse")
    early = social.contact(world, speaker, listener, event_id="contact-early", minute=0)
    other_type = social.contact(
        world, speaker, listener, event_id="contact-other", event_type="proximity_contact"
    )
    name = social.fixture.world("rumor_heard_contacts", world, controller)
    settled = next(
        (
            message
            for message, belief in sorted(state["beliefs"][listener].items())
            if message != "claim-false"
        ),
        None,
    )
    assert settled is not None
    request = {
        "interaction_type": "information_reaction",
        "agent_id": listener,
        "participant_agent_id": speaker,
        "zone_id": zone,
        "message_id": "claim-false",
        "trigger_event_id": trigger,
        "action": "seek_claim_verification",
        "reason_short": "heard_through_contact",
    }
    fields = "information reaction payload fields do not match"
    two = "information reaction requires two known Agents"
    local = "information reaction contact is no longer local"
    contact = "information reaction does not match an applied contact"
    current = "information claim is no longer current"
    settled_text = "information claim has already been verified or rejected"
    action = "information reaction action is not allowed"
    reason = "information reaction reason code is not allowed"
    cases: list[tuple[dict[str, Any], dict[str, Any], str]] = [
        ({"extra": 1, "participant_agent_id": "nobody"}, {}, fields),
        ({"message_id": _DROP}, {}, fields),
        ({"participant_agent_id": "nobody", "zone_id": "x"}, {}, two),
        ({"participant_agent_id": listener}, {}, two),
        ({"participant_agent_id": None}, {}, two),
        ({"zone_id": "x", "trigger_event_id": "other"}, {}, local),
        ({}, {"causation_id": "other"}, local),
        ({"trigger_event_id": "other", "action": "x"}, {"causation_id": "other"}, contact),
        ({"trigger_event_id": reverse}, {"causation_id": reverse}, contact),
        ({"trigger_event_id": early}, {"causation_id": early}, contact),
        ({"trigger_event_id": other_type}, {"causation_id": other_type}, contact),
        ({"message_id": "nothing", "action": "x"}, {}, current),
        ({"message_id": 5}, {}, current),
        ({"action": "x", "reason_short": "x"}, {}, action),
        ({"action": None}, {}, action),
        ({"reason_short": "x"}, {}, reason),
        ({"reason_short": 5}, {}, reason),
        ({"message_id": settled, "action": "x"}, {}, settled_text),
    ]
    for change, event_fields, text in cases:
        event_fields = {"causation_id": trigger, **event_fields}
        social.says(group, name, _social(world, {**request, **change}, **event_fields), text)
    verified = copy.deepcopy(world)
    verified.model_state["information_spread_v1"]["beliefs"][listener]["claim-false"]["belief"] = (
        "verified"
    )
    verified_name = social.fixture.world("rumor_heard_contacts_verified", verified, controller)
    social.says(
        group, verified_name, _social(verified, request, causation_id=trigger), settled_text
    )
    apart = copy.deepcopy(world)
    apart.agents[speaker].zone_id = _other_zone(apart, zone)
    apart_name = social.fixture.world("rumor_heard_contacts_apart", apart, controller)
    social.says(group, apart_name, _social(apart, request, causation_id=trigger), local)
    # The Python rule's writes of a reaction: the listener asks, so the query pressure rises.
    assert state["query_pressure"][listener] < 1.0
    twin = copy.deepcopy(world)
    config = InformationSpreadConfig.from_mapping(twin.model_configs["information_spread"])
    pressure = twin.model_state["information_spread_v1"]["query_pressure"]
    pressure[listener] = min(1.0, pressure[listener] + config.query_pressure_per_claim)
    after = social.fixture.world("rumor_heard_contacts_asked", twin, controller)
    social.applies(group, name, _social(world, request, causation_id=trigger), after)


def _reaction_without_state(social: _Social, small: str, base: Any, controller: Any) -> None:
    world = copy.deepcopy(base)
    world.model_state.pop("information_spread_v1", None)
    listener, speaker = sorted(world.agents)[:2]
    world.agents[speaker].zone_id = world.agents[listener].zone_id
    trigger = social.contact(world, speaker, listener)
    name = social.fixture.world("small_no_information", world, controller)
    payload = {
        "interaction_type": "information_reaction",
        "agent_id": listener,
        "participant_agent_id": speaker,
        "zone_id": world.agents[listener].zone_id,
        "message_id": "claim-false",
        "trigger_event_id": trigger,
        "action": "x",
        "reason_short": "x",
    }
    social.says(
        "a_reaction_without_the_information_state_is_suppressed",
        name,
        _social(world, payload, causation_id=trigger),
        "information state is no longer enabled",
    )


def _another_type_texts(social: _Social, small: str, world: Any) -> None:
    actor = sorted(world.agents)[0]
    not_allowed = "typed social interaction type is not allowed"
    for interaction, change, text in (
        ("dance", {}, not_allowed),
        (5, {}, not_allowed),
        (2.5, {}, not_allowed),
        (True, {}, not_allowed),
        ("dance", {"agent_id": "nobody"}, UNKNOWN_AGENT),
        ("dance", {"reason_short": None}, NO_REASON),
        ("dance", {"conversation_state_context": {}}, CONTEXT),
        ("Cooperation", {}, not_allowed),
    ):
        payload = {
            "interaction_type": interaction,
            "agent_id": actor,
            "reason_short": "x",
            **change,
        }
        social.says(
            "another_interaction_type_is_failed_after_the_common_checks",
            small,
            _social(world, payload),
            text,
        )


# ---- The files of the generated Scenario's worlds -----------------------------------------------

# The log records' sequences of the social cases (`contact`), which went on from one test to the
# next in the pytest file: its cooperation, its reactions, then its reaction without the state.
COOPERATION_SEQUENCES = 900_010
REACTION_SEQUENCES = 900_014
NO_INFORMATION_SEQUENCES = 900_018


def event_texts_fixture() -> dict[str, Any]:
    """`event_texts.json`: the generated Scenario at minute 5 (`small`), with the operational
    conditions at minute 25 (`operational`), and the small world as the social cases edit it; the
    cases of test_event_texts.py on them, and of test_social_interaction_texts.py but its
    reactions to a heard claim."""
    fixture = Fixture()
    small_world, small_controller = generated_world()
    small = fixture.world("small", small_world, small_controller, full=True)
    fixture.canonical_digest(small, small_world)
    operational_world_, controller = operational_world()
    operational = fixture.world("operational", operational_world_, controller)
    worlds = {small: small_world, operational: operational_world_}

    _facility_changes(fixture, small, small_world)
    _outage_under_infrastructure(fixture, operational, operational_world_)
    _small_cases(fixture, small, small_world)
    _delay_texts(fixture, small, operational, worlds)
    _water_texts(fixture, operational, operational_world_)
    _staff_absence_texts(fixture, operational, operational_world_)
    _infrastructure_texts(fixture, small, operational, worlds)
    _information_off(fixture, small, small_world, small_controller)
    _staff_operation_texts(fixture, small, small_world)
    _facility_modifier_texts(fixture, small, small_world)
    _loop_texts(fixture, small, small_world)
    _typed_social_first_texts(fixture, small, small_world)
    _outage_later_texts(fixture, operational, operational_world_)

    social = _Social(fixture, itertools.count(COOPERATION_SEQUENCES))
    _untyped_texts(social, small, small_world, small_controller)
    _action_texts(social, small, small_world, small_controller)
    _moved_texts(social, small, small_world, small_controller)
    _family_texts(social, small, small_world, small_controller)
    _complaint_texts(social, small, small_world, small_controller)
    _cooperation_texts(social, small, small_world, small_controller)
    social = _Social(fixture, itertools.count(NO_INFORMATION_SEQUENCES))
    _reaction_without_state(social, small, small_world, small_controller)
    _another_type_texts(social, small, small_world)
    return fixture.document()


def rumor_texts_fixture() -> dict[str, Any]:
    """`rumor_texts.json`: the generated Scenario with the gate's rumors at minute 40 (`rumor`), and
    as the reactions to a heard claim edit it; the cases of test_event_texts.py on it, and of
    test_social_interaction_texts.py's reactions."""
    fixture = Fixture()
    world, controller = rumor_world()
    rumor = fixture.world("rumor", world, controller, full=True)
    fixture.canonical_digest(rumor, world)
    _rumor_texts(fixture, rumor, world)
    _correction_texts(fixture, rumor, world)
    _contact_texts(fixture, rumor, world)
    _unaware_contact(fixture, rumor, world)
    _reaction_texts(_Social(fixture, itertools.count(REACTION_SEQUENCES)), rumor, world, controller)
    return fixture.document()


# ---- integration/test_travel_completion_texts.py ------------------------------------------------

_GONE = object()
QUEUE_PURPOSES = {
    "toilet_queue_position",
    "resource_queue_position",
    "toilet_queue_service",
    "resource_queue_service",
}


def _controller(scenario: Any) -> Any:
    from sheltersim.core.world import WorldState
    from sheltersim.experiments.metrics import MetricsRecorder
    from sheltersim.experiments.runner import OfflineSimulationController
    from sheltersim.experiments.timeline import _MetricSink

    world = WorldState.from_scenario(scenario)
    world.duration_minutes = max(world.duration_minutes, 600)
    return OfflineSimulationController(world, _MetricSink(MetricsRecorder()))


def _hundred() -> Any:
    from sheltersim.experiments.scenario import ScenarioSpec
    from support.paths import SCENARIO_100_AGENTS

    return _controller(ScenarioSpec.load(SCENARIO_100_AGENTS))


def _walk(
    controller: Any, purpose: str, facility_id: str | None, destination: str, agent_id: str = "A001"
) -> Any:
    """A walk of `agent_id` scheduled as the controller does, at its due minute."""
    world = controller.world
    agent = world.agents[agent_id]
    route = controller.space.route(agent.zone_id, destination, agent.id)
    assert route is not None and route.travel_minutes > 0
    assert controller._schedule_travel(
        agent_id=agent.id,
        route=route,
        purpose=purpose,
        facility_id=facility_id,
        minute=world.minute_index,
        validate_state=False,
    )
    event = next(
        event
        for event in world.event_queue.pending()
        if event.event_type == "agent_travel_complete" and event.payload["agent_id"] == agent_id
    )
    world.minute_index = event.due_minute
    return event


def _zone_elsewhere(world: Any, agent_id: str = "A001") -> str:
    return next(name for name in sorted(world.zones) if name != world.agents[agent_id].zone_id)


def _facility_elsewhere(world: Any, kind: str, agent_id: str = "A001") -> Any:
    zone = world.agents[agent_id].zone_id
    return next(
        facility
        for _, facility in sorted(world.facilities.items())
        if facility.kind == kind and facility.zone_id != zone
    )


def _variant(event: Any, payload: dict[str, Any] | None = None, **fields: Any) -> Any:
    """The event with some payload values changed (`_GONE` drops one) or some fields."""
    from sheltersim.events.models import ScheduledEvent

    changed = {**event.payload_dict()}
    for key, value in (payload or {}).items():
        if value is _GONE:
            changed.pop(key, None)
        else:
            changed[key] = value
    return ScheduledEvent(
        event_id=fields.pop("event_id", event.event_id),
        kind=event.kind,
        due_minute=fields.pop("due_minute", event.due_minute),
        priority=fields.pop("priority", event.priority),
        sequence=fields.pop("sequence", event.sequence),
        event_type=event.event_type,
        payload=changed,
        source_id=event.source_id,
        causation_id=event.causation_id,
        provenance=dict(event.provenance),
    )


class _Travel:
    """The travel cases: the controller's handler of walks, on worlds of the file."""

    def __init__(self, fixture: Fixture) -> None:
        self.fixture = fixture
        self.walks = table("agent_travel_complete")

    def world(self, name: str, controller: Any, *, full: bool = False) -> str:
        return self.fixture.world(name, controller.world, controller, full=full)

    def says(self, group: str, world: str, event: Any, text: str, handler: Any = None) -> None:
        self.fixture.says(group, world, handler or self.walks, event, text)


def _walk_checks(travel: _Travel) -> None:
    group = "a_walk_s_minute_person_and_destination_are_checked_in_the_rule_s_order"
    controller = _hundred()
    world = controller.world
    event = _walk(controller, "return", None, _zone_elsewhere(world))
    name = travel.world("hundred_walk_home", controller, full=True)
    # Another person's walk on this task: before the purpose is read.
    travel.says(
        group,
        name,
        _variant(event, {"agent_id": "A002", "purpose": "another"}),
        "travel completion Agent does not match its active task",
    )
    travel.says(
        group,
        name,
        _variant(event, due_minute=event.due_minute + 1),
        "travel completion does not match an active due task",
    )
    for destination in ("nowhere", "it's"):
        travel.says(
            group,
            name,
            _variant(event, {"destination_zone_id": destination}),
            f"destination zone {destination!r} does not exist",
        )
    # A person no longer walking: before the destination is read.
    from sheltersim.domain.models import Activity

    world.agents["A001"].activity = Activity.IDLE
    name = travel.world("hundred_walk_home_idle", controller)
    travel.says(
        group,
        name,
        _variant(event, {"destination_zone_id": "nowhere"}),
        "travel completion Agent does not match its active task",
    )


def _purpose_checks(travel: _Travel) -> None:
    group = "a_walk_home_that_does_not_fit_its_purpose_fails_with_the_purpose_s_text"
    for purpose, text in (
        ("return", "return travel completion does not match its target"),
        ("queue_abandon", "return travel completion does not match its target"),
        ("another", "unsupported travel purpose 'another'"),
        ("sleep_bed", "sleep-bed travel does not match its live reservation"),
        ("presence_relocation", "presence relocation completion does not match its route"),
        (
            "staff_support_travel",
            "staff support travel does not match its assigned request and staff member",
        ),
        ("toilet_request", "toilet travel completion requires a facility_id"),
    ):
        controller = _hundred()
        world = controller.world
        event = _walk(controller, "return", None, _zone_elsewhere(world))
        if purpose in {"return", "queue_abandon"}:
            world.agents["A001"].target_id = "somewhere else"
        name = travel.world(f"hundred_walk_home_for_{purpose}", controller)
        travel.says(group, name, _variant(event, {"purpose": purpose}), text)


def _sleep_bed_check(travel: _Travel) -> None:
    controller = _hundred()
    world = controller.world
    destination = _zone_elsewhere(world)
    event = _variant(_walk(controller, "return", None, destination), {"purpose": "sleep_bed"})
    world.model_state["geometry_overlay_v1"] = {
        "bed_reservations": {"A001": {"room_id": "room-1", "bed_index": 0}},
        "temporary_rooms": {
            "room-1": {"zone_id": destination, "bed_points_m": [{"x": 1.0, "y": 2.0}]}
        },
    }
    name = travel.world("hundred_walk_to_a_bed", controller)
    travel.says(
        "a_sleep_bed_walk_without_its_journey_fails_after_its_reservation_is_read",
        name,
        event,
        "sleep-bed travel does not match its live reservation",
    )


def _relocation_checks(travel: _Travel) -> None:
    for field in ("task_id", "event_id", "to_zone_id"):
        controller = _hundred()
        world = controller.world
        destination = _zone_elsewhere(world)
        event = _variant(
            _walk(controller, "return", None, destination), {"purpose": "presence_relocation"}
        )
        relocation = {
            "task_id": event.payload["task_id"],
            "event_id": event.event_id,
            "from_zone_id": event.payload["route_zone_ids"][0],
            "to_zone_id": destination,
        }
        relocation[field] = "another"
        world.model_state["population_lifecycle_v1"] = {"pending_relocations": {"A001": relocation}}
        name = travel.world(f"hundred_relocation_of_another_{field}", controller)
        travel.says(
            "a_relocation_walk_that_does_not_match_its_pending_relocation_fails",
            name,
            event,
            "presence relocation completion does not match its route",
        )


def _toilet_checks(travel: _Travel) -> None:
    group = "a_toilet_walk_needs_its_facility_there_and_its_target"
    controller = _hundred()
    world = controller.world
    toilet = _facility_elsewhere(world, "toilet")
    event = _walk(controller, "toilet_request", toilet.id, toilet.zone_id)
    name = travel.world("hundred_walk_to_a_toilet", controller)
    for facility_id in (_GONE, "", 7):
        travel.says(
            group,
            name,
            _variant(event, {"facility_id": facility_id}),
            "toilet travel completion requires a facility_id",
        )
    travel.says(
        group,
        name,
        _variant(event, {"facility_id": "nothing"}),
        "toilet travel destination does not match its facility",
    )
    elsewhere = next(zone for zone in sorted(world.zones) if zone != toilet.zone_id)
    travel.says(
        group,
        name,
        _variant(event, {"destination_zone_id": elsewhere}),
        "toilet travel destination does not match its facility",
    )
    world.agents["A001"].target_id = "somewhere else"
    name = travel.world("hundred_walk_to_a_toilet_elsewhere", controller)
    travel.says(group, name, event, "toilet travel completion does not match its target")


def _queue_walk_home(travel: _Travel) -> None:
    controller = _hundred()
    world = controller.world
    event = _walk(controller, "return", None, _zone_elsewhere(world))
    name = travel.world("hundred_walk_home_again", controller)
    for purpose in sorted(QUEUE_PURPOSES):
        travel.says(
            "a_queue_walk_on_a_walk_home_fails_its_fifo_check",
            name,
            _variant(event, {"purpose": purpose}),
            "physical queue travel does not match its FIFO reservation",
        )


def _dispatched(fixture: Fixture) -> dict[str, Any]:
    """A walk event of another purpose before the real one in the same minute, in the queue: the
    world the core's dispatch phase runs on, and what the pytest checked of the records."""
    from sheltersim.events.models import EventStatus

    controller = _hundred()
    world = controller.world
    event = _walk(controller, "return", None, _zone_elsewhere(world))
    world.schedule_event(
        _variant(
            event,
            {"purpose": "another"},
            event_id="walk-of-another-purpose",
            priority=event.priority - 1,
            sequence=world.allocate_event_sequence(),
        )
    )
    name = fixture.world("hundred_walk_home_after_another", world, controller)
    return {
        "world": name,
        "records": [
            [
                "walk-of-another-purpose",
                EventStatus.FAILED.value,
                "unsupported travel purpose 'another'",
            ],
            [event.event_id, EventStatus.APPLIED.value, None],
        ],
    }


def _resource_checks(travel: _Travel, scenario: Any) -> None:
    group = "a_resource_walk_needs_the_service_its_facility_and_its_route"
    controller = _controller(scenario)
    world = controller.world
    assert (
        controller.event_dispatcher.handlers["agent_travel_complete"].resource_service is not None
    )
    station = _facility_elsewhere(world, "water_station")
    event = _walk(controller, "resource_request", station.id, station.zone_id)
    name = travel.world("generated_walk_to_water", controller, full=True)
    # `AgentTravelCompletionHandler(controller.toilet_service, validate_request_state=False)`.
    without = table("agent_travel_complete", resource_service=None, validate_request_state=False)
    travel.says(group, name, event, "resource travel is not enabled for this scenario", without)
    travel.says(
        group,
        name,
        _variant(event, {"facility_id": _GONE}),
        "resource travel completion requires a facility_id",
    )
    text = "resource travel completion does not match its target facility"
    route = list(event.payload["route_zone_ids"])
    for change in (
        {"facility_id": "nothing"},
        {"origin_zone_id": "nowhere"},
        {"origin_zone_id": _GONE, "route_zone_ids": [5, *route[1:]]},
        {"origin_zone_id": _GONE, "route_zone_ids": []},
        {"route_zone_ids": route[:-1]},
        {"route_zone_ids": "not a route"},
        {"facility_id": _facility_elsewhere(world, "toilet").id},
    ):
        travel.says(group, name, _variant(event, change), text)
    world.agents["A001"].target_id = "somewhere else"
    name = travel.world("generated_walk_to_water_elsewhere", controller)
    travel.says(group, name, event, text)


def travel_texts_fixture() -> dict[str, Any]:
    """`travel_texts.json`: the 100 people's Scenario with a walk scheduled as the controller does
    (`hundred_walk_home`, and as the cases edit it), and the generated Scenario of the Project
    screen with a walk to water; the cases of test_travel_completion_texts.py but its walks in a
    queue (a run of that Scenario until one, which stays there as the test of the kernel's entry),
    and the world of its dispatch step (`dispatched`)."""
    import tempfile

    from support.generated import preview_scenario

    fixture = Fixture()
    travel = _Travel(fixture)
    _walk_checks(travel)
    _purpose_checks(travel)
    _sleep_bed_check(travel)
    _relocation_checks(travel)
    _toilet_checks(travel)
    _queue_walk_home(travel)
    dispatched = _dispatched(fixture)
    with tempfile.TemporaryDirectory(prefix="core-texts-") as root:
        scenario = preview_scenario(Path(root))
    _resource_checks(travel, scenario)
    return {**fixture.document(), "dispatched": dispatched}
