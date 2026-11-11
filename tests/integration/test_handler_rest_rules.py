"""The rest of events/handlers.py's kernels against what their Python rules gave.

benchmarks/rule_shadow/shadow_handlers_rest.py does this on the gate's runs; 7 days hold almost none
of these events, so here each rule also runs on Scenarios made to reach it, with the branches those
may not reach and the values that change a result (docs/rust-port/lanes/m6-handlers-rest.md).

None of these handlers has its Python rule left (lane L10, `kernel_only`). A run or a case is
checked against what the Python rule gave, written down from it before it went: the outcome's class
and text, the amounts it wrote, or a digest of the world and records (`_digest`).

Model version 2 (step 4, L11: the pinned libm crate and the information events' draw keys)
changed some of these results: those digests were re-recorded from the kernels of that model
and are no longer the deleted Python rules'.
"""

from __future__ import annotations

import copy
import hashlib
import json
import sys
from collections import Counter
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest
from support.paths import SMALL_REPLAY
from support.world import kernel_says_python_s_text

from sheltersim import native
from sheltersim.core.engine import TickEngine
from sheltersim.core.rng import CounterRNG
from sheltersim.core.world import WorldState
from sheltersim.domain.information import upgrade_information_runtime_state
from sheltersim.domain.models import (
    ActiveTask,
    Activity,
    InformationSpreadConfig,
    SupportRequestState,
    SupportRequestStatus,
)
from sheltersim.domain.simulator_state import initialize_simulator_extensions
from sheltersim.domain.staff import StaffService
from sheltersim.events.handlers import (
    FacilityOutageHandler,
    InformationContactHandler,
    ProximityContactHandler,
    ResourceLedgerHandler,
    SocialInteractionHandler,
    StaffOperationCompletionHandler,
    StaffSupportCompletionHandler,
    WaterRestorationHandler,
)
from sheltersim.events.models import (
    EventApplicationError,
    EventKind,
    EventLogRecord,
    EventStatus,
    EventSuppressed,
    ScheduledEvent,
)
from sheltersim.experiments.metrics import MetricsRecorder
from sheltersim.experiments.runner import OfflineSimulationController
from sheltersim.experiments.scenario import ScenarioSpec
from sheltersim.experiments.timeline import _MetricSink

# The handlers whose outcomes a run counts, with their kernels.
KERNELS = {
    "ProximityContactHandler": ("handlers", "proximity"),
    "InformationRumorHandler": ("information_events", "rumor"),
    "InformationCorrectionHandler": ("information_events", "correction"),
    "InformationContactHandler": ("information_events", "contact"),
    "ResourceLedgerHandler": ("handlers", "resource_ledger"),
    "FacilityOutageHandler": ("handlers", "facility_outage"),
    "FacilityRecoveryHandler": ("handlers", "facility_recovery"),
    "InfrastructureEventHandler": ("handlers", "infrastructure"),
    "DeliveryDelayHandler": ("handlers", "delivery_delay"),
    "WaterOutageHandler": ("handlers", "water_outage"),
    "WaterRestorationHandler": ("handlers", "water_restoration"),
    "StaffUnavailabilityHandler": ("handlers", "staff_unavailable"),
    "StaffRestorationHandler": ("handlers", "staff_restoration"),
    "StaffOperationArrivalHandler": ("staff_operation_events", "arrive"),
    "StaffOperationReliefHandler": ("staff_operation_events", "relief"),
    "StaffOperationCompletionHandler": ("staff_operation_events", "complete"),
    "SocialInteractionHandler": ("social_interaction", "apply"),
    "AgentTravelCompletionHandler": ("travel_completion", "apply"),
}


def _gate() -> Any:
    root = str(Path(__file__).resolve().parents[2] / "benchmarks")
    if root not in sys.path:
        sys.path.insert(0, root)
    import gate_scenarios

    return gate_scenarios


def _bytes(world: WorldState) -> str:
    return world._canonical_text(world._canonical_data())


def _outcome(call: Any) -> tuple[str, Any]:
    try:
        result = call()
    except EventSuppressed as raised:
        return "suppressed", str(raised)
    except EventApplicationError as raised:
        return "failed", str(raised)
    except native.Unsupported as raised:
        return "refused", str(raised)
    return "applied", [record.to_dict() for record in result or ()]


def _digest(value: Any) -> str:
    """A digest of a JSON value, to pin what the Python rule gave."""
    text = json.dumps(value, sort_keys=True, ensure_ascii=False)
    return hashlib.sha256(text.encode("utf-8")).hexdigest()[:16]


def _copies(world: WorldState) -> tuple[WorldState, WorldState]:
    document = world.to_dict()
    return WorldState.from_dict(document), WorldState.from_dict(document)


class _Comparing:
    """A handler that counts the outcomes of the real one, which it runs.

    No handler here has its Python rule left (lane L10, `kernel_only`): a run is compared with the
    Python rules' run by the digest of its world (`_is_the_python_rules_run`).
    """

    def __init__(self, handler: Any, seen: Counter[str]) -> None:
        self.handler = handler
        self.seen = seen

    def apply(self, world: WorldState, event: ScheduledEvent) -> Any:
        outcome = "applied"
        try:
            return self.handler.apply(world, event)
        except EventSuppressed:
            outcome = "suppressed"
            raise
        except EventApplicationError:
            outcome = "failed"
            raise
        finally:
            self.seen[f"{event.event_type}.{outcome}"] += 1


def _run_comparing(scenario: Any, seen: Counter[str]) -> WorldState:
    world = WorldState.from_scenario(scenario)
    controller = OfflineSimulationController(world, _MetricSink(MetricsRecorder()))
    handlers = controller.event_dispatcher.handlers
    for event_type, handler in list(handlers.items()):
        if type(handler).__name__ in KERNELS:
            handlers[event_type] = _Comparing(handler, seen)
    engine = TickEngine(world, phase_handler=controller)
    for _ in range(world.duration_minutes):
        engine.step()
    return world


def _is_the_python_rules_run(world: WorldState, digest: str) -> None:
    """The world at the end is the one the Python rules made: its digest, taken from that run.

    Pinned on every OS, as the gate's hashes are.
    """
    assert _digest(_bytes(world)) == digest


def test_conditions_of_the_form_are_python_s() -> None:
    gate = _gate()
    conditions = {
        "scheduled": [
            {
                "kind": "power_outage",
                "component_id": "grid-main",
                "start_minute": 20,
                "end_minute": 60,
            },
            {
                "kind": "equipment_failure",
                "facility_id": "water_station_main",
                "units_out": 1,
                "start_minute": 30,
                "end_minute": 70,
            },
            {"kind": "extreme_heat", "zone_id": "sleeping", "start_minute": 10, "end_minute": 50},
            {"kind": "extreme_cold", "zone_id": "food", "start_minute": 55, "end_minute": 80},
            {
                "kind": "staff_unavailable",
                "staff_id": "staff_001",
                "start_minute": 15,
                "end_minute": 45,
                "repeat_every_minutes": 40,
            },
            {
                "kind": "resource_loss",
                "resource_id": "water_ml",
                "quantity": 2_000,
                "due_minute": 50,
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
                {"dependent_id": "accessible_toilet_01", "dependency_id": "panel-main"},
                {"dependent_id": "panel-main", "dependency_id": "grid-main"},
            ],
            "heat_stress_per_minute": 0.001,
            "cold_stress_per_minute": 0.001,
            "source_id": gate.SOURCE_ID,
        },
    }
    seen: Counter[str] = Counter()
    world = _run_comparing(
        gate.generated(evacuees=20, minutes=120, operational_conditions=conditions), seen
    )
    _is_the_python_rules_run(world, "f5758af05264f6df")
    for event_type in (
        "power_outage",
        "power_restored",
        "equipment_failure",
        "equipment_repaired",
        "extreme_heat",
        "extreme_cold",
        "environmental_stressor_end",
        "staff_unavailable",
        "staff_restored",
        "resource_loss",
        "resource_delivery",
    ):
        assert seen[f"{event_type}.applied"] > 0, (event_type, seen)


def test_rumors_and_corrections_are_python_s() -> None:
    gate = _gate()
    seen: Counter[str] = Counter()
    world = _run_comparing(gate.with_rumors(gate.generated(evacuees=20, minutes=120)), seen)
    _is_the_python_rules_run(world, "44de998728962e5d")
    for event_type in ("information_rumor", "information_correction", "information_contact_update"):
        assert seen[f"{event_type}.applied"] > 0, (event_type, seen)


def _schema_1(state: dict[str, Any]) -> None:
    """The information state as a save of schema 1 holds it: beliefs without the summary fields."""
    state["schema_version"] = 1
    for beliefs in state["beliefs"].values():
        for record in beliefs.values():
            for name in ("ever_misbelieved", "ever_forgotten", "last_correction_accepted"):
                del record[name]


def test_an_information_event_upgrades_a_state_of_schema_1_first() -> None:
    """A state of schema 1 in memory is upgraded in place before the event's checks.

    A save of schema 1 is view-only (core/model_version.py `LEGACY_SHAPES_TICKABLE` is false), so
    no restored Run reaches this. The handlers' Python rule upgraded such a state in place; the
    kernels do the same: the tick is the one of a state upgraded before it.
    """
    gate = _gate()
    scenario = gate.with_rumors(gate.generated(evacuees=20, minutes=120))
    worlds = []
    for upgraded_before in (False, True):
        world = WorldState.from_scenario(scenario)
        controller = OfflineSimulationController(world, _MetricSink(MetricsRecorder()))
        engine = TickEngine(world, phase_handler=controller)
        for _ in range(60):
            engine.step()
        state = world.model_state["information_spread_v1"]
        _schema_1(state)
        if upgraded_before:
            upgrade_information_runtime_state(state)
        engine.step()
        corrected = world.event_log.find("correction-false")
        assert corrected is not None and corrected.status is EventStatus.APPLIED
        assert world.model_state["information_spread_v1"]["schema_version"] == 2
        worlds.append(world)
    assert _bytes(worlds[0]) == _bytes(worlds[1])


def test_an_information_state_the_upgrade_turns_down_fails_the_event_before_its_checks() -> None:
    """The host's upgrade (policy E11) runs where the Python rule ran it: after the model's checks
    and before the event's, so a payload that does not fit fails with the upgrade's text, and a
    state of an unknown schema is changed by nothing."""
    gate = _gate()
    world = WorldState.from_scenario(gate.with_rumors(gate.generated(evacuees=20, minutes=120)))
    world.model_state["information_spread_v1"]["schema_version"] = 99
    before = _bytes(world)
    event = ScheduledEvent(
        event_id="rumor-test",
        kind=EventKind.EXOGENOUS,
        due_minute=world.minute_index,
        priority=0,
        sequence=900_001,
        event_type="information_contact_update",
        payload={},
        source_id="SRC-TEST",
    )
    said = _outcome(lambda: InformationContactHandler().apply(world, event))
    assert said == (
        "failed",
        "invalid information-spread state: unsupported information-spread runtime state schema",
    )
    assert _bytes(world) == before


@pytest.mark.parametrize(
    ("name", "digest"),
    [
        ("multi_operational", "666880beb4cb8d08"),
        ("water_intervention", "26726ee9ede8b09d"),
        ("failure_day3", "7228ef1bb1598fdc"),
        ("delivery_intervention", "00f630574725b910"),
    ],
)
def test_the_operational_fixtures_are_python_s(name: str, digest: str) -> None:
    gate = _gate()
    moved = {
        "resource_delivery": 20,
        "delivery_delayed": 10,
        "water_outage": 30,
        "water_restored": 50,
        "facility_outage": 20,
        "facility_restored": 60,
    }
    seen: Counter[str] = Counter()
    world = _run_comparing(gate.fixture_short(name, minutes=100, agents=20, remap=moved), seen)
    _is_the_python_rules_run(world, digest)
    assert sum(seen.values()) > 0


def _small_world(generated_scenario: Any) -> tuple[WorldState, OfflineSimulationController]:
    world = WorldState.from_scenario(generated_scenario)
    controller = OfflineSimulationController(world, _MetricSink(MetricsRecorder()))
    engine = TickEngine(world, phase_handler=controller)
    for _ in range(5):
        engine.step()
    return world, controller


def _event(
    world: WorldState, event_type: str, payload: dict[str, Any], **fields: Any
) -> ScheduledEvent:
    return ScheduledEvent(
        event_id=fields.pop("event_id", f"test-{event_type.replace('_', '-')}"),
        kind=fields.pop("kind", EventKind.EXOGENOUS),
        due_minute=world.minute_index,
        priority=0,
        sequence=fields.pop("sequence", 900_001),
        event_type=event_type,
        payload=payload,
        source_id=fields.pop("source_id", "SRC-TEST"),
        causation_id=fields.pop("causation_id", None),
        provenance=fields.pop("provenance", {"source_type": "assumption", "source_id": "SRC-TEST"}),
    )


# ---- What a kernel leaves to the Python rule ----------------------------------------------------


def test_a_failed_outage_changes_nothing_and_says_python_s_text(generated_scenario: Any) -> None:
    world, _ = _small_world(generated_scenario)
    facility_id = sorted(world.facilities)[0]
    handler = FacilityOutageHandler()
    failed, suppressed = EventApplicationError, EventSuppressed
    for payload, expected in (
        ({"facility_id": facility_id}, (failed, "facility outage payload fields do not match")),
        (
            {"facility_id": facility_id, "units_out": 0},
            (suppressed, "zero-unit facility outage has no effect"),
        ),
        (
            {"facility_id": facility_id, "units_out": -1},
            (failed, "facility outage units_out cannot be negative"),
        ),
        ({"facility_id": "nowhere", "units_out": 1}, (failed, "facility 'nowhere' does not exist")),
        (
            {"facility_id": facility_id, "units_out": 10_000},
            (failed, "facility outage exceeds currently working capacity"),
        ),
        # A Scenario's units are any non-negative int: past 64 bits too, as Python compares them.
        (
            {"facility_id": facility_id, "units_out": 10**20},
            (failed, "facility outage exceeds currently working capacity"),
        ),
        (
            {"facility_id": facility_id, "units_out": -(10**20)},
            (failed, "facility outage units_out cannot be negative"),
        ),
    ):
        event = _event(world, "facility_outage", payload)
        kernel_says_python_s_text(handler, world, event, expected)


def test_a_stale_proximity_sample_is_failed_in_python_s_text(generated_scenario: Any) -> None:
    world, _ = _small_world(generated_scenario)
    event = _event(world, "proximity_contact_update", {})
    text = "proximity contact provenance or time is invalid"
    kernel_says_python_s_text(
        ProximityContactHandler(), world, event, (EventApplicationError, text)
    )


def test_a_contact_that_no_longer_applies_says_python_s_text(generated_scenario: Any) -> None:
    world, _ = _small_world(generated_scenario)
    event = _event(world, "information_contact_update", {"message_id": "nothing"})
    text = "information_contact_update payload fields do not match"
    expected = (EventApplicationError, text)
    kernel_says_python_s_text(InformationContactHandler(), world, event, expected)


def test_a_restoration_of_no_outage_says_python_s_text(generated_scenario: Any) -> None:
    world, _ = _small_world(generated_scenario)
    event = _event(world, "water_restored", {"outage_id": "nothing"})
    expected = (EventApplicationError, "water restoration does not match its outage")
    kernel_says_python_s_text(WaterRestorationHandler(), world, event, expected)


def test_a_patched_handler_is_what_the_loop_calls(
    generated_scenario: Any, monkeypatch: pytest.MonkeyPatch
) -> None:
    world, controller = _small_world(generated_scenario)
    called: list[str] = []

    def fake(_self: Any, _world: Any, event: ScheduledEvent) -> None:
        called.append(event.event_id)

    monkeypatch.setattr(ResourceLedgerHandler, "apply", fake)
    event = _event(
        world,
        "resource_delivery",
        {"warehouse_id": "warehouse_main", "resource_id": "water_ml", "quantity": 1},
        event_id="delivery-now",
    )
    world.schedule_event(event)
    TickEngine(world, phase_handler=controller).step()
    assert called == ["delivery-now"]
    assert world.event_log.find("delivery-now").status is EventStatus.APPLIED


# ---- Care that a selected formula decides --------------------------------------------------


def _assigned_care(world: WorldState, kind: str) -> tuple[ScheduledEvent, str]:
    agent_id = sorted(world.agents)[0]
    staff_id = sorted(world.staff)[0]
    person = world.staff[staff_id]
    agent = world.agents[agent_id]
    task_id = "staff-support-test"
    for key in list(world.active_tasks):
        task = world.active_tasks[key]
        if task.agent_id == agent_id or task.staff_id == staff_id:
            del world.active_tasks[key]
    agent.support_request_id = "support-t1"
    agent.activity = Activity.SEEKING_HELP
    agent.target_id = None
    agent.zone_id = person.zone_id
    agent.stress = 0.5
    world.support_requests["support-t1"] = SupportRequestState(
        "support-t1", agent_id, world.minute_index - 1, world.minute_index + 50
    )
    request = world.support_requests["support-t1"]
    from sheltersim.domain import care_receipts

    common = {
        "request_id": "support-t1",
        "agent_id": agent_id,
        "household_id": None,
        "support_kind": kind,
    }
    care_receipts.append_applied_care_transition(world, **common, status="requested")
    care_receipts.append_applied_care_transition(
        world, **common, status="assigned", staff_id=staff_id
    )
    care_receipts.append_applied_care_transition(
        world, **common, status="started", staff_id=staff_id
    )
    request.status = SupportRequestStatus.ASSIGNED
    request.assigned_staff_id = staff_id
    request.task_id = task_id
    world.active_tasks[task_id] = ActiveTask(
        id=task_id,
        kind="staff_support",
        agent_id=agent_id,
        resource_id=None,
        start_minute=world.minute_index,
        due_minute=world.minute_index,
        staff_id=staff_id,
    )
    person.active_task_id = task_id
    person.work_queue[:] = [task_id]
    person.busy_until = world.minute_index
    return (
        ScheduledEvent(
            event_id="staff-support-complete-test",
            kind=EventKind.COMPLETION,
            due_minute=world.minute_index,
            priority=0,
            sequence=900_002,
            event_type="staff_support_complete",
            payload={
                "request_id": "support-t1",
                "agent_id": agent_id,
                "staff_id": staff_id,
                "task_id": task_id,
                "support_duration_minutes": 10,
            },
            source_id=task_id,
            provenance={"source_type": "assumption", "source_id": "ASM-TEST"},
        ),
        agent_id,
    )


def _care_is_python_s(world: WorldState, kind: str, digest: str) -> dict[str, Any]:
    """The kernel's care: its records and world are the deleted Python rule's, by the digest of
    what that rule returned and left, recorded once. Pinned on every OS, as the gate's
    hashes are."""
    event, _ = _assigned_care(world, kind)
    handler = StaffSupportCompletionHandler(cooldown_minutes=30, require_colocated=True)
    records = native.rules("support").complete_support(handler, world, event)
    assert world._core.mirror("body_state_v1").verify()
    assert len(records) == 1
    found = {"records": [item.to_dict() for item in records], "world": _bytes(world)}
    assert _digest(found) == digest
    return records[0].event.payload


TYPED_KINDS = (
    "caregiver_assistance",
    "dependent_assistance",
    "privacy_accommodation",
    "mobility_assistance",
    "language_access",
    "medical_assessment",
    "medication_support",
    "hygiene_support",
)


# What the Python rule gave, by care outcome formula and kind: `_care_is_python_s`.
CARE_DIGESTS = {
    ("hygiene-support-fixed-relief-v1", "caregiver_assistance"): "1987d4097e4710cb",
    ("hygiene-support-fixed-relief-v1", "dependent_assistance"): "1a9d433c78951f5e",
    ("hygiene-support-fixed-relief-v1", "hygiene_support"): "2f3da6602096c23d",
    ("hygiene-support-fixed-relief-v1", "language_access"): "4e7fe34cefd5355c",
    ("hygiene-support-fixed-relief-v1", "medical_assessment"): "18337c9a5f235364",
    ("hygiene-support-fixed-relief-v1", "medication_support"): "f1d13d0f64c9e3f4",
    ("hygiene-support-fixed-relief-v1", "mobility_assistance"): "46b1d42c482cbf2a",
    ("hygiene-support-fixed-relief-v1", "privacy_accommodation"): "f419db9fdea9a452",
    ("hygiene-support-proportional-relief-v1", "caregiver_assistance"): "a32a24f02bf6a57a",
    ("hygiene-support-proportional-relief-v1", "dependent_assistance"): "505ac668012822d4",
    ("hygiene-support-proportional-relief-v1", "hygiene_support"): "4dad6e5e9970edb5",
    ("hygiene-support-proportional-relief-v1", "language_access"): "a4561ca27263e9fb",
    ("hygiene-support-proportional-relief-v1", "medical_assessment"): "7d60a45dcdf111e4",
    ("hygiene-support-proportional-relief-v1", "medication_support"): "a75e3bbdb4ec6f29",
    ("hygiene-support-proportional-relief-v1", "mobility_assistance"): "ad8fbf05cf6cdd6b",
    ("hygiene-support-proportional-relief-v1", "privacy_accommodation"): "c34fce4f7dffd6a5",
    ("typed-care-support-response-v1", "caregiver_assistance"): "0d9431b83fb0297e",
    ("typed-care-support-response-v1", "dependent_assistance"): "ed8fe5ca10c1ecff",
    ("typed-care-support-response-v1", "hygiene_support"): "17f03d8a675567de",
    ("typed-care-support-response-v1", "language_access"): "099cf836befe8471",
    ("typed-care-support-response-v1", "medical_assessment"): "e09dc238d5b650a6",
    ("typed-care-support-response-v1", "medication_support"): "4e2c51f45b6a1a39",
    ("typed-care-support-response-v1", "mobility_assistance"): "cee9ead6f200553d",
    ("typed-care-support-response-v1", "privacy_accommodation"): "1b9bcc532e3e0b13",
}


@pytest.mark.parametrize("kind", TYPED_KINDS)
@pytest.mark.parametrize(
    "model_id", [model for domain, model in _gate().selectable_models() if domain == "care_outcome"]
)
def test_a_kind_a_selected_formula_decides_is_python_s(
    generated_scenario: Any, model_id: str, kind: str
) -> None:
    registry = generated_scenario.model_configs["policy_registry"]
    registry.pop("suite_id", None)
    registry["care_outcome"] = model_id
    world, _ = _small_world(generated_scenario)
    payload = _care_is_python_s(world, kind, CARE_DIGESTS[(model_id, kind)])
    assert payload["status"] == "completed"


def test_care_with_a_dependent_is_python_s(generated_scenario: Any) -> None:
    registry = generated_scenario.model_configs["policy_registry"]
    registry.pop("suite_id", None)
    registry["care_outcome"] = "typed-care-support-response-v1"
    world, _ = _small_world(generated_scenario)
    caregiver, dependent = sorted(world.agents)[:2]
    context = {
        "caregiver": {
            "household_id": "home-1",
            "care_for_ids": [dependent],
            "source_ids": ["SRC-CAREGIVER"],
        },
        "dependent": {
            "household_id": "home-1",
            "caregiver_ids": [caregiver],
            "source_ids": ["SRC-DEPENDENT"],
        },
    }
    world.model_configs["population_context"] = {
        "people": {caregiver: context["caregiver"], dependent: context["dependent"]}
    }
    payload = _care_is_python_s(world, "caregiver_assistance", "db56eb17db46e325")
    assert payload["outcome_observation"] == "observed_change"
    assert {change["entity_id"] for change in payload["state_changes"]} == {caregiver, dependent}
    assert "SRC-DEPENDENT" in payload["source_ids"]


def test_a_care_delta_1_percent_off_shows(generated_scenario: Any) -> None:
    registry = generated_scenario.model_configs["policy_registry"]
    registry.pop("suite_id", None)
    registry["care_outcome"] = "typed-care-support-response-v1"
    world, _ = _small_world(generated_scenario)
    event, agent_id = _assigned_care(world, "mobility_assistance")
    twin = copy.deepcopy(world)
    twin.model_configs["body_dynamics"]["mobility_assistance_stress_relief"] = 0.03 * 1.01
    handler = StaffSupportCompletionHandler(cooldown_minutes=30, require_colocated=True)
    native.rules("support").complete_support(handler, world, event)
    native.rules("support").complete_support(handler, twin, event)
    assert world.agents[agent_id].stress != twin.agents[agent_id].stress


# ---- The assignment of a request -----------------------------------------------------------------


def _route(minutes: int) -> Any:
    return SimpleNamespace(travel_minutes=minutes)


# The world after `StaffService.assign_pending`'s Python rule, by test: a digest taken before the
# rule went (lane L10, `kernel_only`).
ASSIGNED_DIGESTS = {
    "free_worker": "b2e57bbfee3b75ed",
    "promoted": "d00be4ea83ec3799",
    "route.None.True": "7c41fe3c5d8b72fb",
    "route.1000000.True": "c814b2051a10d9ca",
    "route.3.False": "d0f21eb6a7cee0ad",
    "route.3.True": "4cc8d4c48df97a4d",
}


def _assign(
    label: str, world: WorldState, *, resolver: Any = None, scheduler: Any = None, **options: Any
) -> Any:
    """`assign_work`, the kernel that assigns, and the world it leaves against the Python rule's."""
    actual = native.rules("staff").assign_work(
        StaffService(world, route_resolver=resolver, travel_scheduler=scheduler, **options),
        world.minute_index,
        validate_state=False,
    )
    _is_the_python_rules_run(world, ASSIGNED_DIGESTS[label])
    return actual


def _staffed_world() -> WorldState:
    """A world whose staff are all free, with no shifts to read."""
    document = json.loads(SMALL_REPLAY.read_text(encoding="utf-8"))
    document["schema_version"] = 3
    document["duration_minutes"] = 200
    document["scheduled_events"] = []
    document["model_configs"]["staff_operations"] = {}
    document["model_configs"]["shared_operations"] = {
        "schema_version": 1,
        "policy_version": "shared-operations-v1",
    }
    template = document["staff"][0]
    document["staff"] = [
        {**template, "id": f"S{index:02}", "skills": ["support"]} for index in range(1, 4)
    ]
    world = WorldState.from_scenario(ScenarioSpec.model_validate(document))
    initialize_simulator_extensions(world)
    world.minute_index = 10
    return world


def _open_request(world: WorldState, *, age: int = 0) -> str:
    agent_id = sorted(world.agents)[0]
    agent = world.agents[agent_id]
    for key in list(world.active_tasks):
        if world.active_tasks[key].agent_id == agent_id:
            del world.active_tasks[key]
    agent.activity = Activity.SEEKING_HELP
    agent.target_id = None
    agent.support_request_id = "support-t1"
    world.support_requests["support-t1"] = SupportRequestState(
        "support-t1", agent_id, world.minute_index - age, world.minute_index + 50
    )
    return "support-t1"


def test_a_free_worker_takes_a_request_as_in_python() -> None:
    world = _staffed_world()
    request_id = _open_request(world)
    assigned = _assign("free_worker", world)
    assert request_id in assigned
    assert world.support_requests[request_id].status is SupportRequestStatus.ASSIGNED


def test_an_old_request_is_promoted_as_in_python() -> None:
    world = _staffed_world()
    request_id = _open_request(world, age=5)
    assert _assign("promoted", world, fairness_after_minutes=3) == (request_id,)
    assert world.metric_state["support_fairness_promoted_assignment_count"] >= 1.0


@pytest.mark.parametrize(
    ("minutes", "scheduled", "reason"),
    [
        (None, True, "route_unavailable_at_assignment"),
        (10**6, True, "insufficient_run_time"),
        (3, False, "resident_travel_schedule_rejected"),
        (3, True, None),
    ],
)
def test_a_route_decides_as_in_python(
    minutes: int | None, scheduled: bool, reason: str | None
) -> None:
    world = _staffed_world()
    request_id = _open_request(world)
    route = None if minutes is None else _route(minutes)
    _assign(
        f"route.{minutes}.{scheduled}",
        world,
        resolver=lambda *_: route,
        scheduler=lambda *_: scheduled,
    )
    assert world.support_requests[request_id].last_assignment_wait_reason_code == reason


def test_nobody_free_leaves_the_plain_kernel_running() -> None:
    world = _staffed_world()
    _open_request(world)
    for person in world.staff.values():
        person.active_task_id = "elsewhere"
    service = StaffService(world)
    assert (
        native.rules("staff").assign_none(service, world.minute_index, validate_state=False) == ()
    )


# ---- What else the handlers ask ------------------------------------------------------------------


def test_only_the_due_requests_go_to_the_policy(generated_scenario: Any) -> None:
    world, _ = _small_world(generated_scenario)
    request_id = _open_request(world, age=7)
    asked: list[tuple[str, str]] = []
    policy = SimpleNamespace(
        limits=SimpleNamespace(complaint_wait_minutes=7),
        consider_complaint_response=lambda _world, *, agent_id, request_id: asked.append(
            (agent_id, request_id)
        ),
    )
    controller = SimpleNamespace(world=world, social_policy=policy)
    native.rules("staff").consider_due_complaints(controller, world.minute_index)
    # The Python step's answer: the one request due this minute.
    assert asked == [(world.support_requests[request_id].agent_id, request_id)]
    asked.clear()
    native.rules("staff").consider_due_complaints(controller, world.minute_index + 1)
    assert asked == []
    controller.social_policy = None
    native.rules("staff").consider_due_complaints(controller, world.minute_index)


def test_ingest_of_a_record_of_this_tick_is_python_s(generated_scenario: Any) -> None:
    from support.receipts import facility_recovery_record

    from sheltersim.domain import care_receipts

    world, _ = _small_world(generated_scenario)
    facility_id = sorted(world.facilities)[0]
    record = facility_recovery_record(
        world,
        facility_id=facility_id,
        component_id="component-1",
        status="degraded",
        previous_working_units=2,
        working_units=1,
        causation_id="cause-1",
    )[0]
    world.event_log.append(record)
    care_receipts.ingest_applied_shared_operation_records(world, (record,))
    history = world.model_state["shared_operations_v1"]["history"]
    assert history[-1]["event_id"] == record.event.event_id
    # What the Python rule made of it (it has none left, lane L10), by the digest of the world.
    once = _bytes(world)
    _is_the_python_rules_run(world, "9d84ddda5f8d0fd2")
    # Recorded once: the second time it is found.
    care_receipts.ingest_applied_shared_operation_records(world, (record,))
    assert _bytes(world) == once


def test_the_kernels_are_what_ran(generated_scenario: Any) -> None:
    world, _ = _small_world(generated_scenario)
    before, _ = native.rules("handlers").stats()
    handler = FacilityOutageHandler()
    facility_id = next(key for key, item in sorted(world.facilities.items()) if item.working_units)
    event = _event(world, "facility_outage", {"facility_id": facility_id, "units_out": 1})
    handler.apply(world, event)
    after, _ = native.rules("handlers").stats()
    assert after["facility_outage"] > before.get("facility_outage", 0)


# ---- The other social events: their fixed amounts, which few runs reach ------------------------


SOCIAL_SOURCE = "ASM-EVENT-TRIGGERED-SOCIAL-INTERACTIONS-V1"


def _social_event(world: WorldState, payload: dict[str, Any], **fields: Any) -> ScheduledEvent:
    return _event(
        world,
        "social_interaction",
        payload,
        kind=EventKind.ENDOGENOUS,
        source_id="social-policy-test",
        provenance={
            "source_type": "assumption",
            "source_id": SOCIAL_SOURCE,
            "policy_version": "social-policy-test",
        },
        **fields,
    )


def _social_is_python_s(world: WorldState, event: ScheduledEvent) -> None:
    """The kernel applies the event and writes only the stress and trust of the people it names,
    whose values each test writes down from the deleted Python rule."""
    twin = copy.deepcopy(world)
    handler = SocialInteractionHandler()
    assert native.rules("social_interaction").apply(handler, world, event) is None
    for key in ("agent_id", "participant_agent_id"):
        agent_id = event.payload.get(key)
        if agent_id is not None:
            twin.agents[agent_id].stress = world.agents[agent_id].stress
            twin.agents[agent_id].trust = world.agents[agent_id].trust
    assert _bytes(world) == _bytes(twin)


@pytest.mark.parametrize(
    ("action", "stress", "trust"),
    [
        ("acknowledge_complaint", 0.5 - 0.02, 0.5 + 0.01),
        ("explain_staff_process", 0.5 - 0.01, 0.5 + 0.01),
        ("share_practical_information", 0.5, 0.5 + 0.005),
    ],
)
def test_a_complaint_response_is_python_s(
    generated_scenario: Any, action: str, stress: float, trust: float
) -> None:
    world, _ = _small_world(generated_scenario)
    request_id = _open_request(world, age=4)
    agent = next(iter(sorted(world.agents.items())))[1]
    agent.stress = 0.5
    agent.trust = 0.5
    event = _social_event(
        world,
        {
            "interaction_type": "complaint_response",
            "agent_id": agent.id,
            "request_id": request_id,
            "zone_id": agent.zone_id,
            "stress_threshold": 0.1,
            "complaint_wait_minutes": 3,
            "action": action,
            "reason_short": "long_wait",
        },
        causation_id=request_id,
    )
    _social_is_python_s(world, event)
    assert (agent.stress, agent.trust) == (stress, trust)


@pytest.mark.parametrize(
    ("action", "stress", "trust"),
    [
        ("offer_reassurance", 0.5 - 0.03, 0.5 + 0.01),
        ("share_practical_information", 0.5, 0.5 + 0.005),
    ],
)
def test_an_untyped_social_event_is_python_s(
    generated_scenario: Any, action: str, stress: float, trust: float
) -> None:
    world, _ = _small_world(generated_scenario)
    request_id = _open_request(world)
    agent = world.agents[world.support_requests[request_id].agent_id]
    agent.stress = 0.5
    agent.trust = 0.5
    event = _social_event(world, {"agent_id": agent.id, "request_id": request_id, "action": action})
    _social_is_python_s(world, event)
    assert (agent.stress, agent.trust) == (stress, trust)


def test_cooperation_is_python_s(generated_scenario: Any) -> None:
    world, _ = _small_world(generated_scenario)
    first, second = (world.agents[key] for key in sorted(world.agents)[:2])
    second.zone_id = first.zone_id
    presence = world.model_state.get("population_presence_v1")
    if presence is not None:
        presence["people"][first.id]["household_id"] = "home-1"
        presence["people"][second.id]["household_id"] = "home-1"
    first.stress, second.stress, first.trust, second.trust = 0.5, 0.6, 0.5, 0.6
    contact = ScheduledEvent(
        event_id="contact-test",
        kind=EventKind.ENDOGENOUS,
        due_minute=world.minute_index - 1,
        priority=0,
        sequence=900_010,
        event_type="information_contact_update",
        payload={"sender_agent_id": first.id, "recipient_agent_id": second.id},
        source_id="SRC-TEST",
        provenance={"source_type": "assumption", "source_id": "SRC-TEST"},
    )
    world.event_log.append(
        EventLogRecord(
            event=contact, realized_minute=world.minute_index - 1, status=EventStatus.APPLIED
        )
    )
    event = _social_event(
        world,
        {
            "interaction_type": "cooperation",
            "agent_id": first.id,
            "participant_agent_id": second.id,
            "zone_id": first.zone_id,
            "stress_threshold": 0.1,
            "relationship": "same_group",
            "trigger_event_id": "contact-test",
            "action": "offer_mutual_support",
            "reason_short": "shared_group_contact",
        },
        causation_id="contact-test",
    )
    if presence is None:
        pytest.skip("this Scenario has no presence state to put two people in one household")
    _social_is_python_s(world, event)
    assert (first.stress, second.stress) == (0.5 - 0.02, 0.6 - 0.02)
    assert (first.trust, second.trust) == (0.5 + 0.01, 0.6 + 0.01)


def _operation_world(kind: str, zone_id: str) -> tuple[WorldState, ScheduledEvent, str]:
    """A staff member at work on an operation in this zone, due now."""
    document = json.loads(SMALL_REPLAY.read_text(encoding="utf-8"))
    document["schema_version"] = 3
    document["duration_minutes"] = 200
    document["scheduled_events"] = []
    document["model_configs"]["staff_operations"] = {}
    document["model_configs"]["shared_operations"] = {
        "schema_version": 1,
        "policy_version": "shared-operations-v1",
    }
    world = WorldState.from_scenario(ScenarioSpec.model_validate(document))
    initialize_simulator_extensions(world)
    world.minute_index = 10
    staff_id = sorted(world.staff)[0]
    staff = world.staff[staff_id]
    task_id = "staff-operation-test"
    world.active_tasks[task_id] = ActiveTask(
        id=task_id,
        kind="staff_operation",
        agent_id=None,
        resource_id=None,
        start_minute=5,
        due_minute=10,
        staff_id=staff_id,
    )
    staff.active_task_id = task_id
    staff.work_queue[:] = [task_id]
    staff.busy_until = 10
    world.model_state["staff_operations_v1"]["staff"][staff_id].update(
        {
            "phase": "working",
            "task_id": task_id,
            "task_kind": kind,
            "target_id": None,
            "target_zone_id": zone_id,
            "reason": "test",
            "started_minute": 5,
            "arrival_minute": 5,
            "due_minute": 10,
            "causation_event_ids": ["cause-1"],
        }
    )
    event = ScheduledEvent(
        event_id="staff-operation-complete-test",
        kind=EventKind.COMPLETION,
        due_minute=10,
        priority=5,
        sequence=900_011,
        event_type="staff_operation_complete",
        payload={
            "staff_id": staff_id,
            "task_id": task_id,
            "task_kind": kind,
            "target_id": None,
            "target_zone_id": zone_id,
        },
        source_id="SRC-TEST",
        provenance={"source_type": "assumption", "source_id": "SRC-TEST"},
    )
    return world, event, staff_id


@pytest.mark.parametrize(
    ("kind", "digest", "changed"),
    [
        # The zone is as clean as it gets already: the receipt measures, and nothing changed.
        ("cleaning", "7bb434e07202a409", set()),
        ("information_outreach", "989b5abd395d3d49", {"trust"}),
        ("patrol", "dc462459ee25cdcf", {"trust"}),
    ],
)
def test_a_finished_operation_is_python_s(kind: str, digest: str, changed: set[str]) -> None:
    """The receipt and the world are the Python rule's: the digest of what it gave (lane L10)."""
    probe = WorldState.from_scenario(ScenarioSpec.model_validate(_small_replay_document()))
    zone_id = next(iter(sorted(agent.zone_id for agent in probe.agents.values())))
    world, event, _ = _operation_world(kind, zone_id)
    handler = StaffOperationCompletionHandler()
    records = handler.apply(world, event)
    documents = [item.to_dict() for item in records or ()]
    assert _digest({"records": documents, "world": _bytes(world)}) == digest
    changes = documents[0]["event"]["payload"]["state_changes"]
    assert {change["field_path"] for change in changes} == changed
    assert world._core.mirror("staff_operations_v1").verify()


def _small_replay_document() -> dict[str, Any]:
    document = json.loads(SMALL_REPLAY.read_text(encoding="utf-8"))
    document["schema_version"] = 3
    document["scheduled_events"] = []
    return document


# ---- What a draw hides ---------------------------------------------------------------------------


def test_many_draws_of_a_claim_and_a_correction_are_python_s(generated_scenario: Any) -> None:
    """1% in a coefficient flips few draws: these settings put the probability near the middle."""
    world, _ = _small_world(generated_scenario)
    agent_id = sorted(world.agents)[0]
    world.agents[agent_id].trust = 1.0
    rules = native.rules("information_events")
    # The belief's Python rule: 1.0 when the draw is below the clamped probability, else 0.0.
    believing_probability = min(1.0, max(0.0, 0.0 + 2.0 * 0.5 * (1.0 - 0.5) + 0.0))
    believing = InformationSpreadConfig(
        rumor_belief_probability=0.0, rumor_trust_sensitivity=0.5, rumor_source_sensitivity=0.0
    )
    accepting = InformationSpreadConfig(
        correction_acceptance_probability=1.0,
        correction_trust_sensitivity=0.0,
        correction_source_sensitivity=0.0,
    )
    rng = CounterRNG(world.seed)
    # Model version 2 keys the draws by the minute and the people, the source and the message,
    # without the event's ID.
    for number in range(4000):
        message_id = f"claim-{number}"
        belief_draw = rng.random("rumor-belief-acceptance-v1", 7, agent_id, message_id)
        python_strength = 1.0 if belief_draw < believing_probability else 0.0
        assert python_strength == rules.claim_strength_of(
            world, believing, agent_id, "SRC-TEST", 7, message_id
        )
        label = "correction-acceptance-v1"
        drawn = rng.random(label, 7, "SRC-TEST", agent_id, message_id)
        assert (drawn < 0.95) == rules.acceptance_of(
            world, accepting, label, 1.0, "SRC-TEST", 7, ["SRC-TEST", agent_id, message_id]
        )


def test_a_claim_s_draw_keys_a_seed_past_64_bits_as_python_does(generated_scenario: Any) -> None:
    """A Scenario's seed is any int from 0; CounterRNG keys its draws by `str(seed)`."""
    world, _ = _small_world(generated_scenario)
    agent_id = sorted(world.agents)[0]
    world.agents[agent_id].trust = 1.0
    world.seed = 2**70 + 3
    rules = native.rules("information_events")
    believing = InformationSpreadConfig(
        rumor_belief_probability=0.0, rumor_trust_sensitivity=0.5, rumor_source_sensitivity=0.0
    )
    rng = CounterRNG(world.seed)
    for number in range(200):
        message_id = f"claim-{number}"
        drawn = rng.random("rumor-belief-acceptance-v1", 7, agent_id, message_id)
        assert rules.claim_strength_of(world, believing, agent_id, "SRC-TEST", 7, message_id) == (
            1.0 if drawn < 0.5 else 0.0
        )


# What the Python rule gave without configured amounts, by kind: `_care_is_python_s`.
DEFAULT_CARE_DIGESTS = {
    "caregiver_assistance": "6782c3b5aa910906",
    "dependent_assistance": "2b5392f76fd1210e",
    "hygiene_support": "dd9fad59b0f334ee",
    "language_access": "8528e38368e870f6",
    "medical_assessment": "76cc3b0c71350b04",
    "medication_support": "ea57248cd1f89cb5",
    "mobility_assistance": "6694a4446d3f747b",
    "privacy_accommodation": "80b5e9af9dd35591",
}


@pytest.mark.parametrize("kind", TYPED_KINDS)
def test_care_without_configured_amounts_uses_the_formula_s_defaults(
    generated_scenario: Any, kind: str
) -> None:
    registry = generated_scenario.model_configs["policy_registry"]
    registry.pop("suite_id", None)
    registry["care_outcome"] = "typed-care-support-response-v1"
    world, _ = _small_world(generated_scenario)
    configs = world.model_configs["body_dynamics"]
    for name in [key for key in configs if key.startswith(kind) or "assistance" in key]:
        configs.pop(name)
    _care_is_python_s(world, kind, DEFAULT_CARE_DIGESTS[kind])
