"""Installed alternative numerical operators; no evaluation of arbitrary code.

The kernels of native/src/rules know each class here by identity.
"""

from __future__ import annotations

from sheltersim.policies.information import InformationSpreadPolicy
from sheltersim.policies.psychology import SimplePsychologyPolicy
from sheltersim.policies.social import StressPropagationPolicy


class BoundedStressPolicy(SimplePsychologyPolicy):
    """Exponential pressure/recovery avoids a linear stress saturation step.

    The contributions are SimplePsychologyPolicy's. Their sum p adds `(1 - S)(1 - exp(-p))` to
    stress S, shared among them in proportion, and the recovery r removes `S(1 - exp(-r))`; the
    result is clipped to 0..1. Its step is native/core/src/rules/psychology.rs.
    """

    policy_version = "bounded-stress-v1"


class DeGrootContactPolicy(StressPropagationPolicy):
    """Sparse consensus permits calming and amplification from the same contacts.

    The contacts, their minutes and their draws are StressPropagationPolicy's. Each person moves
    toward the mean stress of the contacts that have lasted `minimum_contact_minutes`, by
    `min(1, stress_spread_per_minute)`, capped per pair and per person, after recovering by
    `natural_recovery_per_minute`; there is no cooldown. Its step is
    native/src/rules/social_propagation.rs.
    """

    policy_version = "degroot-contact-v1"


class HazardInformationPolicy(InformationSpreadPolicy):
    """InformationSpreadPolicy with a hazard: a transfer's probability is `1 - exp(-p)` of the
    sparse model's `p`. Its step is native/src/rules/information.rs."""

    policy_version = "hazard-information-v1"
