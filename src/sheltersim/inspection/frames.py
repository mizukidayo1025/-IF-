"""Read-only presentation of one authoritative boundary state, shared by both APIs.

The frame of a minute is the core's (`world_frame`, #264); the selected person's evidence
(`person_projection`) and the interactions it reads (`social_event_records`) stay here.
"""

from __future__ import annotations

import json
from copy import deepcopy
from typing import Any

from sheltersim import native
from sheltersim.contracts.api import AgentPosition
from sheltersim.contracts.extensions import Point2D
from sheltersim.contracts.population import population_context_people
from sheltersim.core.state_mirror import read_only_part, read_only_state
from sheltersim.core.world import WorldState
from sheltersim.domain.care_receipts import care_history_for_agent
from sheltersim.domain.population_lifecycle import current_household_id
from sheltersim.domain.staff import support_wait_reason_codes, support_waited_minutes


def world_frame(
    world: WorldState,
    *,
    run_id: str,
    branch_id: str,
    computed_until: int,
    state_hash: str | None = None,
) -> dict[str, Any]:
    """The frame of the world at its minute: the API's `TimelineFrame`, as a new dict.

    The core makes it (`sheltersim_core::frame`, #264), byte for byte the frame this function made
    in Python before; `world_frame_bytes` gives its canonical JSON. No state initialization,
    policy evaluation, RNG draws or provider capability.
    """
    return json.loads(
        world_frame_bytes(
            world,
            run_id=run_id,
            branch_id=branch_id,
            computed_until=computed_until,
            state_hash=state_hash,
        )
    )


def world_frame_bytes(
    world: WorldState,
    *,
    run_id: str,
    branch_id: str,
    computed_until: int,
    state_hash: str | None = None,
) -> bytes:
    """The canonical JSON of `world_frame`'s frame.

    A caller that already holds the digest of the world (a checkpoint's `state_hash`) passes it;
    without it the frame's is `world.state_digest()`, which the core computes from the world's rows
    and its `rng_state` after the world is validated, as `state_digest` validates it.
    """
    if state_hash is None:
        world.validate()
    return bytes(
        _CORE_FRAME(
            world,
            run_id=run_id,
            branch_id=branch_id,
            computed_until=computed_until,
            state_hash=state_hash,
        )
    )


_CORE_FRAME = native.rules("frame").world_frame


def social_event_records(world: WorldState) -> tuple[list[dict[str, Any]], bool]:
    social_event_types = {
        "information_contact_update",
        "social_interaction",
        "information_correction",
    }
    social_events = []
    outcomes, window_truncated = world.event_log.recent_matching(frozenset(social_event_types))
    for outcome in outcomes:
        event = outcome.event
        if event.event_type not in social_event_types or outcome.status.value != "applied":
            continue
        payload = event.payload
        actors = sorted(
            {
                value
                for key in (
                    "agent_id",
                    "participant_agent_id",
                    "sender_agent_id",
                    "recipient_agent_id",
                )
                if isinstance((value := payload.get(key)), str)
            }
        )
        projected = {
            "event_id": event.event_id,
            "minute": outcome.realized_minute,
            "event_type": event.event_type,
            "actors": actors,
            "status": outcome.status.value,
            "causation_id": event.causation_id,
        }
        for field in (
            "interaction_type",
            "action",
            "topic_id",
            "message_id",
            "belief",
            "contact_basis",
            "contact_minutes",
            "reason_short",
            "zone_id",
        ):
            value = payload.get(field)
            if isinstance(value, (str, int)) and not isinstance(value, bool):
                projected[field] = value
        social_events.append(projected)
    return social_events, window_truncated


def person_projection(
    world: WorldState, identity: str, *, social_events: list[dict[str, Any]] | None = None
) -> dict[str, Any]:
    """Bounded selected-person evidence; no whole-world hash or model evaluation."""
    presence = read_only_part(world, "population_presence_v1", "people", {}).get(identity)
    agent = world.agents.get(identity)
    if agent is None:
        return {"person": None, "position": None, "presence": deepcopy(presence)}
    traits = read_only_part(
        world,
        "population_dynamics_v1",
        "profile_traits",
        world.model_configs.get("population", {}).get("people", {}),
    ).get(identity)
    context = population_context_people(world).get(identity)
    active_care_requests = []
    care_state = read_only_part(world, "shared_operations_v1", "support_requests", {})
    if isinstance(care_state, dict):
        for request_id, raw_request in sorted(care_state.items()):
            if not isinstance(raw_request, dict) or raw_request.get("agent_id") != identity:
                continue
            request = deepcopy(raw_request)
            runtime = world.support_requests.get(request_id)
            request["runtime_status"] = runtime.status.value if runtime is not None else None
            request["assigned_staff_id"] = (
                runtime.assigned_staff_id if runtime is not None else None
            )
            if runtime is not None:
                request["waited_minutes"] = support_waited_minutes(
                    world, request_id, minute=world.minute_index
                )
                request["wait_reason_codes"] = list(
                    support_wait_reason_codes(world, request_id, minute=world.minute_index)
                )
            active_care_requests.append(request)
    person = {
        "id": identity,
        "group_id": agent.profile.group_id,
        "household_id": presence.get("household_id")
        if presence
        else (context or {}).get("household_id", agent.profile.group_id),
        "zone_id": agent.zone_id,
        "activity": agent.activity.value,
        "target_id": agent.target_id,
        "stress": agent.stress,
        "trust": agent.trust,
        "needs": vars(agent.needs).copy(),
        "mobility": agent.profile.mobility.value,
        "support_need": agent.profile.support_need,
        "traits": deepcopy(traits),
        "context": deepcopy(context),
        "decision": deepcopy(
            read_only_part(world, "agent_dynamics_v1", "agents", {}).get(identity)
        ),
        "beliefs": deepcopy(
            dict(
                list(
                    read_only_part(world, "information_spread_v1", "beliefs", {})
                    .get(identity, {})
                    .items()
                )[:16]
            )
        ),
        "body": deepcopy(read_only_part(world, "body_state_v1", "agents", {}).get(identity)),
        "care_requests": active_care_requests,
        "care_history": care_history_for_agent(world, identity),
        "dialogue": None,
        "dialogue_status": "not_recorded",
    }
    overlay = read_only_state(world, "geometry_overlay_v1", {})
    reservation = (
        overlay.get("bed_reservations", {}).get(identity) if isinstance(overlay, dict) else None
    )
    person["sleeping_place"] = deepcopy(reservation)
    admission = (
        overlay.get("room_admissions", {}).get(identity) if isinstance(overlay, dict) else None
    )
    person["physical_room"] = deepcopy(admission)
    if social_events is None:
        social_events, _ = social_event_records(world)
    dialogue = [
        item
        for item in social_events
        if identity in item["actors"]
        or item["event_type"] == "information_correction"
        and item.get("zone_id") == agent.zone_id
    ][-20:]
    person["dialogue"] = deepcopy(dialogue)
    person["dialogue_status"] = "structured_events_only" if dialogue else "no_retained_events"
    person["family_separated"] = (
        any(
            current_household_id(world, other.id) == current_household_id(world, agent.id)
            and other.zone_id != agent.zone_id
            for other in world.agents.values()
        )
        if current_household_id(world, agent.id) is not None
        else False
    )
    record = read_only_part(world, "movement_v1", "positions", {}).get(identity, {})
    point = Point2D.model_validate(record["point"]) if record.get("point") is not None else None
    kind = record.get("position_kind", "unavailable")
    zone = world.zones[agent.zone_id]
    if (
        point is None
        and not world.model_configs.get("facility_geometry")
        and zone.display_x is not None
        and zone.display_y is not None
    ):
        point, kind = Point2D(x=zone.display_x, y=zone.display_y), "display_only"
    position = AgentPosition(
        agent_id=identity,
        zone_id=record.get("zone_id", agent.zone_id),
        point=point,
        edge_id=record.get("edge_id"),
        progress=record.get("progress", 0),
        position_kind=kind,
    ).model_dump(mode="json")
    return {"person": person, "position": position, "presence": deepcopy(presence)}
