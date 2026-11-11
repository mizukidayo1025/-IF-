"""Checkpoint-safe, event-backed lifecycle receipts for autonomous activities.

Lifecycle records are synthetic state-transition evidence. They contain no generated
utterance and are separate from selected-policy proposals and completion events.

The four rules below are native/src/rules/action_lifecycle.rs's kernels (`kernel_only`). They keep
the open actions in the store `active_action_store` makes (the Rust rows); a plain dict is not one,
and a kernel raises TypeError for it. A receipt the kernels can't write as the receipts' model would
(`ActionLifecycleReceipt`: a reason past 120 characters, an ID that is not stable, a value that is
not finite, a transition before its start, an event ID or sequence the log already holds) is
refused before anything changes: KernelRefusal.
"""

from __future__ import annotations

from collections.abc import Mapping
from typing import Any

from sheltersim import native
from sheltersim.core.kernels import kernel_only
from sheltersim.core.world import WorldState
from sheltersim.events.models import ScheduledEvent

ACTION_LIFECYCLE_SOURCE_ID = "ASM-ACTION-LIFECYCLE-V1"
ACTION_LIFECYCLE_POLICY_VERSION = "applied-action-lifecycle-v2"


def active_action_store(world: WorldState, run_core: Any = None) -> Any:
    """What a controller keeps of the open actions: the Rust rows, in its Run (`run_core`,
    `Run::actions`) when it has one."""
    index = active_action_index(world)
    # A restored record's needs are a frozen payload's.
    for record in index.values():
        if isinstance(record["start_needs"], Mapping):
            record["start_needs"] = dict(record["start_needs"])
    return native.rules("action_lifecycle").ActiveActions(index, run_core)


def active_action_index(world: WorldState) -> dict[str, dict[str, Any]]:
    """Rebuild transient lookup state from persisted event evidence after restore."""
    active: dict[str, dict[str, Any]] = {}
    # The Rust EventLog knows which records decide the result, so a restored log makes
    # only those objects. They give the same dict, in the same order, as all of them.
    for row in world.event_log.active_lifecycle_records():
        event = row.event
        payload = event.payload
        agent_id = payload.get("agent_id")
        if not isinstance(agent_id, str):
            continue
        transition = payload.get("transition")
        if transition in {"completed", "interrupted", "abandoned", "censored"}:
            active.pop(agent_id, None)
        elif transition in {"started", "continued"}:
            active[agent_id] = {
                key: payload[key]
                for key in (
                    "action_id",
                    "agent_id",
                    "action",
                    "target_id",
                    "start_minute",
                    "reason_code",
                    "start_activity",
                    "start_needs",
                    "model_id",
                    "model_source_id",
                )
            }
            active[agent_id]["continued"] = transition == "continued"
    return active


@kernel_only("action_lifecycle", "record_action_proposal")
def record_action_proposal(
    world: WorldState,
    proposal: Any,
    *,
    target_id: str | None,
    committed: bool,
    active_actions: Any,
) -> None:
    """Start or continue an action only when the selected proposal changed actor state.

    Nothing for an uncommitted proposal or an agent not in the world. The same action and target as
    the open one appends one "continued" receipt, the first time. Another one closes the open action
    ("completed" for a `wait` begun before this minute, or a `rest`/`sleep` whose fatigue, or sleep
    debt, fell since its start; else "interrupted"; reason "superseded_by_new_applied_action") and
    opens the new one: a "started" receipt with ID `action-{sequence}`, the agent's activity and
    needs at the start.

    Each receipt is an APPLIED `agent_action_lifecycle` event `action-lifecycle-{sequence}` with the
    detail "autonomous action {transition}"; its sequence is allocated from the world's counter.
    """


@kernel_only("action_lifecycle", "record_applied_action_completion")
def record_applied_action_completion(
    world: WorldState,
    event: ScheduledEvent,
    minute: int,
    *,
    active_actions: Any,
) -> None:
    """Close the open action an applied typed event ends.

    Only when the log holds the event APPLIED (that object, or one whose row is the same), its
    `agent_id` (or `sender_agent_id`) has an open action, the event's target (`request_id` or
    `staff_id` for support events; `facility_id`, `participant_agent_id`, `staff_id` or `request_id`
    otherwise) is not another one, and its minute is not before the action's start. A toilet,
    resource or support completion and a social interaction complete the actions they serve; a queue
    abandonment abandons them; a resource cancellation and a support timeout interrupt them. The
    receipt's reason is the event's type and its causation the event.
    """


@kernel_only("action_lifecycle", "record_applied_population_exit")
def record_applied_population_exit(
    world: WorldState,
    event: ScheduledEvent,
    minute: int,
    *,
    active_actions: Any,
) -> None:
    """Interrupt the open actions of residents an applied population event took away.

    For an APPLIED `effect_program_population` event: its rows in `population_lifecycle_v1`'s
    history (a temporary exit or a transfer) and in `population_dynamics_v1`'s departures (which
    win) give each agent's reason; their open actions close "interrupted", in agent ID order, with
    no activity and the event as cause.
    """


@kernel_only("action_lifecycle", "finalize_active_actions")
def finalize_active_actions(
    world: WorldState,
    minute: int,
    *,
    active_actions: Any,
) -> None:
    """Close every action still open at the end of a Run, in agent ID order.

    "completed" only with the need relief `record_action_proposal` asks for; otherwise, or for an
    agent no longer in the world, "censored". The reason is "simulation_end_right_censor".
    """
