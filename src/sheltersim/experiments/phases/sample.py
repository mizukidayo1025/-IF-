"""The sample phase: the contacts that become next minute's endogenous events."""

from __future__ import annotations

from sheltersim.core.rng import KeyedRandom
from sheltersim.events.models import EventKind, ScheduledEvent


class SamplePhase:
    """Steps of TickPhase.SAMPLE_ENDOGENOUS_EVENTS, a mixin of OfflineSimulationController.

    The driver runs the phase in native/core/src/rules/tick/sample.rs, in this order:
    `_sample_information_contacts`, then `_sample_proximity_contacts` when there is a
    `proximity_contact_config`.
    """

    def _sample_information_contacts(self, minute: int, rng: KeyedRandom) -> None:
        """Next minute's information contacts, which native/src/rules/information.rs samples.

        Nothing without `information_policy`. Otherwise the model's contacts of this minute,
        drawn from `rng` among each person's zone and `zone_neighbors`, decide which claims pass;
        the step writes `world.model_state["information_spread_v1"]` (query pressure, contacts,
        counters, and the forgetting of `evidence-aware-rumor-v1`), schedules each transfer with
        `_schedule_information_contact`, and gives the contact trace to
        `artifacts.record_information_spread_trace` when the Run keeps a trace (every contact with
        a full trace, 20 with a summary).

        The kernel alone runs this step (segments.py, KERNEL_ONLY_SEGMENTS); what it turns down
        raises KernelRefusal: a model off or of a class it does not know, an RNG other than
        CounterRNG, a world at another minute, claims or beliefs that do not parse.
        """

    def _schedule_information_contact(
        self,
        minute: int,
        message_id: str,
        sender_agent_id: str,
        recipient_agent_id: str,
        belief: str,
        contact_basis: str,
        contact_minutes: int,
        probability: float,
        message_source_id: str,
        causation_id: str,
    ) -> None:
        """Next minute's transfer of one claim, called by the Python rule and the kernel."""
        due_minute = minute + 1
        if due_minute >= self.world.duration_minutes:
            return
        sequence = self.world.allocate_event_sequence()
        self.world.schedule_event(
            ScheduledEvent(
                event_id=f"information-contact-{sequence}",
                kind=EventKind.ENDOGENOUS,
                due_minute=due_minute,
                priority=40,
                sequence=sequence,
                event_type="information_contact_update",
                payload={
                    "message_id": message_id,
                    "sender_agent_id": sender_agent_id,
                    "recipient_agent_id": recipient_agent_id,
                    "belief": belief,
                    "contact_basis": contact_basis,
                    "contact_minutes": contact_minutes,
                },
                source_id=message_source_id,
                causation_id=causation_id,
                provenance={
                    "source_type": "assumption",
                    "source_id": "ASM-INFORMATION-SPREAD-V1",
                    "policy_version": self.information_policy.policy_version,
                    "transmission_probability": probability,
                },
            )
        )

    def _sample_proximity_contacts(self, minute: int) -> None:
        """Next minute's same-zone proximity contacts: native/core/src/rules/proximity.rs.

        Nothing without `proximity_contact_config`, at a minute that is not a multiple of its
        `sample_interval_minutes`, or when `minute + 1` is past the Run. Otherwise
        `proximity_policy` (DistanceSampledProximityPolicy) pairs the people whose physical points
        of `movement_v1` lie in the same zone and within `max_distance_m` (each person known, the
        point a finite x and y), closest first, then by ID, at most `max_pairs_per_sample`; each
        pair becomes a `proximity_contact_update` event due at `minute + 1` (priority 40, the
        source ASM-PROXIMITY-CONTACT-V1, the config's `policy_version`), with the distance and both
        points.

        The kernel alone runs this step (segments.py, KERNEL_ONLY_SEGMENTS); what it turns down
        raises KernelRefusal: a policy of another class, a minute or interval that is not an int,
        a coordinate whose grid cell does not fit 64 bits.
        """
