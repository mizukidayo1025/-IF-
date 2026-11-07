"""Sparse, deterministic contact-based information diffusion and inquiry signals.

A tick's sampling is native/src/rules/information.rs, which reads the policy's config and knows
each class by identity (`_sample_information_contacts` of experiments/phases/sample.py).
"""

from __future__ import annotations

from typing import Any

from sheltersim.domain.models import InformationBeliefStatus, InformationSpreadConfig


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

    def record_support_request(
        self,
        runtime_state: dict[str, Any],
        *,
        agent_id: str,
        request_id: str,
        minute: int,
    ) -> None:
        """Tie a real staff request to the unresolved information that raised its signal."""
        message_ids = sorted(
            message_id
            for message_id, belief in runtime_state["beliefs"][agent_id].items()
            if belief["belief"] == InformationBeliefStatus.HEARD.value
        )
        if not message_ids:
            return
        runtime_state["support_requests"].append(
            {
                "request_id": request_id,
                "agent_id": agent_id,
                "minute": minute,
                "message_ids": message_ids,
            }
        )
        runtime_state["counters"]["support_requests"] += 1
        runtime_state["query_pressure"][agent_id] = 0.0


class EvidenceAwareInformationPolicy(InformationSpreadPolicy):
    """Separate exposure, belief, sharing, and forgetting as explicit processes.

    Before the contacts of InformationSpreadPolicy, each belief's share propensity decays and the
    belief is forgotten with probability `1 - exp(-forgetting_rate_per_minute)`; a sender passes a
    claim with its share propensity raised by its distress.
    """

    policy_version = "evidence-aware-rumor-v1"
