"""Reproducible, inspectable behavior proposals; the views they read are in world_view."""

from __future__ import annotations

import math
from dataclasses import dataclass, field, replace
from enum import StrEnum
from types import MappingProxyType
from typing import Any, Mapping, Protocol

from sheltersim.core.rng import RNGKeyPart

# Re-exported: importers name these here.
from sheltersim.policies.world_view import (  # noqa: F401
    AgentView,
    FacilityView,
    NeedView,
    WorldView,
)


class ActionKind(StrEnum):
    """Small set of decisions supported by the first offline behavior model."""

    WAIT = "wait"
    REQUEST_TOILET = "request_toilet"
    REST = "rest"
    SLEEP = "sleep"
    SEEK_HELP = "seek_help"
    FETCH_WATER = "fetch_water"
    COLLECT_MEAL = "collect_meal"
    SEEK_INFORMATION = "seek_information"
    SOCIALIZE = "socialize"
    CARE_FAMILY = "care_family"
    HELP_OTHERS = "help_others"


@dataclass(frozen=True)
class ActionCandidate:
    """One candidate and the evidence needed to inspect its selection."""

    action: ActionKind
    eligible: bool
    trigger: str
    utility: float
    urgency_priority: int
    probability: float

    def __post_init__(self) -> None:
        object.__setattr__(self, "action", ActionKind(self.action))
        if not self.trigger:
            raise ValueError("candidate trigger must be non-empty")
        if not math.isfinite(self.utility):
            raise ValueError("candidate utility must be finite")
        if self.urgency_priority < 0:
            raise ValueError("urgency priority cannot be negative")
        if not math.isfinite(self.probability) or not 0.0 <= self.probability <= 1.0:
            raise ValueError("candidate probability must be between 0 and 1")
        if not self.eligible and self.probability != 0.0:
            raise ValueError("ineligible candidates must have zero probability")


@dataclass(frozen=True)
class ActionProposal:
    """Immutable action proposal with a complete, inspectable decision trace."""

    agent_id: str
    minute_index: int
    observed_state_hash: str
    action: ActionKind
    candidates: tuple[ActionCandidate, ...]
    trigger: str
    choice_value: float
    rng_key: tuple[RNGKeyPart, ...]
    selection_probability: float
    policy_version: str
    next_decision_at: int
    target_id: str | None = None

    def __post_init__(self) -> None:
        object.__setattr__(self, "action", ActionKind(self.action))
        object.__setattr__(self, "candidates", tuple(self.candidates))
        object.__setattr__(self, "rng_key", tuple(self.rng_key))
        if self.minute_index < 0 or self.next_decision_at <= self.minute_index:
            raise ValueError("proposal has invalid decision timing")
        if len(self.observed_state_hash) != 64:
            raise ValueError("observed_state_hash must be a SHA-256 hex digest")
        if not self.candidates or not self.policy_version or not self.trigger:
            raise ValueError("proposal requires candidates, policy version, and trigger")
        if not self.rng_key:
            raise ValueError("proposal must expose the RNG key used for selection")
        if not math.isfinite(self.choice_value) or not 0.0 <= self.choice_value < 1.0:
            raise ValueError("choice_value must be between 0 and 1")
        if (
            not math.isfinite(self.selection_probability)
            or not 0.0 < self.selection_probability <= 1.0
        ):
            raise ValueError("selected action probability must be between 0 and 1")
        eligible = [candidate for candidate in self.candidates if candidate.eligible]
        if not eligible or not math.isclose(sum(item.probability for item in eligible), 1.0):
            raise ValueError("eligible candidate probabilities must sum to one")
        selected = [candidate for candidate in eligible if candidate.action is self.action]
        if len(selected) != 1 or not math.isclose(
            selected[0].probability, self.selection_probability
        ):
            raise ValueError("proposal action must match one eligible candidate")

    def with_outcome(self, target_id: str | None, next_decision_at: int) -> ActionProposal:
        """replace(self, target_id=..., next_decision_at=...): the other checks read fields
        that do not change, so only the timing is checked again."""
        if type(next_decision_at) is not int or next_decision_at <= self.minute_index:
            return replace(self, target_id=target_id, next_decision_at=next_decision_at)
        proposal = object.__new__(ActionProposal)
        object.__setattr__(
            proposal,
            "__dict__",
            {**self.__dict__, "target_id": target_id, "next_decision_at": next_decision_at},
        )
        return proposal


class BehaviorPolicy(Protocol):
    """An installed behavior policy: the settings native/core/src/rules/behavior.rs decides with."""

    policy_version: str


@dataclass(frozen=True)
class BehaviorPolicyDefinition:
    """Safe presentation metadata for one installed behavior-policy variant."""

    variant: str
    label: str
    policy_version: str
    assumption_source_id: str
    selection_rule: str
    description: str
    parameters: tuple[str, ...]
    capabilities: tuple[str, ...]

    def to_dict(self) -> dict[str, Any]:
        return {
            "variant": self.variant,
            "registry_version": "behavior-policy-registry-v1",
            "label": self.label,
            "policy_version": self.policy_version,
            "assumption_source_id": self.assumption_source_id,
            "selection_rule": self.selection_rule,
            "description": self.description,
            "parameters": list(self.parameters),
            "capabilities": list(self.capabilities),
        }


BEHAVIOR_POLICY_REGISTRY: Mapping[str, BehaviorPolicyDefinition] = MappingProxyType(
    {
        "utility-softmax-v1": BehaviorPolicyDefinition(
            variant="utility-softmax-v1",
            label="効用 softmax",
            policy_version="utility-softmax-v1",
            assumption_source_id="ASM-UTILITY-BEHAVIOR-V1",
            selection_rule="eligible candidates are sampled by utility and urgency priority",
            description=(
                "utility scoreに応じた確率で候補を選びます。"
                "確率は実測データで校正されていない仮説です。"
            ),
            parameters=(
                "temperature",
                "decision_interval_minutes",
                "shared candidate utility coefficients",
            ),
            capabilities=("typed action proposals", "keyed random choice", "candidate trace"),
        ),
        "sleep-aware-utility-softmax-v2": BehaviorPolicyDefinition(
            variant="sleep-aware-utility-softmax-v2",
            label="睡眠負債対応 softmax v2",
            policy_version="sleep-aware-utility-softmax-v2",
            assumption_source_id="ASM-SLEEP-AWARE-BEHAVIOR-V2",
            selection_rule="eligible candidates are sampled by utility and urgency priority",
            description=(
                "睡眠負債を個別の必要睡眠時間で正規化し、"
                "必要時間相当の負債が効用へ1単位加算される仮定です。"
            ),
            parameters=(
                "temperature",
                "decision_interval_minutes",
                "sleep_debt_utility_weight",
                "shared candidate utility coefficients",
            ),
            capabilities=("typed action proposals", "keyed random choice", "candidate trace"),
        ),
        "priority-rule-v1": BehaviorPolicyDefinition(
            variant="priority-rule-v1",
            label="優先順位ルール",
            policy_version="priority-rule-v1",
            assumption_source_id="ASM-PRIORITY-RULE-BEHAVIOR-V1",
            selection_rule=(
                "highest urgency priority, then highest utility; "
                "keyed random choice only breaks exact ties"
            ),
            description=(
                "既存の候補適格条件とutility式を使い、優先度・utilityの順で選びます。"
                "同点時だけseed-keyedな抽選を行います。"
            ),
            parameters=(
                "decision_interval_minutes",
                "shared candidate utility coefficients",
                "action tie-breaking uses keyed random draws",
            ),
            capabilities=("typed action proposals", "keyed tie breaking", "candidate trace"),
        ),
    }
)


def behavior_policy_catalog() -> list[dict[str, Any]]:
    """Return the bounded registry catalog for Admin and scenario previews."""
    return [BEHAVIOR_POLICY_REGISTRY[key].to_dict() for key in sorted(BEHAVIOR_POLICY_REGISTRY)]


def behavior_policy_definition(
    variant: str, *, resource_service_enabled: bool, autonomy_enabled: bool = False
) -> dict[str, Any]:
    """Return a run-specific version and capabilities without hidden policy state."""
    if not isinstance(variant, str):
        raise ValueError("behavior policy variant must be a registered string")
    try:
        definition = BEHAVIOR_POLICY_REGISTRY[variant]
    except KeyError as exc:
        raise ValueError(f"unknown behavior policy variant {variant!r}") from exc
    version = (
        f"{definition.policy_version.removesuffix('-v1')}-autonomy-v2"
        if autonomy_enabled
        else definition.policy_version
    )
    if resource_service_enabled:
        version = f"{version.removesuffix('-v1')}-resource-service-v1"
    result = definition.to_dict()
    result["policy_version"] = version
    result["capabilities"] = [
        *definition.capabilities,
        *(("water and meal actions",) if resource_service_enabled else ()),
        *(("rhythm-aware care, support and social choices",) if autonomy_enabled else ()),
    ]
    return result


def create_behavior_policy(variant: str, **parameters: Any) -> BehaviorPolicy:
    """Build an installed variant around the shared typed candidate scorer."""
    resource_service_enabled = parameters.get("resource_service_enabled", False)
    autonomy_enabled = parameters.get("autonomy_enabled", False)
    definition = behavior_policy_definition(
        variant,
        resource_service_enabled=bool(resource_service_enabled),
        autonomy_enabled=bool(autonomy_enabled),
    )
    scorer = UtilityBehaviorPolicy(
        **parameters,
        policy_version=definition["policy_version"],
    )
    if variant in {"utility-softmax-v1", "sleep-aware-utility-softmax-v2"}:
        return scorer
    if variant == "priority-rule-v1":
        return PriorityRuleBehaviorPolicy(scorer, policy_version=definition["policy_version"])
    raise ValueError(f"unknown behavior policy variant {variant!r}")


@dataclass(frozen=True)
class UtilityBehaviorPolicy:
    """Offline utility/softmax policy; coefficients are explicit hypotheses.

    native/core/src/rules/behavior.rs decides with these fields (the propose step `_propose_due`).
    Each due person's candidate actions get a utility (plus 0.5 per urgency priority); the
    eligible ones are drawn with softmax weights at `temperature` (or the person's), times a
    zone's demand multiplier, from the keyed draw ("behavior", rng_key, id, "minute", minute,
    "policy", policy_version, "choice"). The next decision comes `decision_interval_minutes` (or
    the person's) later, scaled for rest and sleep by a zone's duration multiplier.
    """

    temperature: float = 0.2
    decision_interval_minutes: int = 15
    decision_interval_minutes_by_agent: dict[str, int] = field(default_factory=dict)
    temperature_by_agent: dict[str, float] = field(default_factory=dict)
    sleep_debt_utility_weight: float = 1.0
    policy_version: str = "utility-softmax-v1"
    resource_service_enabled: bool = False
    hydration_action_threshold: float = 0.25
    hunger_action_threshold: float = 0.7
    water_utility_base: float = 0.18
    water_utility_need_weight: float = 1.0
    meal_utility_base: float = 0.18
    meal_utility_need_weight: float = 0.85
    information_query_threshold: float = 1.1
    information_query_utility_weight: float = 0.0
    autonomy_enabled: bool = False

    def __post_init__(self) -> None:
        if not math.isfinite(self.temperature) or self.temperature <= 0.0:
            raise ValueError("temperature must be positive and finite")
        if self.decision_interval_minutes <= 0:
            raise ValueError("decision interval must be positive")
        if (
            not math.isfinite(self.sleep_debt_utility_weight)
            or self.sleep_debt_utility_weight < 0.0
        ):
            raise ValueError("sleep debt utility weight must be finite and non-negative")
        if (
            len(self.decision_interval_minutes_by_agent) > 100
            or len(self.temperature_by_agent) > 100
        ):
            raise ValueError("per-Agent behavior coefficients exceed the 100-resident limit")
        if any(
            not isinstance(agent_id, str)
            or not agent_id
            or len(agent_id) > 64
            or type(minutes) is not int
            or not 1 <= minutes <= 1440
            for agent_id, minutes in self.decision_interval_minutes_by_agent.items()
        ):
            raise ValueError("per-Agent decision intervals must be integer minutes in [1, 1440]")
        if any(
            not isinstance(agent_id, str)
            or not agent_id
            or len(agent_id) > 64
            or type(value) not in (int, float)
            or not math.isfinite(value)
            or not 0 < value <= 10
            for agent_id, value in self.temperature_by_agent.items()
        ):
            raise ValueError("per-Agent choice temperatures must be finite values in (0, 10]")
        if not self.policy_version:
            raise ValueError("policy_version must be non-empty")
        for name in (
            "hydration_action_threshold",
            "hunger_action_threshold",
            "water_utility_base",
            "water_utility_need_weight",
            "meal_utility_base",
            "meal_utility_need_weight",
            "information_query_threshold",
            "information_query_utility_weight",
        ):
            value = getattr(self, name)
            if not math.isfinite(value) or value < 0.0:
                raise ValueError(f"{name} must be finite and non-negative")
            maximum = 1.1 if name == "information_query_threshold" else 1.0
            if name.endswith("threshold") and value > maximum:
                raise ValueError(f"{name} must be between zero and {maximum:g}")


@dataclass(frozen=True)
class PriorityRuleBehaviorPolicy:
    """Rank the shared eligible action set, with keyed randomness for exact ties.

    native/core/src/rules/behavior.rs takes the candidates of `scorer` and chooses among those with
    the highest (urgency priority, utility), sorted by the action's value, by the keyed draw of
    `scorer` with "priority-tie-break" added; each tied candidate has probability 1/n.
    """

    scorer: UtilityBehaviorPolicy
    policy_version: str = "priority-rule-v1"

    def __post_init__(self) -> None:
        if not self.policy_version:
            raise ValueError("policy_version must be non-empty")


class BehaviorDecisionRunner:
    """The policy the propose phase decides with (`_propose_due`, rules/behavior.rs of the core)."""

    def __init__(self, policy: BehaviorPolicy | None = None) -> None:
        self.policy = policy or UtilityBehaviorPolicy()
