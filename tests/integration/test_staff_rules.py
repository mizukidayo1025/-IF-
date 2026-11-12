"""staff_operations_v1 and shared_operations_v1 owned by Rust: the boundary of the staff's rules.

native/src/rules/staff.rs and common_operations.rs are the glue of the core's rules of the staff's
assignment and a care receipt (docs/rust-port/lanes/m4-staff.md, l2-staff.md). Those rules have no
Python body left (lane L10, `kernel_only`); the core's tests check them on the worlds this file
built, against the Python rule's formula or a digest of what it gave
(`native/core/tests/staff_rules.rs`, #225). This file keeps what only Python can check: the rows
and their trees, a taint, a restore, the comparisons with the Python validators still in src, and
one test of each kernel's entry, which still reaches the kernel and gives its answer.
"""

from __future__ import annotations

import copy
import hashlib
import json
from typing import Any

import pytest
from support.core_fixtures_staff import staff_world
from support.receipts import care_transition_record

from sheltersim import native
from sheltersim.contracts.operations import SHARED_OPERATIONS_KEY
from sheltersim.core import state_mirror
from sheltersim.core.kernels import KernelRefusal
from sheltersim.core.state_mirror import rust_validated
from sheltersim.core.world import WorldState
from sheltersim.domain.common_operations import (
    _TICK_PASSED,
    _TICK_VALID,
    record_shared_operation,
    validate_shared_operations_state,
)
from sheltersim.domain.models import SupportRequestState, SupportRequestStatus
from sheltersim.domain.staff import StaffService
from sheltersim.domain.staff_operations import (
    STAFF_OPERATIONS_STATE_KEY,
    assign_staff_operations,
    schedule_staff_operation_relief,
    update_staff_operations,
    validate_staff_operations_state,
)
from sheltersim.domain.zones import ZoneGraph
from sheltersim.experiments import metrics
from sheltersim.policies.space import SpaceRouter

KEY = STAFF_OPERATIONS_STATE_KEY
SHARED = SHARED_OPERATIONS_KEY


@pytest.fixture(autouse=True)
def _trees(keep_trees: Any) -> None:
    """These tests pin the tree the kernel writes beside the row, key order and all: the
    namespace keeps its tree here (support/world.py keep_trees)."""
    keep_trees(SHARED)


_world = staff_world


def _twins(**options: Any) -> tuple[WorldState, WorldState]:
    return _world(**options), _world(**options)


def _text(value: Any) -> str:
    """Float bits, 1 against 1.0, and key order all show."""
    return json.dumps(value)


def _mirror(world: WorldState, key: str = KEY) -> Any:
    return world._core.mirror(key)


def _zones(world: WorldState) -> list[str]:
    return sorted(world.zones)


# ---- The rows ----


def test_the_rows_write_the_bytes_of_their_trees() -> None:
    world = _world()
    for key in (KEY, SHARED):
        assert _mirror(world, key).has_row
        assert _mirror(world, key).row_json() == native.canonical_json(world.model_state[key])


def test_a_forwarded_task_reaches_the_row_and_a_finished_task_too() -> None:
    world = _world()
    state = world.model_state[KEY]
    record = dict(state["staff"]["S01"])
    state["staff"]["S01"] = {**record, "phase": "travel", "task_id": "t", "task_kind": "patrol"}
    state["staff"]["S01"]["phase"] = "working"
    state["staff"]["S01"]["route_edge_ids"].append("e")
    assert _mirror(world).verify()
    state["history"].append(
        {
            "task_id": "t",
            "staff_id": "S01",
            "task_kind": "patrol",
            "target_id": None,
            "target_zone_id": _zones(world)[0],
            "reason": "r",
            "status": "completed",
            "start_minute": 0,
            "arrival_minute": 0,
            "complete_minute": 1,
            "travel_minutes": 0,
            "causation_event_ids": ["e"],
        }
    )
    assert _mirror(world).verify()


def test_a_value_the_row_cant_hold_taints_it_and_the_rule_refuses() -> None:
    world = _world()
    zone = _zones(world)[0]
    world.model_state[KEY]["zones"][zone]["sanitation"] = 1
    assert not _mirror(world).has_row
    with pytest.raises(native.Unsupported):
        native.rules("staff").update_operations(world, 5)
    before = _text(world.model_state[KEY])
    with pytest.raises(KernelRefusal):
        update_staff_operations(world, 5)
    assert _text(world.model_state[KEY]) == before


# ---- update_staff_operations ----


def test_a_rate_of_one_percent_more_shows() -> None:
    """The entry of `update_staff_operations`."""
    world, other = _twins()
    other.model_state[KEY]["config"]["sanitation_use_per_person_minute"] = 0.000005 * 1.01
    update_staff_operations(world, 100)
    update_staff_operations(other, 100)
    assert _text(world.model_state[KEY]["zones"]) != _text(other.model_state[KEY]["zones"])


def test_a_config_without_its_table_changes_nothing_and_is_refused() -> None:
    """`config["effects_by_zone"]`: the Python rule raised KeyError for a config no rule writes.
    The core's refusal is Unsupported, and KernelRefusal from the Python step."""
    world = _world()
    world.model_state[KEY]["config"].pop("effects_by_zone")
    before = _text(world.model_state[KEY])
    with pytest.raises(native.Unsupported):
        native.rules("staff").update_operations(world, 50)
    with pytest.raises(KernelRefusal):
        update_staff_operations(world, 50)
    assert _text(world.model_state[KEY]) == before


# ---- assign_staff_operations, schedule_staff_operation_relief ----


@pytest.mark.parametrize("skills", [("support",), ("patrol",), ("cleaning",), ("repair",), ("x",)])
def test_a_tick_with_nothing_to_start_changes_nothing(skills: tuple[str, ...]) -> None:
    """The entries of `assign_staff_operations` and `schedule_staff_operation_relief`."""
    world = _world(skills=skills)
    before = world.state_digest()
    assert native.rules("staff").relief(world, 3) == ()
    assert schedule_staff_operation_relief(world, 3) == ()
    assert native.rules("staff").assign_operations(world, 3, object()) == ()
    assert world.state_digest() == before
    graph = ZoneGraph.from_states(world.zones, world.zone_edges)
    assert assign_staff_operations(world, 3, SpaceRouter(graph)) == ()
    assert world.state_digest() == before


# ---- The validator ----


def test_the_validator_accepts_a_good_state_and_nothing_python_turns_down() -> None:
    world = _world()
    assert rust_validated(world, KEY)
    zone = _zones(world)[0]
    spoils = [
        lambda state: state["zones"][zone].__setitem__("sanitation", 1.5),
        lambda state: state["zones"][zone].__setitem__("updated_minute", -1),
        lambda state: state.__setitem__("schema_version", 2),
        lambda state: state.__setitem__("policy_version", "other"),
        lambda state: state.__setitem__("source_id", "other"),
        lambda state: state["zones"].pop(zone),
        lambda state: state["staff"].pop("S01"),
        lambda state: state["staff"]["S01"].__setitem__("reason", "kept"),
        lambda state: state["staff"]["S01"].__setitem__("travel_minutes", 3),
        lambda state: state["staff"]["S01"]["causation_event_ids"].append("e"),
        lambda state: state["config"].pop("cleaning_sanitation_recovery"),
        lambda state: state["config"].__setitem__("patrol_reassurance", 1.5),
        lambda state: state["config"].__setitem__("patrol_reassurance", True),
        lambda state: state["config"].__setitem__("patrol_reassurance", float("nan")),
        lambda state: state["config"].__setitem__("an_unknown_key", 1),
        lambda state: state["config"]["task_duration_minutes"].__setitem__("patrol", 0),
        lambda state: state["config"]["task_duration_minutes"].__setitem__("patrol", 15.0),
        lambda state: state["config"]["task_duration_minutes"].pop("patrol"),
        lambda state: state["config"].__setitem__(
            "thresholds_by_zone", {"nowhere": {"patrol_stress_at_least": 0.5}}
        ),
        lambda state: state["config"].__setitem__("thresholds_by_zone", {zone: {}}),
        lambda state: state["config"].__setitem__(
            "thresholds_by_zone", {zone: {"patrol_stress_at_least": 2}}
        ),
        lambda state: state["config"].__setitem__(
            "effects_by_zone", {zone: {"sanitation_use_per_person_minute": 0.02}}
        ),
        lambda state: state["history"].append(_finished(zone, staff_id="nobody")),
        lambda state: state["history"].append(_finished(zone, task_kind="other")),
        lambda state: state["history"].append(_finished(zone, status="other")),
        lambda state: state["history"].append(_finished(zone, target_zone_id="nowhere")),
        lambda state: state["history"].append(_finished(zone, target_id="nothing")),
        lambda state: state["history"].append(_finished(zone, start_minute=-1)),
        lambda state: state["history"].append(_finished(zone, complete_minute=1, start_minute=2)),
        lambda state: state["history"].append(_finished(zone, arrival_minute=None)),
        lambda state: state["history"].append(_finished(zone, arrival_minute=9)),
        lambda state: state["history"].append(_finished(zone, causation_event_ids=[""])),
    ]
    for spoil in spoils:
        spoiled = _world()
        spoil(spoiled.model_state[KEY])
        with pytest.raises((ValueError, KeyError, TypeError)):
            validate_staff_operations_state(spoiled)
        assert not rust_validated(spoiled, KEY)


def _finished(zone: str, **fields: Any) -> dict[str, Any]:
    return {
        "task_id": "t",
        "staff_id": "S01",
        "task_kind": "patrol",
        "target_id": None,
        "target_zone_id": zone,
        "reason": "r",
        "status": "completed",
        "start_minute": 0,
        "arrival_minute": 0,
        "complete_minute": 1,
        "travel_minutes": 0,
        "causation_event_ids": ["e"],
        **fields,
    }


def test_the_validator_takes_good_finished_tasks_an_older_config_and_leaves_a_task_to_python() -> (
    None
):
    world = _world()
    zone = _zones(world)[0]
    state = world.model_state[KEY]
    state["history"].append(_finished(zone))
    state["history"].append(
        _finished(zone, status="interrupted", arrival_minute=None, travel_minutes=2, reason="")
    )
    validate_staff_operations_state(world)
    assert rust_validated(world, KEY)
    del state["config"]["thresholds_by_zone"]
    del state["config"]["effects_by_zone"]
    validate_staff_operations_state(world)
    assert rust_validated(world, KEY)
    state["staff"]["S01"] = {**state["staff"]["S01"], "phase": "working"}
    assert not rust_validated(world, KEY)


# ---- StaffService.assign_pending ----


def _request(world: WorldState, number: int, agent_id: str, minute: int = 0) -> SupportRequestState:
    request = SupportRequestState(
        id=f"support-{number}",
        agent_id=agent_id,
        requested_minute=minute,
        timeout_minute=world.duration_minutes - 1,
    )
    world.support_requests[request.id] = request
    return request


def _waiting_fields(world: WorldState) -> list[Any]:
    return [
        (
            request.id,
            request.last_assignment_attempt_minute,
            request.last_assignment_wait_reason_code,
        )
        for request in world.support_requests.values()
    ]


def test_a_free_worker_goes_past_the_plain_kernel() -> None:
    world = _world()
    _request(world, 1, sorted(world.agents)[0])
    service = StaffService(world)
    before = (_waiting_fields(world), _text(dict(world.metric_state)))
    # The entries of `assign_none` and `assign_pending`; with the validation `assign_none` leaves
    # the step to `assign_pending`.
    assert native.rules("staff").assign_none(service, 0) is None
    assert native.rules("staff").assign_none(service, 0, validate_state=False) is None
    assert (_waiting_fields(world), _text(dict(world.metric_state))) == before
    # No route is known here, so it is assigned where it stands.
    assert service.assign_pending(0, validate_state=False) == ("support-1",)


# ---- What the metrics read ----
#
# `_support_wait_snapshot` and `record_shared_operation` have no Python rule left (lane L10,
# `kernel_only`): what they gave here was recorded from them, as digests (`_digest`).


def _digest(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()[:16]


def _both_snapshots(world: WorldState, minute: int) -> str:
    active = sorted(
        (
            request
            for request in world.support_requests.values()
            if request.status in (SupportRequestStatus.PENDING, SupportRequestStatus.ASSIGNED)
        ),
        key=lambda request: request.id,
    )
    return _text(metrics._support_wait_snapshot(world, active, minute))


def test_a_free_worker_s_route_is_read_as_the_python_rule_reads_it() -> None:
    """The entry of `_support_wait_snapshot`."""
    world = _world()
    _request(world, 1, sorted(world.agents)[0])
    text = _both_snapshots(world, 0)
    assert json.loads(text)[0]
    assert _digest(text) == "af5c0d0aa04d98b8"


# ---- record_shared_operation ----


def _receipt(world: WorldState, number: int, status: str, **fields: Any) -> Any:
    event_record, receipt = care_transition_record(
        world,
        request_id=f"request-{number}",
        agent_id=sorted(world.agents)[0],
        household_id=None,
        support_kind="emotional_support",
        status=status,
        **fields,
    )
    world.event_log.append(event_record)
    return receipt


def _record(world: Any, receipt: Any) -> None:
    native.rules("common_operations").record_shared_operation(world, receipt)


def _restored(world: WorldState) -> WorldState:
    """The world copied as plain values and owned again by a new core, as a restore owns them:
    the rows read from the values, with nothing verified."""
    copied = object.__new__(WorldState)
    memo: dict[int, Any] = {}
    for key, value in world.__dict__.items():
        if key != "_core":
            copied.__dict__[key] = copy.deepcopy(value, memo)
    state_mirror.start_core(copied)
    return copied


def test_a_restored_receipt_is_read_by_the_python_rule_once_and_then_trusted() -> None:
    world = _world()
    world.minute_index = 3
    record_shared_operation(world, _receipt(world, 1, "requested"))
    request = _request(world, 1, sorted(world.agents)[0], 3)
    del world.support_requests[request.id]
    world.support_requests["request-1"] = SupportRequestState(
        id="request-1",
        agent_id=sorted(world.agents)[0],
        requested_minute=3,
        timeout_minute=99,
    )
    assert _TICK_VALID(world)

    # A restore reads the receipts from their documents, unverified. (deepcopy forks the rows,
    # with what was verified of them, since lane 6-6 of #152, so it no longer stands for one.)
    copied = _restored(world)
    assert not _TICK_VALID(copied)
    # What the kernel did not check, the contract does, as in the Python rule.
    _record(copied, _receipt(copied, 2, "requested"))
    assert _mirror(copied, SHARED).verify()
    assert _digest(json.dumps(copied.model_state[SHARED])) == "7de6086f49c6ebd0"
    world_two = _restored(world)
    assert not _TICK_VALID(world_two)
    validate_shared_operations_state(world_two, validate_event_history=False)
    assert _TICK_VALID(world_two)
    world_two.minute_index = 4
    _record(world_two, _receipt(world_two, 3, "requested"))
    assert _mirror(world_two, SHARED).verify()


def test_a_passed_staff_receipt_stays_with_the_python_rule() -> None:
    """Its rule also reads the retained staff history, which may have changed since."""
    world = _world()
    state = world.model_state[SHARED]
    state["history"].append(
        {"event_type": "staff_operation_transition", "minute": 0, "event_id": "e-1"}
    )
    _TICK_PASSED(world)
    assert not _TICK_VALID(world)


@pytest.mark.parametrize(
    "spoil",
    [
        lambda state, world: state.__setitem__("schema_version", 2),
        lambda state, world: state.__setitem__("policy_version", "other"),
        lambda state, world: state["support_requests"]["request-1"].__setitem__(
            "status", "completed"
        ),
        lambda state, world: state["support_requests"]["request-1"].__setitem__(
            "agent_id", "not one!"
        ),
        lambda state, world: state["support_requests"]["request-1"].__setitem__(
            "support_kind", "x"
        ),
        lambda state, world: state["support_requests"]["request-1"].__setitem__(
            "created_minute", 99
        ),
        lambda state, world: state["support_requests"].__setitem__(
            "other", dict(state["support_requests"]["request-1"])
        ),
        lambda state, world: world.support_requests.pop("request-1"),
        lambda state, world: setattr(
            world.support_requests["request-1"], "status", SupportRequestStatus.ASSIGNED
        ),
        lambda state, world: setattr(world.support_requests["request-1"], "agent_id", "someone"),
        lambda state, world: state["history"].append({"minute": 99}),
        lambda state, world: state["history"][0].__setitem__("minute", 2),
    ],
)
def test_the_tick_validator_takes_nothing_python_turns_down(spoil: Any) -> None:
    world = _world()
    world.minute_index = 3
    record_shared_operation(world, _receipt(world, 1, "requested"))
    world.support_requests["request-1"] = SupportRequestState(
        id="request-1",
        agent_id=sorted(world.agents)[0],
        requested_minute=3,
        timeout_minute=99,
    )
    assert _TICK_VALID(world)
    spoil(world.model_state[SHARED], world)
    with pytest.raises(ValueError):
        validate_shared_operations_state(world, validate_event_history=False)
    try:
        assert _TICK_VALID(world) is False
    except native.Unsupported:
        pass


def test_the_tick_validator_reads_who_has_an_assigned_request() -> None:
    world = _world()
    world.minute_index = 3
    record_shared_operation(world, _receipt(world, 1, "requested"))
    record_shared_operation(world, _receipt(world, 1, "assigned", staff_id="S01"))
    request = SupportRequestState(
        id="request-1",
        agent_id=sorted(world.agents)[0],
        requested_minute=3,
        timeout_minute=99,
        status=SupportRequestStatus.ASSIGNED,
        assigned_staff_id="S01",
    )
    world.support_requests["request-1"] = request
    assert _TICK_VALID(world)
    validate_shared_operations_state(world, validate_event_history=False)
    request.assigned_staff_id = "S02"
    assert _TICK_VALID(world) is False
    with pytest.raises(ValueError, match="assigned staff disagrees"):
        validate_shared_operations_state(world, validate_event_history=False)
