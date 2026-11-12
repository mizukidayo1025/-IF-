"""Sparse, deterministic contact-based information diffusion and inquiry signals.

A tick's sampling is native/src/rules/information.rs, which reads the policy's config and knows
each class by identity (`_sample_information_contacts` of experiments/phases/sample.py).
"""

from __future__ import annotations

from sheltersim.domain.models import InformationSpreadConfig


class InformationSpreadPolicy:
    """Transfer typed claims through sampled local contacts, never a global broadcast.

    Each minute every person meets at most `max_contacts_per_agent` others of their zone and its
    neighbors, drawn by `CounterRNG`. A sender passes the claim it holds most strongly that the
    recipient holds less strongly, once the contact has lasted `minimum_contact_minutes`, with a
    probability from the contact's minutes, the recipient's trust, the contact's basis and whether
    it corrects; the transfer is next minute's event. Query pressure decays each minute.
    """

    policy_version = "sparse-information-contact-v1"

    def __init__(self, config: InformationSpreadConfig) -> None:
        self.config = config


class EvidenceAwareInformationPolicy(InformationSpreadPolicy):
    """Separate exposure, belief, sharing, and forgetting as explicit processes.

    Before the contacts of InformationSpreadPolicy, each belief's share propensity decays and the
    belief is forgotten with probability `1 - exp(-forgetting_rate_per_minute)`; a sender passes a
    claim with its share propensity raised by its distress.
    """

    policy_version = "evidence-aware-rumor-v1"
