"""Allowlisted physical proximity samplers; these record proximity, never speech."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from sheltersim.domain.social_network import (
    PROXIMITY_CONTACT_POLICY_VERSION,
    PROXIMITY_DISABLED_POLICY_VERSION,
    canonical_proximity_contact_config,
)

DISABLED_PROXIMITY_POLICY_ID = PROXIMITY_DISABLED_POLICY_VERSION
DISTANCE_PROXIMITY_POLICY_ID = PROXIMITY_CONTACT_POLICY_VERSION


@dataclass(frozen=True)
class DisabledProximityPolicy:
    """No proximity contacts: it has no `config`, so the sample phase schedules none."""

    model_id: str = DISABLED_PROXIMITY_POLICY_ID


@dataclass(frozen=True)
class DistanceSampledProximityPolicy:
    """Deterministic same-zone Euclidean sampling with a bounded pair count.

    The sampling is native/core/src/rules/proximity.rs, the kernel of `_sample_proximity_contacts`
    (experiments/phases/sample.py): within each zone, the pairs of physical points at most
    `config["max_distance_m"]` apart, closest first, then by ID, at most
    `config["max_pairs_per_sample"]`.
    """

    config: dict[str, Any]
    model_id: str = DISTANCE_PROXIMITY_POLICY_ID


def create_proximity_policy(
    model_id: str, configs: dict[str, Any]
) -> DisabledProximityPolicy | DistanceSampledProximityPolicy:
    if model_id == DISABLED_PROXIMITY_POLICY_ID:
        return DisabledProximityPolicy()
    if model_id != DISTANCE_PROXIMITY_POLICY_ID:
        raise ValueError("unsupported installed social_proximity model")
    raw = configs.get("social_proximity") or {
        "schema_version": 1,
        "enabled": True,
        "policy_version": PROXIMITY_CONTACT_POLICY_VERSION,
        "sample_interval_minutes": 15,
        "max_distance_m": 1.5,
        "max_pairs_per_sample": 500,
    }
    config = canonical_proximity_contact_config(raw)
    if config is None:
        raise ValueError("selected distance proximity policy requires an enabled config")
    return DistanceSampledProximityPolicy(config)
