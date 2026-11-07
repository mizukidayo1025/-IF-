"""Compare the read-only rules of lane L5 with their Python rules at every call.

- At each update-phase view: `zone_environment_view` of every zone against
  `zone_environment.view_of`, every person's `PersonContext` against `person_context.fields_of`, the
  selected psychology's reading of beliefs, and one person per zone of the kernel's view. Python's
  and Rust's kept values are not used by the first three, so these compare new reads.
- The proximity step has no Python rule left to compare (KERNEL_ONLY_SEGMENTS, lane L10).

Run alone with SHELTERSIM_RULE_SHADOW_ONLY=read_rules, this keeps every tick on the Python path.

`seen` also says which formula each zone was read under and which side of each branch it took,
since most of these are selected by the Scenario, and a formula no Run selects is not compared here
(tests/integration/test_read_rules.py makes those inputs).
"""

from __future__ import annotations

from typing import Any

from shadow_kit import Recorder


def _exact(value: Any) -> Any:
    if value is None or type(value) is bool:
        return value
    if type(value) is float:
        return f"f{value.hex()}"
    if type(value) is int or isinstance(value, str):
        return [type(value).__name__, value]
    if isinstance(value, tuple):
        return ["tuple", *(_exact(item) for item in value)]
    if isinstance(value, dict):
        return ["map", *([key, _exact(item)] for key, item in value.items())]
    if hasattr(value, "__dataclass_fields__"):
        return [type(value).__name__, *([k, _exact(v)] for k, v in value.__dict__.items())]
    raise TypeError(type(value).__name__)


_SHARED = (
    "ambient_noise_level",
    "rest_efficiency_multiplier",
    "ventilation_effectiveness",
    "lighting_level",
    "privacy_level",
    "temperature_c",
    "ventilation_hygiene_growth_per_minute",
    "privacy_discontent_per_minute",
    "condition_modifier_event_ids",
    "condition_assumption_ids",
    "demand_modifiers",
    "min_rest_decision_minutes",
    "max_rest_decision_minutes",
)


def _reached(recorder: Recorder, world: Any, view: Any) -> None:
    from sheltersim.policies.registry import (
        create_zone_environment_formula,
        create_zone_hygiene_formula,
        create_zone_privacy_formula,
        create_zone_rest_formula,
    )

    seen = recorder.seen
    if "zone_environment" not in world.model_configs:
        seen["zones_without_the_policy"] += 1
        return
    configs = world.model_configs
    temperature = create_zone_environment_formula(configs).model_id
    rest = create_zone_rest_formula(configs).model_id
    hygiene = create_zone_hygiene_formula(configs).model_id
    privacy = create_zone_privacy_formula(configs).model_id
    seen[f"zones.{temperature}"] += 1
    seen[f"zones.{rest}"] += 1
    seen[f"zones.{hygiene}"] += 1
    seen[f"zones.{privacy}"] += 1
    seen[f"zones.{temperature}.stress_above_zero"] += view.temperature_stress_per_minute != 0.0
    seen[f"zones.{rest}.at_the_floor"] += view.rest_efficiency_multiplier == 0.1
    seen[f"zones.{rest}.with_noise"] += view.ambient_noise_level != 0.0
    seen[f"zones.{privacy}.discontent_above_zero"] += view.privacy_discontent_per_minute != 0.0
    seen[f"zones.{hygiene}.growth_above_zero"] += view.ventilation_hygiene_growth_per_minute != 0.0
    seen["zones.with_demand_modifiers"] += bool(view.demand_modifiers)
    seen["zones.with_condition_modifiers"] += bool(view.condition_modifier_event_ids)
    seen["zones.demand_at_its_limit"] += any(
        modifier.duration_multiplier in (0.5, 2.0) for modifier in view.demand_modifiers
    )


def _compare_reads(recorder: Recorder, native: Any, world: Any) -> None:
    from sheltersim.contracts.policies import selected_models
    from sheltersim.contracts.population import PersonContext, population_context_people
    from sheltersim.domain.environment import zone_environment_view
    from sheltersim.domain.psychological_state import LIFE_STATE_PSYCHOLOGY_POLICIES
    from sheltersim.policies.world_view import _fields_of_person

    unsupported = native.Unsupported
    zones = native.rules("zone_environment")
    people = native.rules("person_context")
    views = native.rules("world_view")
    seen = recorder.seen
    for zone_id in sorted(world.zones):
        seen["zones.calls"] += 1
        try:
            expected = zone_environment_view(world, zone_id)
        except Exception as raised:  # noqa: BLE001 - only compared with what the kernel does
            try:
                zones.view_of(world, zone_id)
            except unsupported:
                continue
            recorder.differ(f"zone {zone_id}: the kernel took what Python raises on: {raised}")
            continue
        try:
            found = zones.view_of(world, zone_id)
        except unsupported as reason:
            recorder.reasons[f"zone: {reason}"] += 1
            continue
        seen["zones.ran"] += 1
        left, right = _exact(expected), _exact(found)
        if left != right:
            recorder.differ(f"minute {world.minute_index}, zone {zone_id}: {left} against {right}")
        seen["zones.values_compared"] += len(expected.__dict__)
        _reached(recorder, world, expected)
    for identity, raw in population_context_people(world).items():
        seen["people.calls"] += 1
        expected = _fields_of_person(PersonContext.model_validate(raw))
        try:
            found = people.fields_of(raw)
        except unsupported as reason:
            recorder.reasons[f"person: {reason}"] += 1
            continue
        seen["people.ran"] += 1
        if _exact(expected) != _exact(found):
            recorder.differ(f"minute {world.minute_index}, person {identity}: {raw!r}")
        seen["people.values_compared"] += len(expected)
    expected = selected_models(world.model_configs)["psychology"] in LIFE_STATE_PSYCHOLOGY_POLICIES
    found = views.misinformation_applies_of(world)
    seen[f"misinformation_applies.{expected}"] += 1
    if expected is not found:
        recorder.differ(f"minute {world.minute_index}: beliefs read {found}, not {expected}")


def _compare_view(recorder: Recorder, world: Any, made: Any) -> None:
    from sheltersim.domain.environment import zone_environment_view

    done: set[str] = set()
    for agent in made.agents.values():
        if agent.zone_id in done:
            continue
        done.add(agent.zone_id)
        expected = zone_environment_view(world, agent.zone_id)
        recorder.seen["views.people_compared"] += 1
        for name in _SHARED:
            if _exact(getattr(expected, name)) != _exact(getattr(agent, name)):
                recorder.differ(f"minute {world.minute_index}, {agent.id}: {name} of the view")


def prepare(output: str) -> None:
    from sheltersim import native

    # The driver calls its kernels directly, past these stand-ins: alone, this shadow would see no
    # tick and report nothing while every hash passes.
    native.rules("tick").python_path_only(True)
    recorder = Recorder(output, "read_rules")
    views = native.rules("world_view")
    view_kernel = views.view_after_movement

    def view_after_movement(controller: Any, world: Any) -> Any:
        _compare_reads(recorder, native, world)
        made = view_kernel(controller, world)
        _compare_view(recorder, world, made)
        return made

    views.view_after_movement = view_after_movement
