"""Checkpoint-safe autonomy memory and facility awareness for synthetic Agents.

The rules are native/src/rules/agent_dynamics.rs's kernels (`kernel_only`); the validation of a
saved namespace stays here. A kernel refuses (KernelRefusal) a namespace its row can't hold or a
`facility_awareness` config the Scenario's model would refuse or coerce, before it changes anything.
"""

from __future__ import annotations

from typing import Any

from sheltersim import native
from sheltersim.core.kernels import kernel_only
from sheltersim.core.state_mirror import register_rust_validator
from sheltersim.core.world import WorldState
from sheltersim.policies.behavior import ActionProposal

AGENT_DYNAMICS_STATE_KEY = "agent_dynamics_v1"
AGENT_DYNAMICS_POLICY_VERSION = "need-led-autonomy-memory-v1"
ORIENTATION_AGENT_DYNAMICS_POLICY_VERSION = "need-led-autonomy-memory-v2"
# v3 keeps the v2 intake orientation and also announces shared facilities that
# are installed during a Run to the Agents already present.
ANNOUNCED_FACILITY_AGENT_DYNAMICS_POLICY_VERSION = "need-led-autonomy-memory-v3"
ORIENTATION_FACILITY_SOURCE_ID = "ASM-INTAKE-FACILITY-ORIENTATION-V1"
INSTALLED_FACILITY_ANNOUNCEMENT_SOURCE_ID = "ASM-INSTALLED-FACILITY-ANNOUNCEMENT-V1"
# Shared services every evacuee is assumed to be shown at intake. Group-specific
# spaces (pet, family, privacy) are still learned only on arrival.
ORIENTATION_FACILITY_KINDS = frozenset(
    {
        "intake",
        "meal_distribution",
        "medical_support",
        "quiet_space",
        "toilet",
        "water_station",
    }
)


@kernel_only("agent_dynamics", "canonical_config")
def canonical_agent_dynamics_config(raw: Any) -> dict[str, Any]:
    """Select the facility-awareness version; a missing config keeps v1 histories exact.

    None is `{}`. A new dict: `{"schema_version": 1, "policy_version": v1}`, or for v2 and v3 also
    `"source_id": ORIENTATION_FACILITY_SOURCE_ID`. ValueError, in this order: "agent_dynamics
    config has unsupported fields" (not a dict, or a key beside schema_version, policy_version and
    source_id); "agent_dynamics config has an unsupported version" (a schema that is not `== 1`, or
    a policy that is not one of the three); "agent_dynamics v1 has no orientation source" (v1 with
    a source_id); "agent_dynamics v2/v3 must cite the intake orientation assumption".
    """


@kernel_only("agent_dynamics", "create_state")
def create_agent_dynamics_state(world: WorldState) -> dict[str, Any]:
    """Create bounded decision memory without changing legacy Agent encodings.

    `{"schema_version": 1, "policy_version": ..., "agents": {...}}` with a record per agent in ID
    order: what it knows (the facilities of its zone, with v2 and v3 also those of
    ORIENTATION_FACILITY_KINDS, and the `facility_awareness` config's initial IDs, sorted), no last
    action, target, minute or reason, and its `next_decision_at` as the due minute.
    """


def validate_agent_dynamics_state(world: WorldState) -> None:
    """Reject malformed decision memory and references to absent Agents/facilities."""
    state = world.model_state.get(AGENT_DYNAMICS_STATE_KEY)
    if state is None:
        return
    if (
        not isinstance(state, dict)
        or set(state) != {"schema_version", "policy_version", "agents"}
        or state.get("schema_version") != 1
        or state.get("policy_version")
        != canonical_agent_dynamics_config(world.model_configs.get("agent_dynamics"))[
            "policy_version"
        ]
        or not isinstance(state.get("agents"), dict)
        or set(state["agents"]) != set(world.agents)
    ):
        raise ValueError("agent_dynamics_v1 has an invalid exact schema")
    for agent_id, record in state["agents"].items():
        if (
            not isinstance(record, dict)
            or set(record)
            != {
                "known_facility_ids",
                "last_action",
                "last_target_id",
                "last_decision_minute",
                "decision_due_minute",
                "decision_reason",
            }
            or not isinstance(record["known_facility_ids"], list)
            or len(record["known_facility_ids"]) != len(set(record["known_facility_ids"]))
            or set(record["known_facility_ids"]) - set(world.facilities)
            or not isinstance(record["decision_due_minute"], int)
            or record["decision_due_minute"] < 0
        ):
            raise ValueError(f"agent_dynamics_v1 record for {agent_id!r} is invalid")
        last = record["last_decision_minute"]
        if last is not None and (not isinstance(last, int) or isinstance(last, bool) or last < 0):
            raise ValueError(f"agent_dynamics_v1 decision minute for {agent_id!r} is invalid")


@kernel_only("agent_dynamics", "record_proposal")
def record_proposal(world: WorldState, proposal: ActionProposal, *, target_id: str | None) -> None:
    """Persist the selected need-led intention while retaining full trace in artifacts.

    The agent's record, if it has one, takes the action, the target, the proposal's minute, its
    `next_decision_at` as the due minute and its trigger as the reason. The namespace must exist.
    """


@kernel_only("agent_dynamics", "agent_knows_facility")
def agent_knows_facility(world: WorldState, agent_id: str, facility_id: str) -> bool:
    """Apply explicit awareness only when the Scenario selected that model.

    True when neither `facility_awareness` nor `agent_dynamics` is configured (older Scenarios had
    no knowledge gate); else whether the agent's record lists the facility, False without a
    namespace.
    """


@kernel_only("agent_dynamics", "learn_facility")
def learn_facility(world: WorldState, agent_id: str, facility_id: str) -> None:
    """Record a facility and visible co-located facilities after actual arrival.

    Nothing without the namespace, the facility or the agent's record. The record's list becomes the
    sorted union with the facility and, when the `facility_awareness` config discovers co-located
    facilities on arrival, every facility of its zone. A list that comes out the same is not
    written.
    """


@kernel_only("agent_dynamics", "announce_installed_facility")
def announce_installed_facility(world: WorldState, facility_id: str) -> None:
    """Tell present Agents about a shared facility installed during the Run (v3 only).

    Agents admitted later already learn it through intake orientation. Only a facility of
    ORIENTATION_FACILITY_KINDS; each record that lacks it gets it, sorted in.
    """


@kernel_only("agent_dynamics", "add_agent")
def add_agent(world: WorldState, agent_id: str) -> None:
    """Initialize memory for a dynamically admitted Agent: the record `create_agent_dynamics_state`
    gives it. Nothing without the namespace or the agent."""


def remove_agent(world: WorldState, agent_id: str) -> dict[str, Any] | None:
    """Remove active memory and return its detached record for departure history."""
    state = world.model_state.get(AGENT_DYNAMICS_STATE_KEY)
    return None if state is None else state["agents"].pop(agent_id, None)


register_rust_validator(AGENT_DYNAMICS_STATE_KEY, native.state("agent_dynamics").validate)
