"""Same-identity population lifecycle commands and retained absent-person state.

The common presence ledger records where a person is in the population lifecycle;
this module owns the simulation state needed to resume that same identity. Commands
are typed and deterministic. Natural-language compilers must map into these commands
only after validating an approved EffectProgram.

The rules are native/src/rules/population_lifecycle.rs (`kernel_only`): each docstring states its
contract. The command's contract, PopulationPresenceCommand, stays here; the kernel parses with it.
"""

from __future__ import annotations

from typing import Any, Literal

from pydantic import Field, model_validator

from sheltersim import native
from sheltersim.contracts.extensions import ExtensionModel, Identifier
from sheltersim.core.kernels import kernel_only
from sheltersim.core.state_mirror import register_rust_validator
from sheltersim.core.world import WorldState
from sheltersim.events.models import ScheduledEvent

POPULATION_LIFECYCLE_STATE_KEY = "population_lifecycle_v1"
POPULATION_LIFECYCLE_POLICY_VERSION = "same-id-population-lifecycle-v1"
POPULATION_LIFECYCLE_SOURCE_ID = "ASM-POPULATION-LIFECYCLE-V1"
MAX_LIFECYCLE_HISTORY = 20_000
MAX_RETAINED_PEOPLE = 10_000
MAX_RELATIONSHIP_ARCHIVE = 200_000


class PopulationPresenceCommand(ExtensionModel):
    """Validated controller command; does not accept narrative or executable text."""

    action: Literal["temporary_exit", "return", "transfer", "relocate", "reunite"]
    agent_id: Identifier | None = None
    agent_ids: tuple[Identifier, ...] = Field(default=(), max_length=1000)
    zone_id: Identifier | None = None
    household_id: Identifier | None = None
    expected_return_minute: int | None = Field(default=None, strict=True, ge=0)
    source_ids: tuple[Identifier, ...] = Field(default=(), max_length=16)

    @model_validator(mode="after")
    def inputs_match_action(self) -> PopulationPresenceCommand:
        if self.action == "relocate" and self.zone_id is None:
            raise ValueError("relocate requires a destination zone")
        if self.action == "reunite" and self.household_id is None:
            raise ValueError("reunite requires a household ID")
        if self.expected_return_minute is not None and self.action != "temporary_exit":
            raise ValueError("expected_return_minute is valid only for temporary exits")
        return self

    def selected_ids(self) -> tuple[str, ...]:
        ids = tuple(sorted(set(self.agent_ids + ((self.agent_id,) if self.agent_id else ()))))
        if not ids:
            raise ValueError("population command requires at least one agent id")
        if self.action in {"temporary_exit", "return", "transfer", "relocate"} and len(ids) != 1:
            raise ValueError(f"{self.action} requires exactly one agent id")
        if self.action == "reunite" and len(ids) < 2:
            raise ValueError("reunite requires at least two agent ids")
        return ids


def create_population_lifecycle_state() -> dict[str, Any]:
    """Create checkpoint-safe state for temporary absences and deferred ties."""
    return {
        "schema_version": 1,
        "policy_version": POPULATION_LIFECYCLE_POLICY_VERSION,
        "source_id": POPULATION_LIFECYCLE_SOURCE_ID,
        "people": {},
        "relationship_archive": [],
        "messages": {},
        "deferred_beliefs": {},
        "pending_returns": [],
        "pending_relocations": {},
        "history": [],
    }


def initialize_population_lifecycle_state(world: WorldState) -> dict[str, Any]:
    """Install the owned state namespace before the initial checkpoint is captured."""
    state = world.model_state.setdefault(
        POPULATION_LIFECYCLE_STATE_KEY, create_population_lifecycle_state()
    )
    validate_population_lifecycle_state(world)
    return state


# native/src/rules/population_lifecycle.rs: validate_population_lifecycle_state below, as a
# validator that says yes or Unsupported.
register_rust_validator(
    POPULATION_LIFECYCLE_STATE_KEY,
    native.rules("population_lifecycle").validate,
)


@kernel_only("population_lifecycle", "validate_population_lifecycle_state")
def validate_population_lifecycle_state(world: WorldState) -> None:
    """Validate exact schema, archived identities, pending admission, and bounded JSON.

    Nothing without population_lifecycle_v1. ValueError, in this order:
    "population_lifecycle_v1 has an invalid exact schema" (the ten fields, schema 1, this policy
    and source, at most MAX_RETAINED_PEOPLE, MAX_RELATIONSHIP_ARCHIVE and MAX_LIFECYCLE_HISTORY);
    for each retained person "... archived person is invalid" (absent, its six fields, a
    non-negative int since_minute, a list of source events, information of beliefs and
    query_pressure), then its Agent snapshot: "population lifecycle Agent snapshot has an invalid
    schema", "... snapshot identity is invalid", "population lifecycle profile snapshot is invalid",
    the ValueError of Activity and Mobility, "population lifecycle normalized Agent value is
    invalid" (stress, trust, support_need, stress_sensitivity: finite, 0..1), and json.dumps'
    ValueError for a value that is not finite; "... pending return is invalid" (its six fields, a
    retained absent person waiting once, a known zone, a non-negative int minute, a str event ID, a
    list of sources); "... pending relocation is invalid" (an admitted person, its eight fields,
    known zones, int minutes with due after start, a list of sources); "... relationship archive
    is invalid" (its five fields, a known namespace, a str pair key, two different IDs, a bool) and
    json.dumps' ValueError for its value; "... relocation task/event is inconsistent" (one
    travel_return task of the person due then, one pending presence_relocation
    agent_travel_complete event); "... retained message is invalid" (a mapping with its own ID)
    and json.dumps' ValueError; "... deferred beliefs are invalid" (a mapping) and json.dumps'
    ValueError; and, with population_presence_v1 (a PresenceState), "pending return is
    inconsistent with population_presence_v1" unless each waiting person is awaiting_admission.
    """


@kernel_only("population_lifecycle", "apply_presence_command")
def apply_population_presence_command(
    world: WorldState,
    event: ScheduledEvent,
    command: PopulationPresenceCommand | dict[str, Any],
) -> dict[str, Any]:
    """Apply one typed lifecycle command atomically and return its recorded receipts.

    The caller is the approved event/effect controller. An LLM must never call this
    directly; narrative instructions first pass through the typed EffectProgram.

    The command is parsed with PopulationPresenceCommand and its selected_ids; their TypeError or
    ValueError is EventApplicationError("population command is invalid: <error>"). Then
    EventApplicationError for "population lifecycle contract is not installed", "population command
    minute must match the current event boundary", and "expected return minute precedes the
    departure event" (EventSuppressed("temporary exit dropped: the person was not free before the
    expected return") for a retried exit). _apply_presence_action runs on a copy of the world; the
    copy's lifecycle, world and extension validation errors become EventApplicationError("population
    command failed validation: <error>"); the copy then replaces the world.
    """


@kernel_only("population_lifecycle", "apply_presence_action")
def _apply_presence_action(
    world: WorldState,
    candidate: WorldState,
    event: ScheduledEvent,
    typed: PopulationPresenceCommand,
    ids: tuple[str, ...],
) -> dict[str, Any]:
    """Run a typed command on the candidate copy; `world` reads what a deferral writes back.

    temporary_exit and transfer archive an admitted person (their information, body, dynamics and
    relationships; EventApplicationError when absent or already archived) and record the presence
    change. A person who is busy (an active activity, a support request or an owned task) is
    retried next minute on `world` with schedule_population_retry("presence-retry", "population
    exit") and EventSuppressed("population exit waits until the person is free: <reason>"); an exit
    whose return of the same EffectProgram is due is EventSuppressed. return restores a retained
    person to the command's or the recorded return zone, or a temporary room there, or makes them
    wait (a pending return, awaiting_admission); a return whose exit of the same program was
    deferred past it is EventSuppressed. relocate schedules a travel_return task and its
    agent_travel_complete event along an accessible, physically clear route (an admitted, idle
    person and a destination with room, else EventApplicationError); to the same zone it records the
    relocation at once. reunite sets the household of co-located admitted people (with the
    presence contract). Each change appends a lifecycle history row; returns the receipt.
    """


@kernel_only("population_lifecycle", "admit_pending_returns")
def admit_pending_returns(world: WorldState, minute: int) -> tuple[str, ...]:
    """Admit same-ID returnees in stable request order when destination capacity opens.

    In (minute, event_id, agent_id) order, each waiting person whose zone has room, or a temporary
    room, is restored there (presence "returned", history "admitted"); archived relationships whose
    two people are back are restored; then validate_population_lifecycle_state runs. Returns the
    admitted IDs.
    """


@kernel_only("population_lifecycle", "complete_presence_relocation")
def complete_presence_relocation(world: WorldState, event: ScheduledEvent) -> None:
    """Commit household/location receipts after the existing movement handler arrives.

    EventApplicationError("relocation completion does not match its pending route") unless the
    pending relocation of the event's agent_id has this event ID and due minute and the person is
    in its destination. Records the presence change ("relocated") and a history row, drops the
    relocation, runs admit_pending_returns at the due minute, then
    validate_population_lifecycle_state.
    """


@kernel_only("population_lifecycle", "current_household_id")
def current_household_id(world: WorldState, agent_id: str) -> str | None:
    """Resolve the mutable household ledger without rewriting immutable profile traits.

    With population_presence_v1 (a dict), the household_id of the person's record there (None for
    no record); without it, the profile's group_id of an admitted person, else None.
    """


@kernel_only("population_lifecycle", "zone_has_room")
def _zone_has_room(world: WorldState, zone_id: str, *, excluding: str | None = None) -> bool:
    """Whether the zone's residents and incoming travelers, `excluding` one person, are below its
    effective capacity (none: always)."""
