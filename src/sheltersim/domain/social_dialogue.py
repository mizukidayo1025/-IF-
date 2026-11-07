"""Bounded proposal-state context and fictional dialogue examples for social events."""

from __future__ import annotations

from typing import Any

from sheltersim.core.kernels import kernel_only
from sheltersim.core.world import WorldState

_NEEDS = ("toilet_urgency", "hunger", "fatigue", "hydration_need")
_AFFECT = (
    "positive_affect",
    "negative_affect",
    "reassurance",
    "discontent",
    "uncertainty",
    "hygiene_need",
)
_INTERACTION_LINES = {
    "reassurance_exchange": "今わかっていることを、一緒に確かめませんか。",
    "family_care": "必要なことを一緒に確認しよう。",
    "voluntary_support": "手伝えることがあれば、一緒に確認しよう。",
    "complaint_response": "待ち時間や必要な支援について、状況を確認したいです。",
    "cooperation": "できる役割を分けて、一緒に進めよう。",
    "information_reaction": "聞いた内容の出どころを一緒に確かめませんか。",
}


@kernel_only("social_dialogue", "capture_context")
def capture_social_state_context(
    world: WorldState, participant_ids: tuple[str, ...] | list[str]
) -> dict[str, Any]:
    """Capture anonymous modeled state at proposal time for an applied event's history.

    Returns {"schema_version": 1, "captured_minute": world.minute_index, "participants": ...}: for
    each of the sorted, distinct IDs, its stress, trust and needs (toilet_urgency, hunger, fatigue,
    hydration_need) as floats, and as "affect" the float of each of positive_affect,
    negative_affect, reassurance, discontent, uncertainty and hygiene_need of its body_state_v1
    record, or None when the record lacks one. ValueError("social dialogue context requires one or
    two current participants") for no ID, more than two or one not in world.agents; then the
    ValueError of validate_social_state_context for the copy, with world.duration_minutes.
    """


@kernel_only("social_dialogue", "validate_context")
def validate_social_state_context(
    context: Any, participant_ids: tuple[str, ...] | list[str], due_minute: int
) -> None:
    """Reject malformed, future, or identity-mismatched model-state evidence.

    ValueError, in this order: "social dialogue state context does not match its event" unless the
    context is a mapping of exactly schema_version (== 1), captured_minute (an int within 0 and
    due_minute) and participants (a mapping whose keys are the set of participant_ids); then for
    each participant's state: "social dialogue participant state has unsupported fields" unless a
    mapping of exactly stress, trust, needs and affect; "social dialogue needs context is invalid"
    unless needs maps exactly the four needs; "social dialogue affect context is invalid" unless
    affect is None or maps exactly the six affect fields; and "social dialogue state values must be
    finite and within 0..1" for stress, trust, a need or an affect value that is not an int or
    float (bool excluded) within 0 and 1.
    """


def _state_cues(state: dict[str, Any]) -> list[str]:
    cues: list[str] = []
    if state["stress"] >= 0.65:
        cues.append("stress_high_at_proposal")
    for name, value in state["needs"].items():
        if value >= 0.65:
            cues.append(f"{name}_high_at_proposal")
    affect = state.get("affect")
    if isinstance(affect, dict):
        for name in ("negative_affect", "discontent", "uncertainty"):
            if affect[name] >= 0.65:
                cues.append(f"{name}_high_at_proposal")
        if affect["reassurance"] >= 0.65:
            cues.append("reassurance_high_at_proposal")
    return cues


def _state_based_line(
    state: dict[str, Any], *, interaction: str, role: str, changed: set[str]
) -> str:
    cues = _state_cues(state)
    if role == "participant" and interaction == "reassurance_exchange":
        if "reassurance" in changed or "uncertainty" in changed:
            if "stress_high_at_proposal" in cues or "uncertainty_high_at_proposal" in cues:
                return (
                    "不安がありましたが、"
                    "分かっていることを一緒に確かめられて少し安心できた気がします。"
                )
            return "分かっていることを一緒に確かめられて、少し安心できた気がします。"
    if role == "participant" and interaction == "complaint_response":
        if "discontent" in changed or "reassurance" in changed:
            return "困っていたことを聞いてもらえて、状況を整理できた気がします。"
    if "stress_high_at_proposal" in cues:
        return "少し不安があります。今できることを一緒に確認したいです。"
    need_labels = {
        "toilet_urgency": "トイレ",
        "hunger": "食事",
        "fatigue": "休息",
        "hydration_need": "水分",
    }
    for name in _NEEDS:
        if f"{name}_high_at_proposal" in cues:
            return f"{need_labels[name]}について、必要なことを一緒に確認したいです。"
    if "negative_affect_high_at_proposal" in cues or "discontent_high_at_proposal" in cues:
        return "気になっていることを聞いて、一緒に整理したいです。"
    if role == "participant" and interaction == "voluntary_support" and changed:
        return "声をかけてもらえて、必要なことを一緒に確認できました。"
    return _INTERACTION_LINES.get(interaction, "今の状況を一緒に確認したいです。")


def virtual_dialogue_example(
    record: dict[str, Any], observed_model_effects: list[dict[str, Any]]
) -> dict[str, Any] | None:
    """Create a per-person, state-cued example; it never represents recorded speech."""
    interaction = record.get("interaction_type")
    context = record.get("conversation_state_context")
    actor = record.get("agent_id")
    participant = record.get("participant_agent_id")
    expected = [item for item in (actor, participant) if isinstance(item, str)]
    try:
        validate_social_state_context(context, expected, record.get("minute", -1))
    except (TypeError, ValueError):
        return None
    if not isinstance(interaction, str) or not expected:
        return None

    role_ids: list[tuple[str, str]] = []
    if isinstance(actor, str):
        role_ids.append((actor, "initiator"))
    if isinstance(participant, str) and participant != actor:
        role_ids.append((participant, "participant"))
    turns = []
    for identity, role in role_ids:
        state = context["participants"][identity]
        changed: set[str] = set()
        for effect in observed_model_effects:
            effect_agents = effect.get("agent_ids", [])
            if identity in effect_agents:
                changed.update(
                    name
                    for name, value in effect.get("deltas", {}).items()
                    if isinstance(value, (int, float))
                    and not isinstance(value, bool)
                    and value != 0
                )
        turns.append(
            {
                "speaker_id": identity,
                "role": role,
                "example_text": _state_based_line(
                    state, interaction=interaction, role=role, changed=changed
                ),
                "state_cues": _state_cues(state),
                "proposal_state": state,
                "applied_model_changes": sorted(changed),
            }
        )
    if not turns:
        return None
    return {
        "policy_version": "proposal-state-dialogue-v1",
        "schema_version": 1,
        "event_id": record.get("event_id"),
        "interaction_type": interaction,
        "captured_minute": context["captured_minute"],
        "turns": turns,
        "label": "fictional_example_not_recorded_speech",
        "basis": "proposal-time modeled participant state and applied model-state deltas",
    }
