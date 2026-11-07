"""Dynamic arrivals, capacity-aware admission, and departure history.

The rules are native/src/rules/population.rs (`kernel_only`): each docstring states its contract.
"""

from __future__ import annotations

from typing import Any

from sheltersim import native
from sheltersim.core.kernels import kernel_only
from sheltersim.core.state_mirror import register_rust_validator
from sheltersim.core.world import WorldState
from sheltersim.events.models import ScheduledEvent

POPULATION_DYNAMICS_STATE_KEY = "population_dynamics_v1"
POPULATION_DYNAMICS_POLICY_VERSION = "capacity-aware-arrivals-departures-v1"
POPULATION_SOURCE_ID = "ASM-POPULATION-DYNAMICS-V1"
MAX_ARRIVALS_PER_EVENT = 1000
MAX_PENDING_ARRIVALS = 10_000


def create_population_dynamics_state(world: WorldState) -> dict[str, Any]:
    """Capture initial synthetic traits and prepare immutable population history."""
    traits: dict[str, dict[str, Any]] = {}
    raw = world.model_configs.get("population")
    if raw is not None:
        from sheltersim.contracts.extensions import PopulationSpec

        population = PopulationSpec.model_validate(raw)
        traits = {
            agent_id: item.model_dump(mode="json")
            for agent_id, item in sorted(population.people.items())
        }
    return {
        "schema_version": 1,
        "policy_version": POPULATION_DYNAMICS_POLICY_VERSION,
        "source_id": POPULATION_SOURCE_ID,
        "initial_agent_ids": sorted(world.agents),
        "profile_traits": traits,
        "profile_context": {},
        "pending_arrivals": [],
        "arrivals": [],
        "departures": [],
    }


# The state with no one pending: native/src/rules/tick_validation.rs.
register_rust_validator(
    POPULATION_DYNAMICS_STATE_KEY,
    native.rules("tick_validation").population_dynamics,
)


@kernel_only("population", "validate_population_dynamics_state")
def validate_population_dynamics_state(world: WorldState) -> None:
    """Validate dynamic profile snapshots, admission order, and immutable history.

    Nothing without population_dynamics_v1. ValueError, in this order:
    "population_dynamics_v1 has an invalid exact schema" (the eight fields, with or without
    profile_context; schema 1, this policy and source; unique initial_agent_ids; at most
    MAX_PENDING_ARRIVALS waiting), "... is missing an active profile" for an Agent with neither
    traits nor an initial ID, "... has an invalid dynamic context index" for a context of a person
    never known; for each context (a PersonContext), "dynamic household context references an
    unknown person" and "dynamic household context disagrees with generated profile provenance"
    (this source, a household that is the person's and every related person's traits' household);
    for each pending arrival, "population_dynamics_v1 pending arrival is invalid" (its field set,
    a known zone, an ID that is neither waiting twice nor admitted, valid alternate zones), "...
    pending context is invalid", "pending care context disagrees with its persisted profile
    context", and for an arrival on an approach route "pending arrival approach route/timing is
    invalid", "... disagrees with its route contract", "... completion minute is invalid", "...
    completion event is missing", "... completion event disagrees", "out-of-horizon arrival approach
    has a completion event", "completed arrival approach retains a pending event", "arrival
    approach disagrees with population presence"; its traits are a PersonTraits.
    """


@kernel_only("population", "queue_arrivals")
def queue_arrivals(
    world: WorldState,
    *,
    event: ScheduledEvent,
    zone_id: str,
    count: int,
    household_size: int = 1,
    mobility: str = "independent",
    language: str = "ja",
    admission_zone_ids: tuple[str, ...] = (),
    approach_route_id: str | None = None,
    person_context_overrides: tuple[dict[str, Any], ...] = (),
) -> int:
    """Create deterministic synthetic profiles, admitting only within zone capacity.

    Returns how many were admitted now. EventApplicationError, in this order, for an unknown zone;
    invalid alternate zones (a list or tuple of distinct known zones other than zone_id); an
    approach route with alternate zones, an unknown route or one to another zone; alternate zones
    without geometry_overlay schema 3 or 4; count outside 1..MAX_ARRIVALS_PER_EVENT; household_size
    outside 1..12; an invalid mobility or language; invalid person contexts or their member
    indices; past MAX_PENDING_ARRIVALS waiting, 10000 residents or 10000 known profiles; a generated
    ID that is taken.

    Each person's ID ("arrival-" + 28 hex), household, age, needs and profile come from
    SHA-256 of seed, event ID and index; households link caregivers and dependents. A person on an
    approach route waits for its scheduled population_approach_complete event; the others are
    admitted to the first of zone_id and the alternate zones with room, or to a temporary room
    there, or wait. The arrival summary, the presence ledger and the arrival and waiting counts are
    recorded.
    """


@kernel_only("population", "apply_arrival_approach_completion")
def apply_arrival_approach_completion(world: WorldState, event: ScheduledEvent) -> None:
    """Commit the APPLIED arrival at the entrance and retain its event-linked history.

    EventApplicationError, in this order: "arrival approach completion payload fields do not
    match", "... is outside its approved program", "... does not match pending arrival", "arrival
    approach route provenance does not match", "arrival approach has no matching presence state".
    The waiting arrival then holds approach_completed_minute = the due minute (its completion event
    ID is dropped), and the presence ledger moves it from approaching to awaiting_admission.
    """


@kernel_only("population", "admit_pending_arrivals")
def admit_pending_arrivals(world: WorldState, minute: int) -> tuple[str, ...]:
    """Admit waiting synthetic residents in event/ID order when space opens.

    In (created_minute, event_id, ID) order, each arrival that is not still on its approach route is
    admitted where choose_admission_target finds a place. Returns the admitted IDs; updates the
    arrival and waiting counts and each arrival summary's admitted and waiting counts.
    """


@kernel_only("population", "queue_departures")
def queue_departures(
    world: WorldState,
    *,
    event: ScheduledEvent,
    zone_id: str,
    count: int,
    include_initial: bool = False,
) -> int:
    """Remove a deterministic local set of residents, preserving stable departure records.

    EventApplicationError, in this order, for an unknown zone, count outside
    1..MAX_ARRIVALS_PER_EVENT, a non-bool include_initial, and "only N admitted dynamic residents
    are available to leave". The first count residents of the zone (dynamic ones by arrival minute,
    and initial ones first when include_initial) leave; when one of them is busy or has a support
    request, schedule_population_retry runs and EventSuppressed("departure waits for the selected
    residents' active tasks") is raised. A departure releases beds and rooms, records the profile,
    traits, needs and relationships, withdraws the person's claims and cancels their pending
    events (each recorded SUPPRESSED), and removes them from every model state. Then
    admit_pending_arrivals runs. Returns the count.
    """


@kernel_only("population", "schedule_population_retry")
def schedule_population_retry(
    world: WorldState,
    event: ScheduledEvent,
    *,
    id_prefix: str = "departure-retry",
    subject: str = "departure",
) -> None:
    """Retry an approved population event next minute, at most 1440 times.

    The retry joins the approved program's scheduled events, so it passes the same
    approval check. The caller then raises EventSuppressed for this minute.
    EventSuppressed("<subject> could not complete before the scenario ended") when the next minute
    is past the run, and ("<subject> reached the bounded deferral limit") after 1440 retries.
    """


@kernel_only("population", "zone_has_room")
def _zone_has_room(world: WorldState, zone_id: str) -> bool:
    """Whether the zone's effective capacity, if any, exceeds its residents and those walking in."""


@kernel_only("population", "choose_admission_target")
def _choose_admission_target(
    world: WorldState, item: dict[str, Any]
) -> tuple[str, str | None] | None:
    """Choose the preferred zone first, then explicitly allowed alternate zones.

    (zone, None) for the first zone with room, (zone, room) for one with an available temporary
    room, or None.
    """
