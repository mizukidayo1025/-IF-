"""Read-only, detached views of the world, which policies read instead of WorldState."""

from __future__ import annotations

from dataclasses import dataclass, field
from types import MappingProxyType
from typing import Any, Mapping

from sheltersim.domain.environment import ZoneDemandView
from sheltersim.domain.models import Activity, Mobility


@dataclass(frozen=True)
class NeedView:
    """Detached read-only copy of an Agent's current needs."""

    toilet_urgency: float
    hunger: float
    fatigue: float
    hydration_need: float = 0.0


@dataclass(frozen=True)
class AgentView:
    """The behavior-relevant, immutable fields for one Agent."""

    id: str
    group_id: str | None
    zone_id: str
    mobility: Mobility
    support_need: float
    stress_sensitivity: float
    rng_key: str
    activity: Activity
    target_id: str | None
    needs: NeedView
    stress: float
    trust: float
    next_decision_at: int
    resource_service_enabled: bool = False
    queue_task_id: str | None = None
    queue_kind: str | None = None
    queue_target_id: str | None = None
    queue_wait_minutes: int = 0
    support_request_id: str | None = None
    support_request_status: str | None = None
    support_assigned_staff_id: str | None = None
    support_wait_minutes: int = 0
    support_cooldown_remaining_minutes: int = 0
    information_query_pressure: float = 0.0
    misinformation_belief_pressure: float = 0.0
    misinformation_effect_sources: tuple[tuple[str, str, str, str, int], ...] = ()
    environmental_stress_per_minute: float = 0.0
    environmental_stressor_event_ids: tuple[str, ...] = ()
    environmental_assumption_source_id: str | None = None
    ambient_noise_level: float = 0.0
    rest_efficiency_multiplier: float = 1.0
    zone_environment_modifier_event_ids: tuple[str, ...] = ()
    zone_environment_assumption_ids: tuple[str, ...] = ()
    ventilation_effectiveness: float = 0.7
    lighting_level: float = 0.5
    privacy_level: float = 0.5
    temperature_c: float = 20.0
    ventilation_hygiene_growth_per_minute: float = 0.0
    privacy_discontent_per_minute: float = 0.0
    condition_modifier_event_ids: tuple[str, ...] = ()
    condition_assumption_ids: tuple[str, ...] = ()
    demand_modifiers: tuple[ZoneDemandView, ...] = ()
    min_rest_decision_minutes: int = 1
    max_rest_decision_minutes: int = 60
    sleep_debt_minutes: float = 0.0
    uncertainty: float = 0.0
    reassurance: float = 0.0
    discontent: float = 0.0
    positive_affect: float = 0.5
    negative_affect: float = 0.0
    hygiene_need: float = 0.0
    sanitation_level: float = 1.0
    caregiver_ids: tuple[str, ...] = ()
    care_for_ids: tuple[str, ...] = ()
    parent_ids: tuple[str, ...] = ()
    roles: tuple[str, ...] = ()
    usual_wake_minute: int | None = None
    usual_sleep_minute: int | None = None
    required_sleep_minutes: int | None = None
    personal_context_present: bool = False
    chronic_distress: float = 0.0

    @property
    def acute_distress(self) -> float:
        """Compatibility name for the existing, single `stress` field."""
        return self.stress


@dataclass(frozen=True)
class FacilityView:
    """Detached immutable facility information available to policies."""

    id: str
    kind: str
    zone_id: str
    capacity: int
    working_units: int
    active_agent_ids: tuple[str, ...]
    queue_agent_ids: tuple[str, ...]
    resource_active_agent_ids: tuple[str, ...] = ()
    resource_queue_agent_ids: tuple[str, ...] = ()
    facility_event_context_ids: tuple[str, ...] = ()


# What a view made by rules/world_view.rs makes when a Python reader first asks for it.
_MADE_FROM_RUST = frozenset({"agents", "facilities", "zone_members", "group_members"})


@dataclass(frozen=True)
class WorldView:
    """Read-only, detached snapshot passed across policy boundaries.

    The phases make it in native/src/rules/world_view.rs, which reads a snapshot's values itself;
    Python has no other maker of one (lane L10). Only rules still in Python then pay for the Python
    objects, in __getattr__. A view made here (the constructor, `dataclasses.replace`) has no
    snapshot: the kernels that need one (the decisions') refuse it.
    """

    minute_index: int
    agents: Mapping[str, AgentView]
    facilities: Mapping[str, FacilityView]
    state_hash: str | None
    resource_service_enabled: bool = False
    autonomy_enabled: bool = False
    zone_members: Mapping[str, tuple[str, ...]] = field(default_factory=dict)
    group_members: Mapping[str, tuple[str, ...]] = field(default_factory=dict)

    def __post_init__(self) -> None:
        # Copy before wrapping so callers cannot mutate either the view or its source dict.
        agents = dict(self.agents)
        object.__setattr__(self, "agents", MappingProxyType(agents))
        object.__setattr__(self, "facilities", MappingProxyType(dict(self.facilities)))
        zones: dict[str, list[str]] = {}
        groups: dict[str, list[str]] = {}
        for agent_id, agent in sorted(agents.items()):
            zones.setdefault(agent.zone_id, []).append(agent_id)
            if agent.group_id is not None:
                groups.setdefault(agent.group_id, []).append(agent_id)
        object.__setattr__(
            self,
            "zone_members",
            MappingProxyType({key: tuple(value) for key, value in sorted(zones.items())}),
        )
        object.__setattr__(
            self,
            "group_members",
            MappingProxyType({key: tuple(value) for key, value in sorted(groups.items())}),
        )

    def __getattr__(self, name: str) -> Any:
        # Runs only for an attribute the instance doesn't have.
        fields = self.__dict__
        rust = fields.get("_rust_view")
        if rust is None or name not in _MADE_FROM_RUST:
            raise AttributeError(f"{type(self).__name__!r} object has no attribute {name!r}")
        agents, facilities, zone_members, group_members = rust.python_fields()
        fields["agents"] = MappingProxyType(agents)
        fields["facilities"] = MappingProxyType(facilities)
        fields["zone_members"] = MappingProxyType(zone_members)
        fields["group_members"] = MappingProxyType(group_members)
        return fields[name]

    def immediate_needs(self) -> dict[str, float]:
        """The largest of each person's stress and needs; proposals read it many times."""
        values = self.__dict__.get("_immediate_needs")
        if values is None:
            values = {}
            for agent_id, agent in self.agents.items():
                needs = agent.needs
                values[agent_id] = max(
                    agent.stress,
                    needs.toilet_urgency,
                    needs.hunger,
                    needs.fatigue,
                    needs.hydration_need,
                )
            object.__setattr__(self, "_immediate_needs", values)
        return values
