"""Directed, checkpoint-safe contact and relationship records for Agents."""

from __future__ import annotations

import functools
import json
import math
from typing import Any, Mapping

from sheltersim import native
from sheltersim.core.kernels import kernel_only
from sheltersim.core.state_mirror import register_rust_validator
from sheltersim.core.world import WorldState
from sheltersim.events.models import ScheduledEvent

SOCIAL_NETWORK_STATE_KEY = "social_network_v1"
SOCIAL_NETWORK_POLICY_VERSION = "directed-contact-relationships-v1"
MAX_RELATIONSHIPS = 100_000
PROXIMITY_CONTACT_POLICY_VERSION = "distance-sampled-proximity-v1"
PROXIMITY_CONTACT_SOURCE_ID = "ASM-PROXIMITY-CONTACT-V1"
PROXIMITY_DISABLED_POLICY_VERSION = "disabled-proximity-v1"


def canonical_proximity_contact_config(raw: Any) -> dict[str, Any] | None:
    """Validate the opt-in physical-contact sampler's bounded assumptions."""
    if raw is None:
        return None
    if not isinstance(raw, Mapping) or set(raw) != {
        "schema_version",
        "enabled",
        "policy_version",
        "sample_interval_minutes",
        "max_distance_m",
        "max_pairs_per_sample",
    }:
        raise ValueError("social_proximity config fields do not match")
    if (
        type(raw["schema_version"]) is not int
        or raw["schema_version"] != 1
        or type(raw["enabled"]) is not bool
        or raw["policy_version"] != PROXIMITY_CONTACT_POLICY_VERSION
    ):
        raise ValueError("social_proximity config version is unsupported")
    interval = raw["sample_interval_minutes"]
    maximum = raw["max_distance_m"]
    pair_limit = raw["max_pairs_per_sample"]
    if (
        type(interval) is not int
        or not 1 <= interval <= 60
        or isinstance(maximum, bool)
        or not isinstance(maximum, (int, float))
        or not math.isfinite(float(maximum))
        or not 0.25 <= float(maximum) <= 10.0
        or type(pair_limit) is not int
        or not 1 <= pair_limit <= 10_000
    ):
        raise ValueError("social_proximity bounds are invalid")
    if not raw["enabled"]:
        return None
    return {
        "schema_version": 1,
        "enabled": True,
        "policy_version": PROXIMITY_CONTACT_POLICY_VERSION,
        "sample_interval_minutes": interval,
        "max_distance_m": float(maximum),
        "max_pairs_per_sample": pair_limit,
    }


def create_social_network_state() -> dict[str, Any]:
    """Return an empty versioned sparse social graph."""
    return {
        "schema_version": 1,
        "policy_version": SOCIAL_NETWORK_POLICY_VERSION,
        "relationships": {},
        "interaction_count": 0,
        "information_transfer_count": 0,
    }


def _pair_key_problem(pair_key: Any) -> str | None:
    """None for the compact JSON of two different strings, else what is wrong."""
    try:
        pair = json.loads(pair_key)
    except (TypeError, json.JSONDecodeError):
        return "unparsable"
    if (
        not isinstance(pair, list)
        or len(pair) != 2
        or any(not isinstance(item, str) for item in pair)
        or pair[0] == pair[1]
        or json.dumps(pair, separators=(",", ":")) != pair_key
    ):
        return "not a pair"
    return None


# Every validation, which runs at every tick, checks the same relationship keys, and a
# key's check depends on the key alone (#228).
_cached_pair_key_problem = functools.lru_cache(maxsize=65_536)(_pair_key_problem)


_RELATIONSHIP_FIELDS = frozenset(
    {
        "contact_count",
        "conversation_count",
        "cooperation_count",
        "trust",
        "shared_message_ids",
        "last_contact_minute",
        "last_interaction",
    }
)


def _is_count(value: object) -> bool:
    return isinstance(value, int) and not isinstance(value, bool) and value >= 0


def validate_social_network_state(world: WorldState) -> None:
    """Validate sparse directed edges without requiring every possible Agent pair."""
    state = world.model_state.get(SOCIAL_NETWORK_STATE_KEY)
    if state is None:
        return
    if (
        not isinstance(state, dict)
        or set(state)
        != {
            "schema_version",
            "policy_version",
            "relationships",
            "interaction_count",
            "information_transfer_count",
        }
        or state.get("schema_version") != 1
        or state.get("policy_version") != SOCIAL_NETWORK_POLICY_VERSION
        or not isinstance(state.get("relationships"), dict)
        or len(state["relationships"]) > MAX_RELATIONSHIPS
    ):
        raise ValueError("social_network_v1 has an invalid exact schema")
    # With the Rust kernels, state/social_network.rs accepts first (rust_validated at the caller),
    # and reads only the records written since its last check. Without them, or for a value it
    # leaves to Python, these rules decide. Every tick checks every relationship here, so each
    # record is checked inline, accepting exactly what the field-by-field rules accept.
    for pair_key, record in state["relationships"].items():
        problem = (
            _cached_pair_key_problem(pair_key)
            if type(pair_key) is str
            else _pair_key_problem(pair_key)
        )
        if problem == "unparsable":
            raise ValueError("social_network_v1 relationship key is invalid")
        if (
            problem is not None
            or not isinstance(record, dict)
            or record.keys() != _RELATIONSHIP_FIELDS
            or not _is_count(record["contact_count"])
            or not _is_count(record["conversation_count"])
            or not _is_count(record["cooperation_count"])
            or not isinstance(record["shared_message_ids"], list)
        ):
            raise ValueError("social_network_v1 relationship record is invalid")
        trust = record["trust"]
        if trust is not None and (
            not isinstance(trust, (int, float))
            or isinstance(trust, bool)
            or not 0.0 <= trust <= 1.0
        ):
            raise ValueError("social_network_v1 trust value is invalid")
        minute = record["last_contact_minute"]
        if minute is not None and (
            not isinstance(minute, int) or isinstance(minute, bool) or minute < 0
        ):
            raise ValueError("social_network_v1 contact time is invalid")


@kernel_only("social_network", "observe_event")
def observe_event(world: WorldState, event: ScheduledEvent, minute: int) -> None:
    """Extract only allowlisted, structured contact data from an applied event.

    information_contact_update with str recipient_agent_id and sender_agent_id: the directed record
    (recipient, sender) counts a contact and a conversation, takes last_contact_minute = minute,
    and keeps a str message_id among its last 64 shared_message_ids (a new one counts an
    information transfer); the interaction count grows by 1. proximity_contact_update with str
    first_agent_id and second_agent_id and an int observed_minute (bool excluded): both directed
    records count a contact at observed_minute, and the interaction count grows by 1 each.
    social_interaction of cooperation, information_reaction, reassurance_exchange, family_care or
    voluntary_support with str agent_id and participant_agent_id: both directed records count a
    conversation at minute with last_interaction (cooperation also counts a cooperation) and move
    trust (None counting as 0.5) by the delta, clamped to 0..1: none for information_reaction,
    0.01 each for cooperation; for the
    three social actions, with relief = social_action_affect_transfer (0.02) of body_state_v1's
    config times the actor's clamped positive_affect (0.5) - negative_affect (0.0), the actor's
    record by max(0, relief * 0.25) (0 for reassurance_exchange) and the participant's by
    relief * 0.5; the interaction count grows by 1. Each written record then takes the trust of
    body_state_v1's record of the same key, if it has one. One person with themselves records
    nothing. ValueError("social_network_v1 reached the configured relationship bound") when a new
    record would pass MAX_RELATIONSHIPS.
    """


def add_agent(world: WorldState, agent_id: str) -> None:
    """New Agents receive no invented prior relationships."""
    del world, agent_id


def remove_agent(world: WorldState, agent_id: str) -> list[dict[str, Any]]:
    """Detach a departed Agent's ties for immutable population departure history."""
    state = world.model_state.get(SOCIAL_NETWORK_STATE_KEY)
    if state is None:
        return []
    archived = []
    for pair_key in sorted(tuple(state["relationships"])):
        pair = json.loads(pair_key)
        if agent_id in pair:
            archived.append({"pair": pair, "record": state["relationships"].pop(pair_key)})
    return archived


register_rust_validator(SOCIAL_NETWORK_STATE_KEY, native.state("social_network").validate)
