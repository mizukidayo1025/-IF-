"""The sample phase: the contacts that become next minute's endogenous events."""

from __future__ import annotations

from sheltersim.core.rng import KeyedRandom
from sheltersim.domain.social_network import PROXIMITY_CONTACT_SOURCE_ID
from sheltersim.events.models import EventKind, ScheduledEvent
from sheltersim.experiments.phases.segments import no_python_step


class SamplePhase:
    """Steps of TickPhase.SAMPLE_ENDOGENOUS_EVENTS, a mixin of OfflineSimulationController."""

    def _run_sample_phase(self, minute: int, rng: KeyedRandom) -> None:
        self._sample_information_contacts(minute, rng)
        self._sample_proximity_contacts(minute)

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
        raise no_python_step("_sample_information_contacts")

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
        """Schedule only reproducible same-zone physical proximity observations."""
        config = self.proximity_contact_config
        if (
            config is None
            or minute % config["sample_interval_minutes"] != 0
            or minute + 1 >= self.world.duration_minutes
        ):
            return
        for pair in self.proximity_policy.sample(self.world):
            due_minute = minute + 1
            sequence = self.world.allocate_event_sequence()
            self.world.schedule_event(
                ScheduledEvent(
                    event_id=f"proximity-contact-{sequence}",
                    kind=EventKind.ENDOGENOUS,
                    due_minute=due_minute,
                    priority=40,
                    sequence=sequence,
                    event_type="proximity_contact_update",
                    payload={
                        "first_agent_id": pair.first_agent_id,
                        "second_agent_id": pair.second_agent_id,
                        "zone_id": pair.zone_id,
                        "observed_minute": due_minute,
                        "contact_minutes": 0,
                        "distance_m": pair.distance_m,
                        "first_point": pair.first_point,
                        "second_point": pair.second_point,
                    },
                    source_id=PROXIMITY_CONTACT_SOURCE_ID,
                    provenance={
                        "source_type": "assumption",
                        "source_id": PROXIMITY_CONTACT_SOURCE_ID,
                        "policy_version": config["policy_version"],
                        "observed_minute": due_minute,
                    },
                )
            )
