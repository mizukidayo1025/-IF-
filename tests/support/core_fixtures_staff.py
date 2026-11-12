"""The fixture of the core's tests of the staff's rules (Issue #225, lane M4).

`native/core/tests/staff_rules.rs` checks the staff's rules of a tick (`rules::staff`: the zones'
sanitation, the routed tasks and their relief, `StaffService.assign_pending`, the waits the metrics
read) and `record_shared_operation` (`rules::common_operations`) on the worlds the pytest files
built: `integration/test_staff_rules.py` and three tests of `integration/test_handler_rest_rules.py`
(the assignment of a request). Those rules have no Python body left (lane L10, `kernel_only`): the
values they are checked against are the deleted rule's formula, as the pytest files wrote it, or a
digest of what it gave, which the core's test makes again.

`staff_rules.json` is `{"worlds": {...}, "service": {...}, "tests": {group: ...}}`:

- A world is `core_fixtures.export_world`'s entry of `staff_world()` (`plain`), or a variant of
  another world of the file, `{"base": name, "changes": [...]}` (`core_texts.changes`; read back by
  `native/core/tests/support/cases.rs`), each made by the edits the pytest made.
- `service` is what `assign_pending` takes of a `StaffService(world)` of no route.
- A group holds what one test function needs besides its worlds: the inputs that Python's
  `random.Random` drew, the deleted rule's values, and the receipts (`model_dump(mode="json")`)
  with the records of the EventLog they need.
"""

from __future__ import annotations

import contextlib
import json
import random
from collections.abc import Iterator
from typing import Any

from support.core_fixtures import export_world
from support.core_texts import changes
from support.paths import SMALL_REPLAY

KEY = "staff_operations_v1"
SHARED = "shared_operations_v1"


# ---- The worlds ---------------------------------------------------------------------------------


def staff_world(
    *, skills: tuple[str, ...] = ("support",), configs: dict[str, Any] | None = None
) -> Any:
    """The small replay at minute 0 with three staff members (`S01`..`S03`) of these skills, the
    staff's and the shared operations' states, and these configs besides."""
    from sheltersim.core.world import WorldState
    from sheltersim.domain.simulator_state import initialize_simulator_extensions
    from sheltersim.experiments.scenario import ScenarioSpec

    document = json.loads(SMALL_REPLAY.read_text(encoding="utf-8"))
    document["schema_version"] = 3
    document["duration_minutes"] = 200
    document["scheduled_events"] = []
    document["model_configs"]["staff_operations"] = {}
    document["model_configs"]["shared_operations"] = {
        "schema_version": 1,
        "policy_version": "shared-operations-v1",
    }
    document["model_configs"].update(configs or {})
    template = document["staff"][0]
    document["staff"] = [
        {**template, "id": f"S{index:02}", "skills": list(skills)} for index in range(1, 4)
    ]
    world = WorldState.from_scenario(ScenarioSpec.model_validate(document))
    initialize_simulator_extensions(world)
    return world


@contextlib.contextmanager
def _kept_tree() -> Iterator[None]:
    """`keep_trees(SHARED)` of the pytest file: the worlds made inside hold that tree."""
    from sheltersim.core import state_mirror

    detached = state_mirror.DETACHED_NAMESPACES
    state_mirror.DETACHED_NAMESPACES = detached - {SHARED}
    try:
        yield
    finally:
        state_mirror.DETACHED_NAMESPACES = detached


class _File:
    def __init__(self) -> None:
        self.worlds: dict[str, Any] = {}
        self.entries: dict[str, Any] = {}
        self.tests: dict[str, Any] = {}

    def world(self, name: str, world: Any, base: str | None = None) -> str:
        """Add a world, whole without `base`, else as the changes from `base`."""
        assert name not in self.entries, name
        entry = json.loads(json.dumps(export_world(world, validate=False)))
        self.entries[name] = entry
        if base is None:
            self.worlds[name] = entry
        else:
            self.worlds[name] = {"base": base, "changes": changes(self.entries[base], entry)}
        return name


def _zones(world: Any) -> list[str]:
    return sorted(world.zones)


def _text(value: Any) -> str:
    """The deleted rule's zones as the core's test writes them: sorted keys, float bits shown."""
    return json.dumps(value, sort_keys=True)


def _request(world: Any, number: int, agent_id: str, minute: int = 0) -> Any:
    from sheltersim.domain.models import SupportRequestState

    request = SupportRequestState(
        id=f"support-{number}",
        agent_id=agent_id,
        requested_minute=minute,
        timeout_minute=world.duration_minutes - 1,
    )
    world.support_requests[request.id] = request
    return request


def _head(request: Any, kind: str, event_id: str) -> dict[str, Any]:
    return {
        "request_id": request.id,
        "agent_id": request.agent_id,
        "household_id": None,
        "support_kind": kind,
        "status": "requested",
        "created_minute": request.requested_minute,
        "updated_minute": request.requested_minute,
        "latest_event_id": event_id,
    }


def _care(world: Any, request: Any, kind: str) -> None:
    world.model_state[SHARED]["support_requests"][request.id] = _head(
        request, kind, f"care-{request.id}"
    )


def _busy(world: Any, staff_id: str) -> None:
    world.staff[staff_id].active_task_id = f"task-{staff_id}"


def _requested(
    file: _File, name: str, skills: tuple[str, ...], configs: dict[str, Any] | None = None
) -> str:
    """A base of the random rounds, to keep their changes small: the world of these skills and
    configs with a request of each person at minute 0 and every worker busy."""
    if name not in file.entries:
        world = staff_world(skills=skills, configs=configs)
        for index, agent_id in enumerate(sorted(world.agents)):
            _request(world, index, agent_id)
        for staff_id in world.staff:
            _busy(world, staff_id)
        file.world(name, world, "plain")
    return name


# ---- update_staff_operations --------------------------------------------------------------------


def _python_sanitation(world: Any, state: dict[str, Any], minute: int) -> dict[str, Any]:
    """The zones as `update_staff_operations` left them: each zone's sanitation falls by
    `load * rate * elapsed`, down to 0.0, and its `updated_minute` is the minute."""
    zones = json.loads(json.dumps(state["zones"]))
    config = state["config"]
    for zone_id in sorted(world.zones):
        load = sum(agent.zone_id == zone_id for agent in world.agents.values())
        zone = zones[zone_id]
        elapsed = max(0, minute - zone["updated_minute"])
        rate = (
            config["effects_by_zone"]
            .get(zone_id, {})
            .get("sanitation_use_per_person_minute", config["sanitation_use_per_person_minute"])
        )
        zone["sanitation"] = max(0.0, zone["sanitation"] - load * rate * elapsed)
        zone["updated_minute"] = minute
    return zones


def _sanitation(file: _File) -> None:
    from sheltersim.domain.staff_operations import update_staff_operations

    # test_the_sanitation_is_the_python_rules_on_random_rates: 40 rounds of random rates, each
    # updated at 4 random minutes.
    generator = random.Random(7)
    rounds = []
    for number in range(40):
        rate = generator.uniform(0.0, 1.0) ** 3
        zone_rate = generator.uniform(0.0, 0.01)
        world = staff_world()
        zone = generator.choice(_zones(world))
        config = world.model_state[KEY]["config"]
        config["sanitation_use_per_person_minute"] = rate
        config["effects_by_zone"] = {zone: {"sanitation_use_per_person_minute": zone_rate}}
        name = file.world(f"sanitation_{number}", world, "plain")
        minute, steps = 0, []
        for _step in range(4):
            minute += generator.randrange(0, 90)
            expected = _python_sanitation(world, world.model_state[KEY], minute)
            update_staff_operations(world, minute)
            steps.append([minute, _text(expected)])
        rounds.append({"world": name, "steps": steps})
    file.tests["sanitation_random_rates"] = rounds

    # test_a_rate_of_one_percent_more_shows.
    world = staff_world()
    world.model_state[KEY]["config"]["sanitation_use_per_person_minute"] = 0.000005 * 1.01
    file.world("rate_one_percent_more", world, "plain")

    # test_an_int_rate_the_config_takes_is_the_python_rule_s.
    cases = []
    for number, (key, value) in enumerate(
        (("global", 0), ("global", 1), ("zone", 0)),
    ):
        world = staff_world()
        zone = _zones(world)[0]
        config = world.model_state[KEY]["config"]
        if key == "global":
            config["sanitation_use_per_person_minute"] = value
        else:
            config["effects_by_zone"] = {zone: {"sanitation_use_per_person_minute": value}}
        for agent in world.agents.values():
            agent.zone_id = zone
        world.model_state[KEY]["zones"][zone]["sanitation"] = 0.75
        expected = _python_sanitation(world, world.model_state[KEY], 50)
        name = file.world(f"int_rate_{number}", world, "plain")
        cases.append({"world": name, "minute": 50, "zones": _text(expected)})
    file.tests["int_rate"] = cases

    # test_a_config_without_its_table_changes_nothing_and_is_refused.
    world = staff_world()
    world.model_state[KEY]["config"].pop("effects_by_zone")
    file.world("no_zone_table", world, "plain")


# ---- assign_staff_operations, schedule_staff_operation_relief -----------------------------------


def _set_stress(world: Any, zone: str, stress: float) -> None:
    for agent in world.agents.values():
        agent.zone_id = zone
        agent.stress = stress


def _operations(file: _File) -> None:
    from sheltersim.domain.body_state import canonical_body_config

    # What each world's `assign_staff_operations` reads: "routes" (a free worker has a task to
    # take, so the router is read), "none" (nothing to start), "refused".
    cases: dict[str, list[Any]] = {}

    def case(group: str, name: str, world: Any, base: str, expected: str) -> None:
        file.world(name, world, base)
        cases.setdefault(group, []).append({"world": name, "expected": expected})

    # test_a_free_worker_with_a_task_reads_the_router_and_without_one_doesnt.
    patrol = file.world("patrol", staff_world(skills=("patrol",)), "plain")
    for number, (threshold, stress, expected) in enumerate(
        ((None, 0.6, "routes"), (None, 0.5999999, "none"), (1, 0.9, "none"), (1, 1.0, "routes"))
    ):
        world = staff_world(skills=("patrol",))
        zone = _zones(world)[0]
        if threshold is not None:
            world.model_state[KEY]["config"]["thresholds_by_zone"] = {
                zone: {"patrol_stress_at_least": threshold}
            }
        _set_stress(world, zone, stress)
        case("patrol", f"patrol_{number}", world, patrol, expected)

    # test_the_cleaning_threshold_is_the_rules.
    cleaning = file.world("cleaning", staff_world(skills=("cleaning",)), "plain")
    for number, (sanitation, taken, expected) in enumerate(
        ((0.85, False, "none"), (0.8499999, False, "routes"), (0.8499999, True, "none"))
    ):
        world = staff_world(skills=("cleaning",))
        zone = _zones(world)[0]
        state = world.model_state[KEY]
        state["zones"][zone]["sanitation"] = sanitation
        if taken:
            state["staff"]["S01"] = {
                **state["staff"]["S01"],
                "phase": "working",
                "task_id": "t",
                "task_kind": "cleaning",
                "target_zone_id": zone,
                "reason": "r",
                "started_minute": 0,
                "arrival_minute": 0,
                "due_minute": 9,
            }
        case("cleaning", f"cleaning_{number}", world, cleaning, expected)

    # test_the_outreach_threshold_is_the_rules.
    configs = {
        "body_dynamics": canonical_body_config(None),
        "policy_registry": {"schema_version": 2, "psychology": "life-state-feedback-v1"},
    }
    outreach = file.world(
        "outreach", staff_world(skills=("information",), configs=configs), "plain"
    )
    for number, (uncertainty, expected) in enumerate(((0.45, "routes"), (0.4499999, "none"))):
        world = staff_world(skills=("information",), configs=configs)
        zone = _zones(world)[0]
        for agent in world.agents.values():
            agent.zone_id = zone
        for record in world.model_state["body_state_v1"]["agents"].values():
            record["uncertainty"] = uncertainty
        case("outreach", f"outreach_{number}", world, outreach, expected)

    # test_a_repair_reads_the_router_and_another_formula_goes_to_the_python_rule.
    repair = file.world("maintenance", staff_world(skills=("maintenance",)), "plain")
    cases.setdefault("repair", []).append({"world": repair, "expected": "none"})
    world = staff_world(skills=("maintenance",))
    facility = next(iter(world.facilities.values()))
    facility.working_units = facility.capacity - 1
    case("repair", "repair_degraded", world, repair, "routes")
    world = staff_world(skills=("maintenance",))
    world.model_configs["policy_registry"] = {"staff_operations": "a-later-formula"}
    case("repair", "repair_later_formula", world, repair, "refused")
    file.tests["operations"] = cases

    # test_a_tick_with_nothing_to_start_changes_nothing.
    names = []
    for skill in ("support", "patrol", "cleaning", "repair", "x"):
        name = {"support": "plain", "patrol": patrol, "cleaning": cleaning}.get(skill)
        names.append(name or file.world(f"skills_{skill}", staff_world(skills=(skill,)), "plain"))
    file.tests["nothing_to_start"] = names

    # test_a_task_with_no_id_sends_the_relief_to_the_python_rule.
    world = staff_world()
    state = world.model_state[KEY]
    state["staff"]["S01"] = {**state["staff"]["S01"], "phase": "working"}
    file.world("working_without_task", world, "plain")


# ---- StaffService.assign_pending ----------------------------------------------------------------


def _waiting(file: _File) -> None:
    from sheltersim.domain.staff import _CARE_SKILL_ALTERNATIVES

    kinds = (*_CARE_SKILL_ALTERNATIVES, "a_kind_of_no_table")

    # test_requests_that_stay_pending_are_the_python_rules: 30 rounds of random skills and kinds,
    # every request pending and every worker busy.
    generator = random.Random(11)
    rounds = []
    for number in range(30):
        skills = generator.choice([("support",), ("nursing",), ("hygiene",)])
        world = staff_world(skills=skills)
        people = sorted(world.agents)
        chosen = [generator.choice(kinds[:-1]) for _ in people]
        for index, (agent_id, kind) in enumerate(zip(people, chosen)):
            request = _request(world, index, agent_id)
            if index % 2:
                _care(world, request, kind)
        for staff_id in world.staff:
            _busy(world, staff_id)
        base = _requested(file, f"pending_{skills[0]}", skills)
        rounds.append(file.world(f"pending_{number}", world, base))
    file.tests["pending_rounds"] = rounds

    # test_the_wait_counts_staff_off_shift_away_and_busy_as_the_python_rule_does.
    windows = {"S01": [{"start_minute": 100, "end_minute": 110}]}
    world = staff_world(configs={"staff_dynamics": {"shift_windows_by_staff": windows}})
    for staff_id in ("S02", "S03"):
        _busy(world, staff_id)
    _request(world, 1, sorted(world.agents)[0])
    file.world("off_shift", world, "plain")

    # test_a_free_worker_goes_past_the_plain_kernel.
    world = staff_world()
    _request(world, 1, sorted(world.agents)[0])
    file.world("free_worker", world, "plain")

    # test_what_the_plain_kernel_doesnt_decide_goes_to_assign_work.
    world = staff_world()
    for staff_id in world.staff:
        _busy(world, staff_id)
    request = _request(world, 1, sorted(world.agents)[0])
    busy = file.world("all_busy", world, "plain")
    world.model_state["llm"] = {"support_priorities": {request.id: 1}}
    file.world("priority_int", world, busy)
    world.model_state["llm"] = {"support_priorities": {request.id: True}}
    file.world("priority_bool", world, busy)

    # test_each_kind_asks_for_the_skills_of_the_table: a world of each skill, all busy, with one
    # request; the core's test puts a head of each kind in the row.
    every_skill = sorted(
        {skill for skills in _CARE_SKILL_ALTERNATIVES.values() for skill in skills}
    )
    skills = []
    for skill in [*every_skill, "other"]:
        world = staff_world(skills=(skill,))
        for staff_id in world.staff:
            _busy(world, staff_id)
        request = _request(world, 1, sorted(world.agents)[0])
        skills.append(file.world(f"kind_skill_{skill}", world, busy))
    heads = {kind: _head(request, kind, "event-1") for kind in kinds}
    file.tests["kinds"] = {
        "skills": skills,
        "heads": heads,
        # Whether a worker of each skill qualifies: the table's skills, "support" for a kind it
        # does not name, and emotional support without a head.
        "qualifies": {
            kind: [
                skill in _CARE_SKILL_ALTERNATIVES.get(kind, {"support"})
                for skill in [*every_skill, "other"]
            ]
            for kind in kinds
        },
    }

    # test_the_reasons_are_the_python_rules_on_random_staff: 120 rounds of random staff, rests,
    # shifts and requests.
    from sheltersim.domain.models import Activity, SupportRequestStatus

    generator = random.Random(23)
    rounds = []
    for number in range(120):
        end = generator.choice([1, 60])
        skills = generator.choice([("support",), ("nursing", "support"), ("privacy",)])
        configs = {
            "staff_dynamics": {
                "shift_windows_by_staff": {"S01": [{"start_minute": 0, "end_minute": end}]}
            }
        }
        world = staff_world(skills=skills, configs=configs)
        minute = generator.randrange(0, 90)
        world.minute_index = minute
        operations = world.model_state[KEY]["staff"]
        dynamics = world.model_state["staff_dynamics_v1"]["staff"]
        for staff_id in world.staff:
            choice = generator.randrange(5)
            _busy(world, staff_id)
            if choice == 1:
                operations[staff_id] = {**operations[staff_id], "phase": "working"}
            elif choice == 2:
                dynamics[staff_id]["rest_until_minute"] = minute + 5
            elif choice == 3:
                dynamics[staff_id]["rest_until_minute"] = minute
            elif choice == 4:
                world.staff[staff_id].active_task_id = None
                dynamics[staff_id]["rest_until_minute"] = minute + 1
        people = sorted(world.agents)
        for index, agent_id in enumerate(people[:6]):
            request = _request(world, index, agent_id, generator.randrange(0, minute + 1))
            if index % 2:
                _care(world, request, generator.choice(kinds[:-1]))
            if index % 3 == 2:
                request.status = SupportRequestStatus.ASSIGNED
                request.task_id = f"task-{index}"
                world.agents[agent_id].activity = generator.choice(
                    [Activity.MOVING, Activity.SEEKING_HELP]
                )
        base = _requested(file, f"reasons_{'_'.join(skills)}_{end}", skills, configs)
        rounds.append(file.world(f"reasons_{number}", world, base))
    file.tests["reasons_rounds"] = rounds

    # test_a_free_worker_s_route_is_read_as_the_python_rule_reads_it: the world `free_worker`.

    # test_handler_rest_rules.py's assignments: `_staffed_world` (this world at minute 10) with
    # `_open_request`: one request of the first person, made now or 5 minutes before.
    for name, age in (("staffed_request", 0), ("staffed_old_request", 5)):
        file.world(name, _staffed(age), "plain")
    # test_nobody_free_leaves_the_plain_kernel_running: every worker on another task.
    world = _staffed(0)
    for person in world.staff.values():
        person.active_task_id = "elsewhere"
    file.world("staffed_nobody_free", world, "staffed_request")


def _staffed(age: int) -> Any:
    from sheltersim.domain.models import Activity, SupportRequestState

    world = staff_world()
    world.minute_index = 10
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
    return world


# ---- record_shared_operation --------------------------------------------------------------------


def _logged(made: Any) -> dict[str, Any]:
    """A receipt and the record of the EventLog it needs, as JSON."""
    record, receipt = made
    return json.loads(
        json.dumps({"record": record.to_dict(), "receipt": receipt.model_dump(mode="json")})
    )


def _receipt(world: Any, number: int, status: str, **fields: Any) -> dict[str, Any]:
    """`_receipt` of the pytest file: the receipt's record appended to the EventLog."""
    from support.receipts import care_transition_record

    made = care_transition_record(
        world,
        request_id=f"request-{number}",
        agent_id=fields.pop("agent_id", sorted(world.agents)[0]),
        household_id=None,
        support_kind="emotional_support",
        status=status,
        **fields,
    )
    world.event_log.append(made[0])
    return {"minute": world.minute_index, **_logged(made)}


def _receipts(file: _File) -> None:
    from sheltersim.contracts.operations import NumericStateChange
    from sheltersim.domain.common_operations import record_shared_operation

    def record(world: Any, logged: dict[str, Any]) -> None:
        from sheltersim.contracts.operations import CareTransitionReceipt, FacilityRecoveryReceipt

        kind = (
            FacilityRecoveryReceipt
            if logged["receipt"]["event_type"] == "facility_recovery_transition"
            else CareTransitionReceipt
        )
        record_shared_operation(world, kind.model_validate(logged["receipt"]))

    # test_a_request_s_receipts_are_the_python_rules_and_kept_in_the_row: four receipts of one
    # request, one minute apart.
    world = staff_world()
    agent = sorted(world.agents)[0]
    stress_change = NumericStateChange(
        entity_id=agent,
        entity_kind="agent",
        field_path="stress",
        before=0.5,
        after=world.agents[agent].stress,
        unit="index",
    )
    steps = []
    for status, fields in (
        ("requested", {}),
        ("assigned", {"staff_id": "S01"}),
        ("started", {"staff_id": "S01"}),
        (
            "completed",
            {
                "staff_id": "S01",
                "outcome_observation": "observed_change",
                "state_changes": (stress_change,),
            },
        ),
    ):
        world.minute_index += 1
        logged = _receipt(world, 1, status, **fields)
        record(world, logged)
        steps.append(logged)
    file.tests["request_receipts"] = steps

    # test_a_receipt_the_rule_refuses_changes_nothing_and_has_its_text and
    # test_an_order_or_identity_the_rule_refuses_is_its_own: at minute 3.
    world = staff_world()
    world.minute_index = 3
    first = _receipt(world, 1, "requested")
    record(world, first)
    file.tests["refused"] = {
        "first": first,
        "completed": _receipt(world, 1, "completed"),
        "other_request": _receipt(world, 2, "assigned"),
        "other_agent": _receipt(world, 1, "assigned", agent_id=sorted(world.agents)[1]),
    }

    # test_a_state_change_that_is_not_the_world_s_is_the_rules_to_refuse.
    world = staff_world()
    world.minute_index = 3
    started = []
    for status in ("requested", "started"):
        logged = _receipt(world, 1, status, staff_id=None)
        record(world, logged)
        started.append(logged)
    change = NumericStateChange(
        entity_id=agent,
        entity_kind="agent",
        field_path="stress",
        before=0.5,
        after=world.agents[agent].stress + 0.25,
        unit="index",
    )
    completed = _receipt(
        world, 1, "completed", outcome_observation="observed_change", state_changes=(change,)
    )
    file.tests["state_change"] = {"before": started, "completed": completed}

    # test_a_facility_s_recovery_receipt_is_the_rule_s and
    # test_a_recovery_receipt_of_another_state_is_left_to_the_rule_s_text.
    from support.receipts import facility_recovery_record

    world = staff_world()
    world.minute_index = 3
    facility = next(iter(world.facilities.values()))
    made = facility_recovery_record(
        world,
        facility_id=facility.id,
        component_id="component",
        status="restored",
        previous_working_units=0,
        working_units=facility.working_units,
        causation_id="cause",
    )
    file.tests["recovery"] = {"minute": 3, **_logged(made)}
    facility.working_units -= 1
    file.world("recovery_degraded", world, "plain")


# ---- The file -----------------------------------------------------------------------------------


def staff_rules_fixture() -> dict[str, Any]:
    """`staff_rules.json`."""
    from sheltersim.domain.staff import StaffService

    file = _File()
    with _kept_tree():
        world = staff_world()
        file.world("plain", world)
        service = StaffService(world)
        # The order of the keys of the tree Python made of the namespace, which keeps it.
        file.tests["shared_keys"] = list(world.model_state[SHARED])
        _sanitation(file)
        _operations(file)
        _waiting(file)
        _receipts(file)
    return {
        "worlds": file.worlds,
        "service": {
            "policy": service.policy_version,
            "task_minutes": service.task_duration_minutes,
            "fairness_after": service.fairness_after_minutes,
        },
        "tests": file.tests,
    }
