"""Sparse, deterministic contact-based stress propagation policy.

The policy is disabled by default. Its coefficients are scenario assumptions, not
calibrated estimates of real shelter behavior.

A tick's step is native/src/rules/social_propagation.rs, which reads these classes' parameters and
knows each class by identity (`_social_propagation` of experiments/phases/update.py).
"""

from __future__ import annotations

import math
from dataclasses import asdict, dataclass
from dataclasses import field as dataclass_field
from typing import Any, Mapping

_SOURCE_ID = "ASM-SOCIAL-PROPAGATION-V1"
_BOSSE_SOURCE_ID = "ASM-BOSSE-AFFECT-CONTAGION-V1"
_DEGROOT_SOURCE_ID = "ASM-DEGROOT-CONTACT-V1"
SOCIAL_RUNTIME_STATE_KEY = "social_propagation_v1"


@dataclass(frozen=True)
class SocialPropagationParameters:
    """Bounded, auditable coefficients for one optional social stress model."""

    enabled: bool = False
    max_contacts_per_agent: int = 3
    minimum_contact_minutes: int = 5
    stress_spread_per_minute: float = 0.001
    natural_recovery_per_minute: float = 0.0005
    cooldown_minutes: int = 30
    max_pair_stress_increase: float = 0.05
    max_agent_stress_increase_per_minute: float = 0.05
    source_expressiveness_by_agent: dict[str, float] = dataclass_field(default_factory=dict)
    family_channel_strength: float = 1.0
    same_zone_channel_strength: float = 0.75
    nearby_zone_channel_strength: float = 0.5
    source_id: str = _SOURCE_ID

    def __post_init__(self) -> None:
        if not isinstance(self.enabled, bool):
            raise ValueError("social propagation enabled must be boolean")
        for name, value in (
            ("max_contacts_per_agent", self.max_contacts_per_agent),
            ("minimum_contact_minutes", self.minimum_contact_minutes),
            ("cooldown_minutes", self.cooldown_minutes),
        ):
            if not isinstance(value, int) or isinstance(value, bool):
                raise ValueError(f"{name} must be an integer")
            minimum = 0 if name == "max_contacts_per_agent" else 1
            maximum = 16 if name == "max_contacts_per_agent" else 10_080
            if not minimum <= value <= maximum:
                raise ValueError(f"{name} must be between {minimum} and {maximum}")
        for name in (
            "stress_spread_per_minute",
            "natural_recovery_per_minute",
            "max_pair_stress_increase",
            "max_agent_stress_increase_per_minute",
        ):
            value = getattr(self, name)
            if (
                not isinstance(value, (int, float))
                or isinstance(value, bool)
                or not math.isfinite(value)
                or not 0.0 <= value <= 0.25
            ):
                raise ValueError(f"{name} must be finite and between 0 and 0.25")
        for name in (
            "family_channel_strength",
            "same_zone_channel_strength",
            "nearby_zone_channel_strength",
        ):
            value = getattr(self, name)
            if (
                not isinstance(value, (int, float))
                or isinstance(value, bool)
                or not math.isfinite(value)
                or not 0.0 <= value <= 1.0
            ):
                raise ValueError(f"{name} must be finite and between 0 and 1")
        if len(self.source_expressiveness_by_agent) > 10_000 or any(
            not isinstance(agent_id, str)
            or not agent_id
            or type(value) not in (int, float)
            or isinstance(value, bool)
            or not math.isfinite(value)
            or not 0.0 <= value <= 1.0
            for agent_id, value in self.source_expressiveness_by_agent.items()
        ):
            raise ValueError("source_expressiveness_by_agent values must be within 0..1")
        if self.source_id not in {_SOURCE_ID, _BOSSE_SOURCE_ID, _DEGROOT_SOURCE_ID}:
            raise ValueError("social propagation source_id is not registered")

    @classmethod
    def from_mapping(
        cls,
        document: Mapping[str, Any] | None,
        *,
        source_id: str | None = None,
    ) -> SocialPropagationParameters:
        """Validate a scenario's social-propagation model configuration."""
        if document is None:
            return cls()
        if not isinstance(document, Mapping):
            raise ValueError("social_propagation model config must be an object")
        allowed = set(cls.__dataclass_fields__)
        provenance_keys = {"source_type", "source_id"}
        unknown = set(document) - allowed - provenance_keys
        if unknown:
            raise ValueError(f"unknown social_propagation settings: {sorted(unknown)}")
        source_type = document.get("source_type")
        configured_source = document.get("source_id")
        if source_type is not None and source_type != "assumption":
            raise ValueError("social_propagation source_type must be assumption")
        if configured_source is not None and configured_source not in {
            _SOURCE_ID,
            _BOSSE_SOURCE_ID,
            _DEGROOT_SOURCE_ID,
        }:
            raise ValueError("social_propagation source_id does not match its model assumption")
        if (
            source_id is not None
            and configured_source is not None
            and configured_source not in {source_id, _SOURCE_ID}
        ):
            raise ValueError("social_propagation source_id must match the selected social model")
        try:
            values = {key: value for key, value in document.items() if key in allowed}
            values["source_id"] = source_id or configured_source or _SOURCE_ID
            return cls(**values)
        except TypeError as exc:
            raise ValueError(f"invalid social_propagation settings: {exc}") from exc

    def provenance(self) -> dict[str, Any]:
        """Return source classification and the exact active coefficient values."""
        coefficients = asdict(self)
        coefficients.pop("source_id", None)
        return {
            "source_type": "assumption",
            "source_id": self.source_id,
            "policy_version": {
                _SOURCE_ID: "sparse-family-zone-contact-v1",
                _DEGROOT_SOURCE_ID: "degroot-contact-v1",
                _BOSSE_SOURCE_ID: "expressiveness-channel-openness-v1",
            }[self.source_id],
            "coefficients": coefficients,
            "notes": (
                "Experimental coefficients for sensitivity analysis; not empirically "
                "calibrated or validated for real populations."
            ),
        }


class StressPropagationPolicy:
    """Sample a bounded sparse contact graph and propose explainable stress changes.

    Each person meets at most `max_contacts_per_agent` others a minute, drawn by `CounterRNG`
    from household, zone and neighboring zones. A contact that has lasted
    `minimum_contact_minutes` raises the recipient's stress by the positive difference times
    `stress_spread_per_minute` and its minutes, within the pair and the person's caps and outside
    the person's cooldown; every stress also recovers by `natural_recovery_per_minute`.
    """

    policy_version = "sparse-family-zone-contact-v1"

    def __init__(self, parameters: SocialPropagationParameters | None = None) -> None:
        self.parameters = parameters or SocialPropagationParameters()


class ExpressivenessChannelOpennessPolicy(StressPropagationPolicy):
    """Bosse-inspired bounded affect synchrony over sparse family/zone contacts.

    The cited expressiveness-channel-openness structure motivates the operator. The
    contact weights and resident sensitivity mapping below remain
    assumptions of 避難所IF！; this is not a calibrated implementation of the published model.

    The same contacts as StressPropagationPolicy's; a contact's weight is the source's
    expressiveness times its channel's strength times the recipient's stress sensitivity, and
    the weighted mean difference moves the stress both ways, capped per pair and per person.
    """

    policy_version = "expressiveness-channel-openness-v1"
