"""Versioned, operator-approved temporary capacity for dynamic population overflow.

This contract affects admission and capacity validation only. It carries no floorplan
coordinates, so consumers must not draw a fabricated room or walking path for a space.
"""

from __future__ import annotations

from typing import Any

from sheltersim import native
from sheltersim.core.kernels import kernel_only
from sheltersim.core.state_mirror import register_rust_validator
from sheltersim.core.world import WorldState

TEMPORARY_SPACES_STATE_KEY = "temporary_spaces_v1"
TEMPORARY_SPACES_POLICY_VERSION = "typed-temporary-capacity-v1"
TEMPORARY_SPACES_SOURCE_ID = "ASM-TEMPORARY-CAPACITY-V1"
MAX_TEMPORARY_SPACES = 256
MAX_TEMPORARY_SPACE_HISTORY = 20_000

_STATE_FIELDS = {"schema_version", "policy_version", "spaces", "history"}
_SPACE_FIELDS = {
    "space_id",
    "zone_id",
    "people_capacity",
    "sleeping_capacity",
    "start_minute",
    "end_minute",
    "active",
    "start_event_id",
    "end_event_id",
    "program_id",
    "effect_id",
    "source_ids",
    "geometry_status",
}


def create_temporary_spaces_state() -> dict[str, Any]:
    return {
        "schema_version": 1,
        "policy_version": TEMPORARY_SPACES_POLICY_VERSION,
        "spaces": {},
        "history": [],
    }


def validate_temporary_spaces_state(world: WorldState) -> None:
    state = world.model_state.get(TEMPORARY_SPACES_STATE_KEY)
    if state is None:
        return
    if (
        not isinstance(state, dict)
        or set(state) != _STATE_FIELDS
        or state.get("schema_version") != 1
        or state.get("policy_version") != TEMPORARY_SPACES_POLICY_VERSION
        or not isinstance(state.get("spaces"), dict)
        or len(state["spaces"]) > MAX_TEMPORARY_SPACES
        or not isinstance(state.get("history"), list)
        or len(state["history"]) > MAX_TEMPORARY_SPACE_HISTORY
    ):
        raise ValueError("temporary_spaces_v1 has an invalid exact schema")
    for space_id, row in state["spaces"].items():
        if (
            not isinstance(row, dict)
            or set(row) != _SPACE_FIELDS
            or row.get("space_id") != space_id
            or row.get("zone_id") not in world.zones
            or type(row.get("people_capacity")) is not int
            or not 1 <= row["people_capacity"] <= 1000
            or type(row.get("sleeping_capacity")) is not int
            or not 0 <= row["sleeping_capacity"] <= row["people_capacity"]
            or type(row.get("start_minute")) is not int
            or type(row.get("end_minute")) is not int
            or row["start_minute"] < 0
            or row["end_minute"] <= row["start_minute"]
            or type(row.get("active")) is not bool
            or not isinstance(row.get("start_event_id"), str)
            or not row["start_event_id"]
            or (row["active"] and row.get("end_event_id") is not None)
            or (
                not row["active"]
                and (not isinstance(row.get("end_event_id"), str) or not row["end_event_id"])
            )
            or not isinstance(row.get("program_id"), str)
            or not isinstance(row.get("effect_id"), str)
            or not isinstance(row.get("source_ids"), list)
            or any(not isinstance(item, str) or not item for item in row["source_ids"])
            or row.get("geometry_status") != "zone_only_capacity"
        ):
            raise ValueError(f"temporary space {space_id!r} is invalid")
    for row in state["history"]:
        if (
            not isinstance(row, dict)
            or set(row)
            != {"event_id", "space_id", "zone_id", "minute", "transition", "causation_id"}
            or not isinstance(row["event_id"], str)
            or not isinstance(row["space_id"], str)
            or row["zone_id"] not in world.zones
            or type(row["minute"]) is not int
            or row["transition"] not in {"activated", "expired"}
            or (row["causation_id"] is not None and not isinstance(row["causation_id"], str))
        ):
            raise ValueError("temporary_spaces_v1 history record is invalid")


@kernel_only("overlay", "capacity_metrics")
def capacity_metrics(
    world: WorldState, zone_id: str, *, occupancy: int | None = None
) -> dict[str, int | None]:
    """Zone-only capacity and separately measured physical room intake.

    `nominal_capacity` is the zone's own (None: unbounded, and so is every total). Active
    temporary spaces add their people; an expired one keeps holding the people still above the
    rest (`retained_expired_capacity`). Signed physical rooms (geometry overlay schema 3 or 4)
    add their people, and an expired room keeps the people it admitted.
    """


@kernel_only("overlay", "effective_zone_capacity")
def effective_zone_capacity(
    world: WorldState, zone_id: str, *, occupancy: int | None = None
) -> int | None:
    """capacity_metrics(...)["effective_capacity"]."""


@kernel_only("overlay", "effective_total_zone_capacity")
def effective_total_zone_capacity(
    world: WorldState, zone_id: str, *, occupancy: int | None = None
) -> int | None:
    """Capacity bound for WorldState validation, including signed physical rooms."""


@kernel_only("overlay", "incoming_travelers")
def incoming_travelers(world: WorldState, zone_id: str, *, excluding: str | None = None) -> int:
    """People walking to zone_id, directly or to one of its facilities.

    Travel admission holds their place from departure, so every other admission into
    the zone must count them too, or their arrival overfills it (#180).
    """


@kernel_only("overlay", "register_temporary_space")
def register_temporary_space(world: WorldState, record: dict[str, Any], *, event_id: str) -> None:
    """Add an active space and its "activated" history row.

    ValueError when the namespace is not installed, the ID exists, or MAX_TEMPORARY_SPACES are
    held. The history keeps its last MAX_TEMPORARY_SPACE_HISTORY rows.
    """


@kernel_only("overlay", "expire_temporary_space")
def expire_temporary_space(
    world: WorldState, space_id: str, *, event_id: str, causation_id: str | None
) -> bool:
    """End an active space, with its "expired" history row; False for one unknown or ended."""


register_rust_validator(TEMPORARY_SPACES_STATE_KEY, native.rules("overlay").validate_spaces)
