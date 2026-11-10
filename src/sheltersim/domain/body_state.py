"""Versioned long-term body and affect state for the registered life-state policy."""

from __future__ import annotations

import functools
import json
import math
import re
from typing import Any

from sheltersim import native
from sheltersim.core.history import records_since_previous_tick
from sheltersim.core.kernels import kernel_only
from sheltersim.core.state_mirror import (
    holds_detached,
    read_only_items,
    read_only_part,
    read_only_state,
    rust_accepts,
    validated_state,
)
from sheltersim.core.world import WorldState
from sheltersim.policies.affect import create_affect_policy

BODY_STATE_KEY = "body_state_v1"
BODY_POLICY_VERSION = "life-state-feedback-v1"
BODY_SOURCE_ID = "ASM-LIFE-STATE-FEEDBACK-V1"
MAX_SOCIAL_HISTORY = 20_000
MAX_SOCIAL_RELATIONSHIPS = 100_000

_DEFAULTS: dict[str, float] = {
    "awake_sleep_debt_minutes_per_minute": 1.0,
    "rest_sleep_debt_recovery_per_minute": 0.25,
    "sleep_sleep_debt_recovery_per_minute": 1.0,
    "sleep_debt_stress_rate": 0.001,
    "uncertainty_drift_per_minute": 0.00002,
    "uncertainty_decay_per_minute": 0.00005,
    "discontent_stress_rate": 0.001,
    "discontent_queue_rate": 0.0005,
    "discontent_relief_per_minute": 0.0002,
    "reassurance_decay_per_minute": 0.0003,
    "positive_affect_decay_per_minute": 0.0001,
    "negative_affect_stress_rate": 0.001,
    "hygiene_growth_per_minute": 0.00005,
    "sanitation_hygiene_growth_per_minute": 0.0001,
    "sanitation_stress_rate": 0.002,
    "affect_transfer_rate": 0.05,
    "relationship_trust_gain": 0.0005,
    "relationship_trust_decay_per_day": 0.002,
    "relationship_repeat_contact_bonus": 0.05,
    "cooperation_positive_gain": 0.03,
    "cooperation_negative_reduction": 0.02,
    "cooperation_reassurance_gain": 0.03,
    "cooperation_discontent_reduction": 0.01,
    "social_action_affect_transfer": 0.02,
    "complaint_reassurance_gain": 0.02,
    "complaint_discontent_reduction": 0.01,
    "hygiene_support_relief": 0.15,
    "caregiver_assistance_stress_relief": 0.03,
    "caregiver_assistance_reassurance_gain": 0.02,
    "privacy_accommodation_discontent_relief": 0.02,
    "mobility_assistance_stress_relief": 0.03,
    "language_access_uncertainty_relief": 0.05,
    "medical_assessment_uncertainty_relief": 0.02,
    "medication_support_uncertainty_relief": 0.01,
    "dependent_assistance_stress_relief": 0.04,
    "dependent_assistance_reassurance_gain": 0.03,
    "correction_uncertainty_reduction": 0.05,
    "correction_reassurance_gain": 0.01,
}


def canonical_body_config(raw: Any) -> dict[str, Any]:
    """Validate explicit units/ranges; arbitrary policy expressions are never accepted."""
    if raw is None:
        raw = {}
    if not isinstance(raw, dict):
        raise ValueError("body_dynamics config must be an object")
    allowed = {
        "schema_version",
        "policy_version",
        "source_id",
        *_DEFAULTS,
        "social_interaction_minutes",
        "affect_policy",
        "relationship_distance_scale_m",
        "sleep_debt_stress_rate_by_agent",
        "negative_affect_stress_rate_by_agent",
        "discontent_stress_rate_by_agent",
        "complaint_reassurance_gain_by_agent",
        "hygiene_support_relief_by_agent",
        "caregiver_assistance_stress_relief_by_agent",
        "caregiver_assistance_reassurance_gain_by_agent",
        "privacy_accommodation_discontent_relief_by_agent",
        "mobility_assistance_stress_relief_by_agent",
        "language_access_uncertainty_relief_by_agent",
        "medical_assessment_uncertainty_relief_by_agent",
        "medication_support_uncertainty_relief_by_agent",
        "dependent_assistance_stress_relief_by_agent",
        "dependent_assistance_reassurance_gain_by_agent",
    }
    if set(raw) - allowed:
        raise ValueError("body_dynamics config contains unsupported fields")
    config = {
        "schema_version": 1,
        "policy_version": BODY_POLICY_VERSION,
        "source_id": BODY_SOURCE_ID,
        **_DEFAULTS,
        "social_interaction_minutes": 5,
        "affect_policy": "linear-positive-negative-v1",
        "relationship_distance_scale_m": 2.0,
        **raw,
    }
    if (
        config["schema_version"] != 1
        or config["policy_version"] != BODY_POLICY_VERSION
        or config["source_id"] != BODY_SOURCE_ID
    ):
        raise ValueError("unsupported body_dynamics policy or source")
    for name in _DEFAULTS:
        value = config[name]
        if (
            not isinstance(value, (int, float))
            or isinstance(value, bool)
            or not math.isfinite(value)
            or not 0.0 <= value <= 1.0
        ):
            raise ValueError(f"body_dynamics {name} must be finite and within 0..1")
    if (
        type(config["social_interaction_minutes"]) is not int
        or not 1 <= config["social_interaction_minutes"] <= 1440
    ):
        raise ValueError("body_dynamics social_interaction_minutes must be within 1..1440")
    if not isinstance(config["affect_policy"], str):
        raise ValueError("body_dynamics affect_policy must be an installed model ID")
    create_affect_policy(config["affect_policy"])
    distance_scale = config["relationship_distance_scale_m"]
    if (
        not isinstance(distance_scale, (int, float))
        or isinstance(distance_scale, bool)
        or not math.isfinite(distance_scale)
        or not 0.1 <= distance_scale <= 100.0
    ):
        raise ValueError(
            "body_dynamics relationship_distance_scale_m must be within 0.1..100 metres"
        )
    if config["awake_sleep_debt_minutes_per_minute"] > 10.0:
        raise ValueError("awake sleep debt rate cannot exceed 10 minutes per minute")
    if (
        config["rest_sleep_debt_recovery_per_minute"] > 10.0
        or config["sleep_sleep_debt_recovery_per_minute"] > 10.0
    ):
        raise ValueError("sleep debt recovery cannot exceed 10 minutes per minute")
    for field, label in (
        ("sleep_debt_stress_rate_by_agent", "sleep debt rates"),
        ("negative_affect_stress_rate_by_agent", "negative-affect stress rates"),
        ("discontent_stress_rate_by_agent", "discontent stress rates"),
        ("complaint_reassurance_gain_by_agent", "reassurance gains"),
        ("hygiene_support_relief_by_agent", "hygiene support relief"),
        ("caregiver_assistance_stress_relief_by_agent", "caregiver stress relief"),
        ("caregiver_assistance_reassurance_gain_by_agent", "caregiver reassurance gains"),
        ("privacy_accommodation_discontent_relief_by_agent", "privacy discontent relief"),
        ("mobility_assistance_stress_relief_by_agent", "mobility stress relief"),
        ("language_access_uncertainty_relief_by_agent", "language-access uncertainty relief"),
        ("medical_assessment_uncertainty_relief_by_agent", "medical-support uncertainty relief"),
        ("medication_support_uncertainty_relief_by_agent", "medication-support uncertainty relief"),
        ("dependent_assistance_stress_relief_by_agent", "dependent-assistance stress relief"),
        (
            "dependent_assistance_reassurance_gain_by_agent",
            "dependent-assistance reassurance gains",
        ),
    ):
        per_agent = config.get(field)
        if per_agent is None:
            continue
        if not isinstance(per_agent, dict) or not per_agent:
            raise ValueError(f"body_dynamics per-Agent {label} must be a non-empty ID map")
        for agent_id, rate in per_agent.items():
            if (
                not isinstance(agent_id, str)
                or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._:-]{0,63}", agent_id)
                or type(rate) not in (int, float)
                or not math.isfinite(rate)
                or not 0.0 <= rate <= 1.0
            ):
                raise ValueError(f"body_dynamics per-Agent {label} are invalid")
    return config


# Accepts the header, records and relationships from the row (native/src/state/body_state.rs).
_ROWS_VALID = native.state("body_state").rows_valid


def create_body_state(world: WorldState) -> dict[str, Any]:
    """Initialize a detached record for each active Agent before checkpoint zero."""
    config, _ = _body_config_for_world(world)
    return {
        "schema_version": 1,
        "policy_version": BODY_POLICY_VERSION,
        "source_id": BODY_SOURCE_ID,
        "config": config,
        "agents": {
            agent_id: {
                "sleep_debt_minutes": min(1440.0, agent.needs.fatigue * 360.0),
                "uncertainty": min(1.0, 0.25 + agent.stress * 0.5),
                "reassurance": 0.0,
                "discontent": agent.stress,
                "positive_affect": 0.5,
                "negative_affect": agent.stress,
                "hygiene_need": 0.0,
            }
            for agent_id, agent in sorted(world.agents.items())
        },
        "relationships": {},
        "history": [],
    }


def _relationship_key(key: Any) -> tuple[tuple[Any, ...] | None, bool] | None:
    """None if key is not JSON, else (its two items if it is a list of two, whether
    key is their compact JSON)."""
    try:
        pair = json.loads(key)
    except (TypeError, json.JSONDecodeError):
        return None
    if not isinstance(pair, list) or len(pair) != 2:
        return None, False
    return tuple(pair), json.dumps(pair, separators=(",", ":")) == key


# Every validation, which runs at every tick, parses the same relationship keys, and
# the parse depends on the key alone (#228). Whether the people exist is checked
# every time.
_cached_relationship_key = functools.lru_cache(maxsize=65_536)(_relationship_key)


def _parse_key(key: Any) -> tuple[tuple[Any, ...] | None, bool] | None:
    return _cached_relationship_key(key) if type(key) is str else _relationship_key(key)


_RELATIONSHIP_FIELDS = frozenset(
    {
        "contact_count",
        "trust",
        "contact_minutes",
        "last_contact_minute",
        "last_interaction",
        "topic_ids",
    }
)


def validate_body_config(world: WorldState, state: dict[str, Any]) -> set[str]:
    """Check the saved config; return the people its per-person parameters name. Past the saved
    config and the configs, the check reads only whether each of them is here now or in the
    population state's history (native/src/rules/after_tick.rs checks that at each tick)."""
    config = canonical_body_config(state["config"])
    per_agent_fields = (
        "sleep_debt_stress_rate_by_agent",
        "negative_affect_stress_rate_by_agent",
        "discontent_stress_rate_by_agent",
        "complaint_reassurance_gain_by_agent",
        "hygiene_support_relief_by_agent",
        "caregiver_assistance_stress_relief_by_agent",
        "caregiver_assistance_reassurance_gain_by_agent",
        "privacy_accommodation_discontent_relief_by_agent",
        "mobility_assistance_stress_relief_by_agent",
        "language_access_uncertainty_relief_by_agent",
        "medical_assessment_uncertainty_relief_by_agent",
        "medication_support_uncertainty_relief_by_agent",
        "dependent_assistance_stress_relief_by_agent",
        "dependent_assistance_reassurance_gain_by_agent",
    )
    configured_agent_ids = set().union(*(set(config.get(field, {})) for field in per_agent_fields))
    past_residents = configured_agent_ids - set(world.agents)
    if past_residents:
        population = read_only_state(world, "population_dynamics_v1", {})
        historical_ids = set(world.agents)
        if isinstance(population, dict):
            historical_ids.update(population.get("initial_agent_ids", ()))
            historical_ids.update(population.get("profile_traits", {}))
        if configured_agent_ids - historical_ids:
            raise ValueError("body_state_v1 per-Agent parameter references an unknown resident")
    selected_affect = _registry_affect_model(world)
    registry_selected_affect = selected_affect is not None
    if selected_affect is not None and config["affect_policy"] != selected_affect:
        raise ValueError("body_state_v1 affect model does not match the shared Policy Registry")
    optional_compatibility_fields = {
        "social_interaction_minutes",
        "relationship_distance_scale_m",
        "relationship_repeat_contact_bonus",
        "hygiene_support_relief",
        "caregiver_assistance_stress_relief",
        "caregiver_assistance_reassurance_gain",
        "privacy_accommodation_discontent_relief",
        "mobility_assistance_stress_relief",
        "language_access_uncertainty_relief",
        "medical_assessment_uncertainty_relief",
        "medication_support_uncertainty_relief",
        "dependent_assistance_stress_relief",
        "dependent_assistance_reassurance_gain",
    }
    if not registry_selected_affect:
        optional_compatibility_fields.add("affect_policy")
    # The saved config may be the canonical one without some optional fields that
    # older versions did not write. Checking the fields directly accepts the same
    # configs as comparing with every such reduced config (up to 2**14), which ran
    # at every tick (#228).
    saved = state["config"]
    if (
        not isinstance(saved, dict)
        or not set(saved) <= set(config)
        or not set(config) - set(saved) <= optional_compatibility_fields
        or any(saved[key] is not config[key] and saved[key] != config[key] for key in saved)
    ):
        raise ValueError("body_state_v1 config is not canonical")
    return configured_agent_ids


def _validate_body_rows(world: WorldState, state: dict[str, Any]) -> None:
    """The rules of the records and relationships, when the kernel has not accepted them."""
    fields = {
        "sleep_debt_minutes",
        "uncertainty",
        "reassurance",
        "discontent",
        "positive_affect",
        "negative_affect",
        "hygiene_need",
    }
    for agent_id, record in state["agents"].items():
        if not isinstance(record, dict) or set(record) != fields:
            raise ValueError(f"body_state_v1 record for {agent_id!r} is invalid")
        for name, value in record.items():
            upper = 1440.0 if name == "sleep_debt_minutes" else 1.0
            if (
                not isinstance(value, (int, float))
                or isinstance(value, bool)
                or not math.isfinite(value)
                or not 0.0 <= value <= upper
            ):
                raise ValueError(f"body_state_v1 {name} for {agent_id!r} is invalid")
    agents = world.agents
    for key, record in state["relationships"].items():
        parsed = _cached_relationship_key(key) if type(key) is str else _relationship_key(key)
        if parsed is None:
            raise ValueError("body_state_v1 relationship key is invalid")
        pair, compact = parsed
        if (
            pair is None
            or pair[0] not in agents
            or pair[1] not in agents
            or pair[0] == pair[1]
            or not compact
            or not isinstance(record, dict)
            or record.keys() != _RELATIONSHIP_FIELDS
            or type(contact_count := record["contact_count"]) is not int
            or contact_count < 0
            or type(contact_minutes := record["contact_minutes"]) is not int
            or contact_minutes < 0
            or not isinstance(topic_ids := record["topic_ids"], list)
            or len(topic_ids) > 32
            or (
                topic_ids and any(not isinstance(item, str) or len(item) > 64 for item in topic_ids)
            )
        ):
            raise ValueError("body_state_v1 relationship record is invalid")
        trust = record["trust"]
        minute = record["last_contact_minute"]
        if trust is not None and (
            not isinstance(trust, (int, float)) or isinstance(trust, bool) or not 0 <= trust <= 1
        ):
            raise ValueError("body_state_v1 trust must be within 0..1")
        if minute is not None and (type(minute) is not int or minute < 0):
            raise ValueError("body_state_v1 contact minute is invalid")


def validate_body_state(
    world: WorldState,
    *,
    validate_event_history: bool = True,
    history_memo: Any = None,
) -> None:
    """Reject partial state, unknown people, out-of-range values and malformed evidence."""
    detached = holds_detached(world, BODY_STATE_KEY)
    state = None if detached else world.model_state.get(BODY_STATE_KEY)
    if state is None and not detached:
        return
    # True says the row holds what these rules would accept, so a config or history error below
    # keeps its place: everything before it passed. False says nothing, and they all run.
    rows_accepted = _ROWS_VALID is not None and rust_accepts(world, BODY_STATE_KEY, _ROWS_VALID)
    if detached:
        # The row alone holds it. Past the rows, a tick's check reads the config and the newest
        # history items only, which is all it makes of the row (state_mirror.RowItems).
        state = (
            {
                "config": read_only_part(world, BODY_STATE_KEY, "config"),
                "history": read_only_items(world, BODY_STATE_KEY, "history"),
            }
            if rows_accepted and history_memo is None
            else validated_state(world, BODY_STATE_KEY, history_memo)
        )
    if not rows_accepted and (
        not isinstance(state, dict)
        or set(state)
        != {
            "schema_version",
            "policy_version",
            "source_id",
            "config",
            "agents",
            "relationships",
            "history",
        }
        or state.get("schema_version") != 1
        or state.get("policy_version") != BODY_POLICY_VERSION
        or state.get("source_id") != BODY_SOURCE_ID
        or not isinstance(state.get("agents"), dict)
        or set(state["agents"]) != set(world.agents)
        or not isinstance(state.get("relationships"), dict)
        or len(state["relationships"]) > MAX_SOCIAL_RELATIONSHIPS
        or not isinstance(state.get("history"), list)
        or len(state["history"]) > MAX_SOCIAL_HISTORY
    ):
        raise ValueError("body_state_v1 has an invalid exact schema")
    validate_body_config(world, state)
    if not rows_accepted:
        _validate_body_rows(world, state)
    population = read_only_state(world, "population_dynamics_v1", {})
    historical_ids = set(world.agents)
    if isinstance(population, dict):
        historical_ids.update(population.get("initial_agent_ids", ()))
        historical_ids.update(population.get("profile_traits", {}))
        historical_ids.update(
            item.get("agent_id")
            for item in population.get("departures", ())
            if isinstance(item, dict) and isinstance(item.get("agent_id"), str)
        )
    history = state["history"]
    if not validate_event_history:
        history = records_since_previous_tick(history, world.minute_index)
    elif history_memo is not None:
        history = history_memo.unread((BODY_STATE_KEY, "history"), history, historical_ids)
    for item in history:
        expected = {"event_id", "minute", "kind", "actor_ids", "topic_id", "causation_id", "deltas"}
        if (
            not isinstance(item, dict)
            or set(item) != expected
            or not isinstance(item["event_id"], str)
            or type(item["minute"]) is not int
            or item["minute"] < 0
            or not isinstance(item["kind"], str)
            or not isinstance(item["actor_ids"], list)
            or any(agent_id not in historical_ids for agent_id in item["actor_ids"])
            or not isinstance(item["deltas"], dict)
        ):
            raise ValueError("body_state_v1 social history item is invalid")


def add_body_agent(world: WorldState, agent_id: str) -> None:
    """Initialize a newly admitted Agent without inventing prior reassurance."""
    state = world.model_state.get(BODY_STATE_KEY)
    agent = world.agents.get(agent_id)
    if state is not None and agent is not None:
        state["agents"][agent_id] = {
            "sleep_debt_minutes": min(1440.0, agent.needs.fatigue * 360.0),
            "uncertainty": min(1.0, 0.25 + agent.stress * 0.5),
            "reassurance": 0.0,
            "discontent": agent.stress,
            "positive_affect": 0.5,
            "negative_affect": agent.stress,
            "hygiene_need": 0.0,
        }


def remove_body_agent(world: WorldState, agent_id: str) -> dict[str, Any] | None:
    """Remove active body/social state; population history retains the stable identity."""
    state = world.model_state.get(BODY_STATE_KEY)
    if state is None:
        return None
    removed = state["agents"].pop(agent_id, None)
    for key in sorted(tuple(state["relationships"])):
        if agent_id in json.loads(key):
            state["relationships"].pop(key)
    return removed


@kernel_only("body_state", "record_social_event")
def record_social_event(world: WorldState, event: Any, minute: int) -> None:
    """Apply bounded affect and trust changes only from typed, applied simulation events.

    Nothing without body_state_v1. Each change below is clamped to 0..1 and its history item (kind,
    sorted actor_ids, the str message_id or topic_id, causation_id, the changed fields' deltas, the
    contact distance of two people with positions, a positive finite
    interaction_duration_minutes or contact_minutes) joins the last MAX_SOCIAL_HISTORY items:

    - proximity_contact_update of two different current Agents with an int observed_minute: each
      directed relationship is recorded ("observed_proximity_contact", the payload's distance_m
      and contact_minutes), with no affect change.
    - information_contact_update of two different current Agents: the selected affect policy's
      contact contribution of the sender to the recipient (affect_transfer_rate, the directed
      relationship's trust or 0.5), and the relationship recorded ("information_contact").
    - social_interaction "cooperation" of two current Agents: for each, the policy's social-action
      scale of the relationship's trust times the cooperation gains and reductions of the config;
      both relationships recorded. "reassurance_exchange", "family_care", "voluntary_support":
      the actor's signed positive - negative affect through the policy's social action
      (social_action_affect_transfer) on the participant (uncertainty only for
      reassurance_exchange); the participant's relationship recorded. "complaint_response" of a
      current actor: the complaint gains, and negative_affect - 0.01.
    - information_correction: each Agent in the zone (under evidence-aware-rumor-v1 only those whose
      belief of the message was last corrected and accepted by this event) loses
      correction_uncertainty_reduction of uncertainty, gains correction_reassurance_gain and loses
      0.01 of negative_affect.

    Recording a relationship creates it (trust 0.5) unless MAX_SOCIAL_RELATIONSHIPS are held, moves
    its trust by the selected relationship policy, counts the contact and its minutes, and appends
    a "relationship_update" history item.
    """


def _registry_affect_model(world: WorldState) -> str | None:
    from sheltersim.contracts.policies import selected_models

    return selected_models(world.model_configs).get("affect")


def _body_config_for_world(world: WorldState) -> tuple[dict[str, Any], bool]:
    config = canonical_body_config(world.model_configs.get("body_dynamics"))
    selected = _registry_affect_model(world)
    if selected is not None:
        config["affect_policy"] = selected
    return config, selected is not None
