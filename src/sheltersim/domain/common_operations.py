"""Checkpointed shared care/staff/facility transition records and safe projections."""

from __future__ import annotations

from typing import Any

from pydantic import TypeAdapter

from sheltersim import native
from sheltersim.contracts.operations import (
    MAX_SHARED_OPERATION_HISTORY,
    SHARED_OPERATIONS_KEY,
    SHARED_OPERATIONS_POLICY,
    CareRequestState,
    CareTransitionReceipt,
    OperationalDemandSnapshot,
    OperationHistoryRecord,
    SharedOperationsState,
    StaffOperationReceipt,
    StaffSupportContinuityReceipt,
)
from sheltersim.core.history import records_since_previous_tick
from sheltersim.core.kernels import kernel_only
from sheltersim.core.state_mirror import (
    detached_row,
    holds_detached,
    read_only_part,
    rust_accepts,
    validated_state,
)
from sheltersim.core.tick_validation import saved_json
from sheltersim.core.world import WorldState
from sheltersim.domain.geometry_overlay import parsed_facility_geometry
from sheltersim.events.models import EventStatus

# native/src/state/shared_operations.rs: what accepts a tick's state with no replay of the history,
# and what tells the row that these rules accepted its recent receipts.
_TICK_VALID = native.state("shared_operations").validate_tick
_TICK_PASSED = native.state("shared_operations").recent_passed
_HISTORY_PASSED = native.state("shared_operations").history_passed

_CARE_TRANSITIONS = {
    None: {"requested"},
    "requested": {"assigned", "started", "declined", "interrupted"},
    "assigned": {"started", "declined", "interrupted"},
    "started": {"completed", "interrupted"},
}


def _care_interruption_due(
    world: WorldState, request_id: str, head_status: str, request_status: str
) -> bool:
    """A request timed out in this minute's batch of due events, before its care record.

    SupportRequestTimeoutHandler closes the request inside the batch, and the dispatch phase
    records the care interruption after the batch (`_record_request_interrupted`, and
    native/src/rules/dispatch.rs in the same order). An event later in the batch whose handler
    validates the world sees the request closed and its care head still requested (#88).
    Only this minute's applied timeout of the request allows it: once the minute has moved on,
    validate_after_tick finds a missing interruption.
    """
    if head_status != "requested" or request_status != "unresolved":
        return False
    return any(
        record.status is EventStatus.APPLIED
        and record.event.event_type == "support_request_timeout"
        and record.event.payload.get("request_id") == request_id
        for record in world.event_log.at_minute(world.minute_index)
    )


def create_shared_operations_state() -> dict[str, Any]:
    return {
        "schema_version": 1,
        "policy_version": SHARED_OPERATIONS_POLICY,
        "support_requests": {},
        "history": [],
        "history_truncated": False,
    }


def validate_shared_operations_state(
    world: WorldState,
    *,
    validate_event_history: bool = True,
    history_memo: Any = None,
) -> None:
    detached = holds_detached(world, SHARED_OPERATIONS_KEY)
    state = None if detached else world.model_state.get(SHARED_OPERATIONS_KEY)
    if state is None and not detached:
        return
    if (
        not validate_event_history
        and history_memo is None
        and _TICK_VALID is not None
        and rust_accepts(world, SHARED_OPERATIONS_KEY, _TICK_VALID)
    ):
        return
    if detached:
        # The row alone holds it: the save's document, or one made from the row.
        state = validated_state(world, SHARED_OPERATIONS_KEY, history_memo)
    # Per-tick validation parses only the records from the tick that just ended (each
    # was checked against its event when appended); the run-boundary validation
    # replays the whole history and the care lifecycle.
    checked = state
    history = state.get("history") if isinstance(state, dict) else None
    if not validate_event_history and isinstance(history, list):
        checked = {**state, "history": records_since_previous_tick(history, world.minute_index)}
    # A save's validation continues the replay after the records unchanged since the
    # last one; a truncated history has no fixed start to replay from.
    seen_events: set[str] = set()
    care: dict[str, CareRequestState] = {}
    last_minute = 0
    replay_memo = history_memo if isinstance(history, list) and not _truncated(state) else None
    if replay_memo is not None:
        kept = replay_memo.resume((SHARED_OPERATIONS_KEY, "history"), _staff_operations_json(world))
        if kept is not None:
            start, (seen_events, care, last_minute) = kept
            checked = {**state, "history": history[start:]}
    try:
        parsed = SharedOperationsState.model_validate(checked)
    except (TypeError, ValueError) as exc:
        raise ValueError("shared_operations_v1 has an invalid exact schema") from exc
    if (
        len(state["history"]) > MAX_SHARED_OPERATION_HISTORY
        or any(a.minute > b.minute for a, b in zip(parsed.history, parsed.history[1:]))
        or (parsed.history and last_minute > parsed.history[0].minute)
    ):
        raise ValueError("shared_operations_v1 history is over limit or out of time order")
    for item in parsed.history:
        if item.minute > world.minute_index or item.event_id in seen_events:
            raise ValueError(
                "shared_operations_v1 history has future, duplicate, or unordered records"
            )
        seen_events.add(item.event_id)
        event_record = world.event_log.find(item.event_id)
        if (
            event_record is None
            or event_record.status is not EventStatus.APPLIED
            or event_record.realized_minute != item.minute
            or event_record.event.event_type != item.event_type
            or not _event_payload_matches(event_record, item)
        ):
            raise ValueError("shared_operations_v1 record must link to a realized applied event")
        if isinstance(item, StaffOperationReceipt):
            _validate_staff_operation_receipt(
                world, item, history_truncated=parsed.history_truncated
            )
        if not validate_event_history:
            continue
        if isinstance(item, CareTransitionReceipt):
            prior = care.get(item.request_id)
            if prior is None:
                if item.status != "requested" and not parsed.history_truncated:
                    raise ValueError("a support request must begin with a requested transition")
                saved = parsed.support_requests.get(item.request_id)
                created = (
                    saved.created_minute if parsed.history_truncated and saved else item.minute
                )
                if created > item.minute:
                    raise ValueError("truncated care history has an invalid creation minute")
            else:
                if (prior.agent_id, prior.household_id, prior.support_kind) != (
                    item.agent_id,
                    item.household_id,
                    item.support_kind,
                ):
                    raise ValueError("support request identity changed during its lifecycle")
                if item.status not in _CARE_TRANSITIONS.get(prior.status, set()):
                    raise ValueError("support lifecycle transition is out of order")
                created = prior.created_minute
            care[item.request_id] = CareRequestState(
                request_id=item.request_id,
                agent_id=item.agent_id,
                household_id=item.household_id,
                support_kind=item.support_kind,
                status=item.status,
                created_minute=created,
                updated_minute=item.minute,
                latest_event_id=item.event_id,
            )
        elif isinstance(item, StaffSupportContinuityReceipt):
            prior = care.get(item.request_id)
            saved = parsed.support_requests.get(item.request_id)
            if prior is None:
                if not parsed.history_truncated or (item.status == "requeued" and saved is None):
                    raise ValueError("support continuity must follow an active care transition")
                if saved is not None and (
                    saved.agent_id != item.agent_id
                    or saved.latest_event_id != item.event_id
                    or saved.status != "requested"
                ):
                    raise ValueError(
                        "truncated support continuity does not match the active request"
                    )
                created = saved.created_minute if saved is not None else item.minute
            else:
                if prior.status not in {"assigned", "started"} or prior.agent_id != item.agent_id:
                    raise ValueError("support continuity does not follow assigned resident care")
                created = prior.created_minute
            if item.status == "requeued":
                care[item.request_id] = CareRequestState(
                    request_id=item.request_id,
                    agent_id=item.agent_id,
                    household_id=prior.household_id if prior is not None else saved.household_id,
                    support_kind=prior.support_kind if prior is not None else saved.support_kind,
                    status="requested",
                    created_minute=created,
                    updated_minute=item.minute,
                    latest_event_id=item.event_id,
                )
            else:
                care.pop(item.request_id, None)
    saved_care = dict(parsed.support_requests)
    active_care = {
        key: value
        for key, value in care.items()
        if value.status in {"requested", "assigned", "started"}
    }
    for request_id, replayed in active_care.items():
        if saved_care.get(request_id) != replayed:
            raise ValueError(
                "shared_operations_v1 care request head does not match its retained history"
            )
    for request_id, head in saved_care.items():
        if head.status not in {"requested", "assigned", "started"}:
            raise ValueError("shared_operations_v1 stores a terminal request as active")
        request = world.support_requests.get(request_id)
        if request is None or request.agent_id != head.agent_id:
            raise ValueError(
                "shared_operations_v1 care head has no matching persisted support request"
            )
        expected_status = "pending" if head.status == "requested" else "assigned"
        if request.status.value != expected_status and not _care_interruption_due(
            world, request_id, head.status, request.status.value
        ):
            raise ValueError(
                "shared_operations_v1 care status disagrees with the persisted support request"
            )
        if request_id in active_care or not validate_event_history:
            if head.status in {"assigned", "started"}:
                evidence = world.event_log.find(head.latest_event_id)
                staff_id = evidence.event.payload.get("staff_id") if evidence is not None else None
                if request.assigned_staff_id != staff_id:
                    raise ValueError(
                        "shared_operations_v1 assigned staff disagrees with the support task"
                    )
            continue
        if not parsed.history_truncated:
            raise ValueError("shared_operations_v1 care request head has no matching history")
        record = world.event_log.find(head.latest_event_id)
        if (
            record is None
            or record.status is not EventStatus.APPLIED
            or record.realized_minute != head.updated_minute
            or record.event.event_type
            not in {"care_support_transition", "staff_support_continuity"}
        ):
            raise ValueError("truncated care request head has no applied event evidence")
        try:
            latest = TypeAdapter(OperationHistoryRecord).validate_python(
                record.event.to_dict()["payload"]
            )
        except (TypeError, ValueError) as exc:
            raise ValueError("truncated care request head event has an invalid receipt") from exc
        receipt_matches = (
            isinstance(latest, CareTransitionReceipt)
            and (
                latest.request_id,
                latest.agent_id,
                latest.household_id,
                latest.support_kind,
                latest.status,
                latest.event_id,
            )
            == (
                request_id,
                head.agent_id,
                head.household_id,
                head.support_kind,
                head.status,
                head.latest_event_id,
            )
        ) or (
            isinstance(latest, StaffSupportContinuityReceipt)
            and latest.status == "requeued"
            and (latest.request_id, latest.agent_id, latest.event_id)
            == (request_id, head.agent_id, head.latest_event_id)
            and head.status == "requested"
        )
        if not receipt_matches or not _event_payload_matches(record, latest):
            raise ValueError("truncated care request head disagrees with its applied event")
    if replay_memo is not None:
        last_minute = parsed.history[-1].minute if parsed.history else last_minute
        replay_memo.keep(
            (SHARED_OPERATIONS_KEY, "history"), len(history), (seen_events, care, last_minute)
        )
    _tell_the_row_they_passed(world, validate_event_history, history_memo)


def _tell_the_row_they_passed(
    world: WorldState, validate_event_history: bool, history_memo: Any
) -> None:
    """A tick's kernels trust the receipts these rules have just passed: the recent ones of a tick,
    or all of them after a replay of the whole history."""
    try:
        if validate_event_history:
            _HISTORY_PASSED(world)
        elif history_memo is None:
            _TICK_PASSED(world)
    except native.Unsupported:
        pass


def _truncated(state: Any) -> bool:
    return not isinstance(state, dict) or state.get("history_truncated") is not False


def _staff_operations_json(world: WorldState) -> bytes | None:
    """What the rule for an old StaffOperationReceipt reads besides the receipt."""
    mirror = detached_row(world, "staff_operations_v1")
    if mirror is not None:
        # The same bytes, from the row's items, and no document made of them.
        raws = mirror.items_json("history")
        return b"[" + b",".join(raws or ()) + b"]"
    operations = world.model_state.get("staff_operations_v1", {})
    return saved_json(operations.get("history", ()) if isinstance(operations, dict) else ())


@kernel_only("common_operations", "record_shared_operation")
def record_shared_operation(world: WorldState, record: OperationHistoryRecord) -> None:
    """Append a receipt only after its matching state-changing event is applied.

    The receipt's `model_dump(mode="json")` goes to the history, which keeps the newest
    MAX_SHARED_OPERATION_HISTORY (then history_truncated is True). A care transition updates its
    request's head in support_requests (dropped when completed, interrupted or declined), and a
    continuity receipt requeues the request or drops it.

    ValueError, before any change, in this order: no shared_operations_v1 dict; the state with the
    records since the previous tick not passing SharedOperationsState; a receipt minute that is not
    the world's; no applied event of the receipt's ID and type realized at that minute; an event
    already recorded since the previous tick; an event payload, causation or source that is not
    the receipt's (`_event_payload_matches`); then the receipt's own checks (a care transition out
    of `_CARE_TRANSITIONS`' order, a changed request identity, a request ID used before, too many
    requests; a continuity receipt that does not match the interrupted request, its status or its
    previous assignment; a handoff that does not match the active task; a facility recovery that
    does not match the facility; a staff operation receipt without its one retained completion);
    and last a state change (care and staff operation receipts) whose after-value is not the
    world's.
    """


def _event_payload_matches(event_record: Any, record: OperationHistoryRecord) -> bool:
    """Require the complete immutable event payload and causal/source headers to agree."""
    event = event_record.event
    payload = event.payload_dict()
    expected = record.model_dump(mode="json")
    if (
        isinstance(record, StaffSupportContinuityReceipt)
        and "cause_type" not in payload
        and record.cause_type == "staff_unavailable"
    ):
        # Existing v1 receipts predate the optional cause discriminator.
        expected.pop("cause_type")
    return (
        payload == expected
        and event.causation_id == record.causation_id
        and (event.source_id is None or event.source_id in record.source_ids)
    )


def _validate_staff_operation_receipt(
    world: WorldState,
    record: StaffOperationReceipt,
    *,
    history_truncated: bool,
) -> None:
    operations = world.model_state.get("staff_operations_v1", {})
    history = operations.get("history", ()) if isinstance(operations, dict) else ()
    matches = [
        row
        for row in history
        if isinstance(row, dict)
        and row.get("task_id") == record.task_id
        and row.get("complete_minute") == record.minute
    ]
    if not matches:
        if (
            history_truncated
            and history
            and all(
                row.get("complete_minute", 0) > record.minute
                for row in history
                if isinstance(row, dict)
            )
        ):
            return
        raise ValueError("staff operation receipt has no matching retained completion")
    if len(matches) != 1:
        raise ValueError("staff operation receipt matches duplicate completion history")
    row = matches[0]
    if (
        row.get("staff_id") != record.staff_id
        or row.get("task_kind") != record.task_kind
        or row.get("target_id") != record.target_id
        or row.get("target_zone_id") != record.target_zone_id
        or row.get("reason") != record.reason
        or row.get("status") != record.status
        or row.get("start_minute") != record.start_minute
        or row.get("arrival_minute") != record.arrival_minute
        or row.get("travel_minutes") != record.travel_minutes
        or record.causation_id not in row.get("causation_event_ids", ())
    ):
        raise ValueError("staff operation receipt disagrees with retained completion history")


def operational_demand_snapshot(world: WorldState) -> dict[str, Any]:
    """Return observed occupancy/queue/staff/resource state and explicit non-estimates."""
    from sheltersim.contracts.spatial import occupancy_snapshot

    geometry_raw = world.model_configs.get("facility_geometry")
    geometry = parsed_facility_geometry(geometry_raw) if geometry_raw else None
    explicit_points = geometry.service_points if geometry is not None else ()
    independent_queue_state = world.model_state.get("facility_queues_v1", {})
    services = []
    for facility in sorted(world.facilities.values(), key=lambda row: row.id):
        points = [point for point in explicit_points if point.facility_id == facility.id]
        if not points and geometry is not None and facility.id in geometry.facility_points:
            from sheltersim.contracts.spatial import legacy_service_points

            points = list(legacy_service_points(geometry, [facility]))
        for point in points:
            queue_row = (
                independent_queue_state.get("queues", {}).get(point.queue_id)
                if isinstance(independent_queue_state, dict)
                else None
            )
            bound = point.queue_id == facility.id and queue_row is None
            services.append(
                {
                    "service_point_id": point.id,
                    "facility_id": facility.id,
                    "zone_id": facility.zone_id,
                    "point_m": point.point_m.model_dump(mode="json"),
                    "queue_id": point.queue_id,
                    "queue_length": (
                        len(queue_row["queued_request_ids"])
                        if isinstance(queue_row, dict)
                        else (
                            len(facility.queue_agent_ids) + len(facility.resource_queue_agent_ids)
                            if bound
                            else None
                        )
                    ),
                    "active_services": (
                        len(queue_row["active_request_ids"])
                        if isinstance(queue_row, dict)
                        else (
                            len(facility.active_agent_ids) + len(facility.resource_active_agent_ids)
                            if bound
                            else None
                        )
                    ),
                    "queued_request_ids": list(queue_row["queued_request_ids"])
                    if isinstance(queue_row, dict)
                    else None,
                    "active_request_ids": list(queue_row["active_request_ids"])
                    if isinstance(queue_row, dict)
                    else None,
                    "last_stall": queue_row["last_stall"] if isinstance(queue_row, dict) else None,
                    "service_capacity_concurrent": point.service_capacity,
                    "reservation_binding": "independent_queue_state"
                    if isinstance(queue_row, dict)
                    else ("legacy_facility_fifo" if bound else "not_connected"),
                    "accessibility": point.access.accessibility,
                    "required_access_tags": list(point.access.required_access_tags),
                    "required_staff_skills": list(point.access.required_staff_skills),
                }
            )
    requests = read_only_part(world, SHARED_OPERATIONS_KEY, "support_requests", {})
    request_counts: dict[str, int] = {}
    for request in requests.values():
        status = request.get("status", "unknown")
        request_counts[status] = request_counts.get(status, 0) + 1
    snapshot = OperationalDemandSnapshot(
        minute=world.minute_index,
        population_count=len(world.agents),
        pending_arrivals=len(
            read_only_part(world, "population_dynamics_v1", "pending_arrivals", [])
        ),
        zones=tuple(occupancy_snapshot(world)["zones"]),
        services=tuple(services),
        staff={
            "count": len(world.staff),
            "assigned": sum(row.active_task_id is not None for row in world.staff.values()),
        },
        resources={key: value for key, value in world.resources.items() if type(value) is int},
        support_requests=request_counts,
    )
    return snapshot.model_dump(mode="json")
