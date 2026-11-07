"""Compare the kernel of the social action with its Python rule.

- The handler's apply: the Python rule on stand-ins of the two people; then the three values it
  writes, by their bits, on the objects; the rows are compared with the objects every 500 calls.
- The namespace's validator is in shadow_tick_validation.py, with the other rows'. observe_event,
  capture_context and validate_context have no Python rule left to compare with (`kernel_only`).
"""

from __future__ import annotations

from types import SimpleNamespace
from typing import Any

from shadow_kit import Recorder

SOCIAL_ACTIONS = {"reassurance_exchange", "family_care", "voluntary_support"}


def _person_copy(agent: Any) -> SimpleNamespace:
    needs = agent.needs
    return SimpleNamespace(
        id=agent.id,
        zone_id=agent.zone_id,
        profile=agent.profile,
        stress=agent.stress,
        trust=agent.trust,
        needs=SimpleNamespace(
            toilet_urgency=needs.toilet_urgency,
            hunger=needs.hunger,
            fatigue=needs.fatigue,
            hydration_need=needs.hydration_need,
        ),
    )


def _written(person: Any) -> list[str]:
    return [person.stress.hex(), person.trust.hex(), person.needs.fatigue.hex()]


def prepare(output: str) -> None:
    from sheltersim import native

    recorder = Recorder(output, "social_network")
    unsupported = native.Unsupported
    interaction = native.rules("social_interaction")
    apply_kernel = interaction.apply
    calls = [0]

    def apply(handler: Any, world: Any, event: Any) -> Any:
        from sheltersim.events.handlers import SocialInteractionHandler
        from sheltersim.events.models import EventSuppressed

        payload = event.payload
        kind = payload.get("interaction_type") or "plain"
        recorder.seen[f"apply.calls.{kind}"] += 1
        if kind not in SOCIAL_ACTIONS:
            # The Python rule of these writes the namespaces it reads: it runs once, after this.
            try:
                return apply_kernel(handler, world, event)
            except unsupported as reason:
                recorder.reasons[f"apply: {kind}: {reason}"] += 1
                raise
        ids = [payload.get("agent_id"), payload.get("participant_agent_id")]
        copies = {
            id_: _person_copy(world.agents[id_])
            for id_ in ids
            if isinstance(id_, str) and id_ in world.agents
        }
        stand_in = SimpleNamespace(
            agents=copies,
            model_state=world.model_state,
            minute_index=world.minute_index,
        )
        python_error = None
        try:
            SocialInteractionHandler.apply.__wrapped__(handler, stand_in, event)
        except Exception as raised:  # noqa: BLE001 - only compared with what the kernel does
            python_error = raised
        try:
            result = apply_kernel(handler, world, event)
        except unsupported as reason:
            detail = f"{reason}" if python_error is None else f"Python raises: {reason}"
            recorder.reasons[f"apply: {kind}: {detail}"] += 1
            raise
        except EventSuppressed as suppressed:
            # The text is the event's `detail`, and nothing may have changed.
            recorder.seen[f"apply.suppressed.{kind}"] += 1
            if type(python_error) is not EventSuppressed or str(python_error) != str(suppressed):
                recorder.differ(f"apply: suppressed {suppressed}, and Python: {python_error}")
            else:
                recorder.seen["apply.texts_compared"] += 1
            for id_ in copies:
                if _written(_person_copy(world.agents[id_])) != _written(copies[id_]):
                    recorder.differ(f"apply: a suppressed event changed {id_}")
            raise
        recorder.seen[f"apply.kernel_ran.{kind}"] += 1
        if python_error is not None:
            recorder.differ(f"apply: the kernel took what Python turned down: {python_error}")
            return result
        for id_, copy in copies.items():
            person = world.agents[id_]
            if _written(copy) != _written(person):
                recorder.differ(
                    f"apply minute {world.minute_index} {id_}: {_written(copy)} != "
                    f"{_written(person)}"
                )
            else:
                recorder.seen["apply.values_compared"] += 3
        calls[0] += 1
        if calls[0] % 500 == 0:
            recorder.seen["apply.row_checks"] += 1
            problem = world._core.entities().verify(world)
            if problem is not None:
                recorder.differ(f"apply minute {world.minute_index}: {problem}")
        return result

    interaction.apply = apply
