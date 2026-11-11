"""Checkpointable day-scale latent distress state for the research psychology model.

The update phase's `_update_long_term_psychology` (native/core/src/rules/psychology.rs) adds each
minute's final acute distress, after social influence, to a resident's day. At the last minute of
a day the long-term distress D becomes `baseline + retention (D - baseline) + acute_weight
(mean acute - baseline) + high_distress_weight (share of high minutes)`, within 0..1.
"""

from __future__ import annotations

import math
from typing import Any

from sheltersim import native
from sheltersim.core.state_mirror import register_rust_validator
from sheltersim.domain.models import validate_unit_interval

PSYCHOLOGICAL_STATE_KEY = "psychological_distress_v1"
PSYCHOLOGICAL_STATE_SCHEMA_VERSION = 1
PSYCHOLOGICAL_STATE_POLICY_VERSION = "mean-reverting-distress-v1"
INTEGRATED_PSYCHOLOGY_POLICY_VERSION = "mean-reverting-life-state-v1"
MEAN_REVERTING_PSYCHOLOGY_POLICIES = frozenset(
    {PSYCHOLOGICAL_STATE_POLICY_VERSION, INTEGRATED_PSYCHOLOGY_POLICY_VERSION}
)
LIFE_STATE_PSYCHOLOGY_POLICIES = frozenset(
    {"life-state-feedback-v1", INTEGRATED_PSYCHOLOGY_POLICY_VERSION}
)


def create_psychological_state(
    baselines: dict[str, float],
) -> dict[str, Any]:
    """Create one latent state and daily acute-load accumulator per resident."""
    agents: dict[str, dict[str, float | int]] = {}
    for agent_id, baseline in sorted(baselines.items()):
        validate_unit_interval(f"baseline distress for {agent_id}", baseline)
        agents[agent_id] = {
            "baseline_distress": float(baseline),
            "long_term_distress": float(baseline),
            "daily_acute_sum": 0.0,
            "daily_high_distress_minutes": 0,
            "daily_observed_minutes": 0,
            "completed_days": 0,
        }
    return {
        "schema_version": PSYCHOLOGICAL_STATE_SCHEMA_VERSION,
        "last_updated_minute": -1,
        "agents": agents,
    }


def validate_psychological_state(state: Any) -> None:
    """Validate the bounded, versioned latent state used by checkpoints."""
    if not isinstance(state, dict) or set(state) != {
        "schema_version",
        "last_updated_minute",
        "agents",
    }:
        raise ValueError("psychological distress state fields do not match schema v1")
    if state["schema_version"] != PSYCHOLOGICAL_STATE_SCHEMA_VERSION:
        raise ValueError("unsupported psychological distress state schema")
    minute = state["last_updated_minute"]
    if type(minute) is not int or minute < -1:
        raise ValueError("psychological distress last minute is invalid")
    agents = state["agents"]
    if not isinstance(agents, dict) or len(agents) > 100_000:
        raise ValueError("psychological distress agents must be a bounded object")
    fields = {
        "baseline_distress",
        "long_term_distress",
        "daily_acute_sum",
        "daily_high_distress_minutes",
        "daily_observed_minutes",
        "completed_days",
    }
    for agent_id, record in agents.items():
        if (
            not isinstance(agent_id, str)
            or not agent_id
            or not isinstance(record, dict)
            or set(record) != fields
        ):
            raise ValueError("psychological distress agent record is invalid")
        validate_unit_interval("baseline_distress", record["baseline_distress"])
        validate_unit_interval("long_term_distress", record["long_term_distress"])
        total = record["daily_acute_sum"]
        if (
            not isinstance(total, (int, float))
            or isinstance(total, bool)
            or not math.isfinite(total)
        ):
            raise ValueError("psychological distress daily integral must be finite")
        observed = record["daily_observed_minutes"]
        high = record["daily_high_distress_minutes"]
        days = record["completed_days"]
        if type(observed) is not int or not 0 <= observed < 1440:
            raise ValueError("psychological distress daily observed minutes are invalid")
        if type(high) is not int or not 0 <= high <= observed:
            raise ValueError("psychological distress high minutes are invalid")
        if type(days) is not int or days < 0 or total < 0 or total > observed + 1e-9:
            raise ValueError("psychological distress daily accumulation is invalid")


# Rust holds this namespace in native/src/state/psychological_distress.rs.
register_rust_validator(PSYCHOLOGICAL_STATE_KEY, native.state("psychological_distress").validate)
