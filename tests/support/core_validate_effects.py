"""Broken EffectProgram and zone environment states and Python's errors for the core's whole-world
validation (#250, #351).

`validate_zone_environment_runtime_state` (domain/environment.py, wrapped as "invalid zone
environment state: {exc}") and `validate_effect_program_state` (events/effects.py, wrapped as
"invalid EffectProgram state: {exc}"), each with and without the event history.
`native/core/tests/validate_effects.rs` compares the core's answers with these
(`support/core_validate.py`).

The cases change the file's own world, `effects` (`_effects_world`): the generated world of
`validate_worlds.json` with a second program scheduled, whose effects change a zone's noise and
its demand and take a toilet out of use (with a guard), run `TICKS` ticks. Its
zone_environment_v1 then holds two modifiers whose start events are in the log and whose expiry
events wait in the queue, and effect_program_v1 a facility's modifier stack.

Not here: a zone_environment config the contract refuses (or one of zone-environment-v1 with a
condition modifier), and a missing config. The extensions' check of the world's formula selection
reads the config first and raises its own text; the core ports the rule all the same. A demand
modifier without resource_service_v1 needs a WorldState v5 world with the environment policy,
which no base has.
"""

from __future__ import annotations

import copy
from collections.abc import Callable
from typing import Any

from support.core_validate import Case, cases_fixture

TICKS = 15
PROGRAM = "f-environment"
ENVIRONMENT = "zone_environment_v1"
EFFECTS = "effect_program_v1"
# The program of the generated world itself (facility moves).
MOVES = "fixture-facility-moves"
TOILET = "toilet_01"

Edit = Callable[[dict[str, Any]], None]


def _action(op: str, kind: str, target: str, parameters: dict[str, Any], **options: Any) -> Any:
    from sheltersim.events.effects import DEFAULT_PRIMITIVE_REGISTRY, TargetSelector, TimedAction

    spec = DEFAULT_PRIMITIVE_REGISTRY.get(op)
    assert spec is not None
    end = options.pop("end")
    return TimedAction(
        op=op,
        target_selector=TargetSelector(kind=kind, target_id=target),
        end_minute=end,
        parameters=parameters,
        units=sorted(spec.units)[0],
        source_id="ASM-F-EFFECTS",
        policy_dependencies=sorted(spec.required_policies),
        reversible=end is not None,
        **options,
    )


def environment_program() -> Any:
    """The second program of `_effects_world`."""
    from sheltersim.events.effects import EffectProgram, GuardExpression, SupportMode

    guard = GuardExpression.model_validate(
        {
            "operator": "and",
            "operands": [
                {"operator": "gte", "field": "simulation.minute", "value": 0},
                {"operator": "lt", "field": "target.capacity", "value": 1000.5},
            ],
        }
    )
    return EffectProgram(
        program_id=PROGRAM,
        narrative="A noisy night and a toilet out of use.",
        source_text="A noisy night and a toilet out of use.",
        simulation_start_minute=0,
        effects=[
            _action(
                "set_ambient_noise",
                "zone",
                "sleeping",
                {"level": 0.8},
                effect_id="noise",
                start_minute=2,
                end=40,
                assumption_ids=["ASM-F-NOISE"],
            ),
            _action(
                "modify_demand_rate",
                "zone",
                "sleeping",
                {"action": "rest", "probability_multiplier": 1.5},
                effect_id="demand",
                start_minute=3,
                end=50,
                assumption_ids=[],
            ),
            _action(
                "set_facility_availability",
                "facility",
                TOILET,
                {"working_units": 0},
                effect_id="toilet-off",
                start_minute=4,
                end=30,
                assumption_ids=[],
                guard=guard,
            ),
        ],
        assumptions=[
            {
                "assumption_id": "ASM-F-NOISE",
                "description": "Night noise of a crowded hall.",
                "source_id": "ASM-F-EFFECTS",
                "units": "ratio",
                "value": 0.8,
            },
            {
                "assumption_id": "ASM-F-COUNT",
                "description": "A count.",
                "source_id": "ASM-F-EFFECTS",
                "value": 3,
            },
        ],
        support_mode=SupportMode.COMPOSED,
        compiler_provenance={
            "compiler_kind": "llm",
            "compiler_version": "test-v1",
            "model_id": "model-1",
            "prompt_hash": "abc",
        },
    )


def _effects_world() -> Any:
    from sheltersim.events.effects import schedule_effect_program
    from support.core_tick import generated_run

    world, _, engine = generated_run()
    schedule_effect_program(world, environment_program())
    for _ in range(TICKS):
        engine.step()
    return world


BASES = {"effects": _effects_world}


# ---- Edits ---------------------------------------------------------------------------------------


def _then(*edits: Edit) -> Edit:
    def edit(document: dict[str, Any]) -> None:
        for each in edits:
            each(document)

    return edit


def _node(document: dict[str, Any], path: tuple[Any, ...]) -> Any:
    node: Any = document
    for key in path:
        node = node[key]
    return node


def _set(*path: Any, value: Any) -> Edit:
    def edit(document: dict[str, Any]) -> None:
        _node(document, path[:-1])[path[-1]] = copy.deepcopy(value)

    return edit


def _drop(*path: Any) -> Edit:
    def edit(document: dict[str, Any]) -> None:
        del _node(document, path[:-1])[path[-1]]

    return edit


def _modifier(document: dict[str, Any], op: str) -> dict[str, Any]:
    (record,) = [
        record
        for record in document["model_state"][ENVIRONMENT]["modifiers"].values()
        if record["op"] == op
    ]
    return record


def _env(which: str, **values: Any) -> Edit:
    """Change fields of the zone modifier of an operator."""

    def edit(document: dict[str, Any]) -> None:
        _modifier(document, which).update(copy.deepcopy(values))

    return edit


def _env_drop(op: str, name: str) -> Edit:
    def edit(document: dict[str, Any]) -> None:
        del _modifier(document, op)[name]

    return edit


def _rekey(op: str, key: str) -> Edit:
    """Store the zone modifier of an operator under another key."""

    def edit(document: dict[str, Any]) -> None:
        modifiers = document["model_state"][ENVIRONMENT]["modifiers"]
        old = next(key for key, record in modifiers.items() if record["op"] == op)
        modifiers[key] = modifiers.pop(old)

    return edit


def _events(document: dict[str, Any]) -> list[dict[str, Any]]:
    """The events of the queue and of the log's records, as the document holds them."""
    return [*document["event_queue"], *(record["event"] for record in document["event_log"])]


def _event(document: dict[str, Any], event_type: str, effect: str | None = None) -> dict[str, Any]:
    found = [
        event
        for event in _events(document)
        if event["event_type"] == event_type
        and (
            effect is None
            or (event.get("provenance") or {}).get(EFFECTS, {}).get("effect_id") == effect
        )
    ]
    assert found, (event_type, effect)
    return found[0]


def _event_edit(kind: str, effect: str | None = None, **values: Any) -> Edit:
    def edit(document: dict[str, Any]) -> None:
        _event(document, kind, effect).update(copy.deepcopy(values))

    return edit


def _payload(kind: str, effect: str | None = None, **values: Any) -> Edit:
    def edit(document: dict[str, Any]) -> None:
        _event(document, kind, effect)["payload"].update(copy.deepcopy(values))

    return edit


def _marker(kind: str, effect: str | None = None, **values: Any) -> Edit:
    def edit(document: dict[str, Any]) -> None:
        marker = _event(document, kind, effect)["provenance"][EFFECTS]
        marker.update(copy.deepcopy(values))

    return edit


def _marker_drop(kind: str, name: str, effect: str | None = None) -> Edit:
    def edit(document: dict[str, Any]) -> None:
        del _event(document, kind, effect)["provenance"][EFFECTS][name]

    return edit


def _program(*path: Any, value: Any, program: str = PROGRAM) -> Edit:
    return _set("model_state", EFFECTS, "programs", program, *path, value=value)


def _stack(*path: Any, value: Any) -> Edit:
    return _set("model_state", EFFECTS, "active_facility_modifiers", TOILET, *path, value=value)


def _facility(facility_id: str, **values: Any) -> Edit:
    def edit(document: dict[str, Any]) -> None:
        (facility,) = [item for item in document["facilities"] if item["id"] == facility_id]
        facility.update(values)

    return edit


def _config(**values: Any) -> Edit:
    def edit(document: dict[str, Any]) -> None:
        document["model_configs"]["zone_environment"].update(copy.deepcopy(values))

    return edit


def _case(name: str, *edits: Edit, in_flight: Any = None) -> Case:
    return Case(name, "effects", edit=_then(*edits), in_flight=in_flight)


START = "effect_program_environment"
EXPIRY = "effect_program_environment_expire"
APPLY = "effect_program_apply"
FACILITY_EXPIRY = "effect_program_expire"

CASES: list[Case] = [
    _case("effects_valid"),
    # zone_environment config and state.
    _case("zone_config_int_for_float_valid", _config(comfortable_max_temperature_c=26)),
    _case(
        "zone_config_not_canonical",
        _drop("model_configs", "zone_environment", "noise_rest_efficiency_loss"),
    ),
    _case(
        "zone_config_baseline_unknown_zone",
        _config(
            baseline_by_zone={
                "nowhere": {
                    "ventilation_effectiveness": 0.7,
                    "lighting_level": 0.5,
                    "privacy_level": 0.5,
                    "temperature_c": 20.0,
                }
            }
        ),
    ),
    _case("zone_state_missing", _drop("model_state", ENVIRONMENT)),
    _case("zone_state_schema_2", _set("model_state", ENVIRONMENT, "schema_version", value=2)),
    _case("zone_state_modifiers_list", _set("model_state", ENVIRONMENT, "modifiers", value=[])),
    _case("zone_state_extra", _set("model_state", ENVIRONMENT, "extra", value=1)),
    # A modifier record.
    _case("zone_modifier_bad_key", _rekey("set_ambient_noise", "modifier-xyz")),
    _case("zone_modifier_extra_field", _env("set_ambient_noise", extra=1)),
    _case("zone_modifier_missing_field", _env_drop("set_ambient_noise", "source_id")),
    _case("zone_modifier_unknown_op", _env("set_ambient_noise", op="set_colour")),
    _case("zone_modifier_op_list", _env("set_ambient_noise", op=["set_ambient_noise"])),
    _case("zone_modifier_zone_dict", _env("set_ambient_noise", zone_id={"zone": "sleeping"})),
    _case("zone_modifier_unknown_zone", _env("set_ambient_noise", zone_id="nowhere")),
    _case("zone_modifier_bad_program_id", _env("set_ambient_noise", program_id="bad id")),
    _case("zone_modifier_effect_id_number", _env("set_ambient_noise", effect_id=3)),
    _case("zone_modifier_end_event_number", _env("set_ambient_noise", end_event_id=5)),
    _case("zone_modifier_empty_assumption", _env("set_ambient_noise", assumption_ids=[""])),
    _case(
        "zone_modifier_repeated_assumption",
        _env("set_ambient_noise", assumption_ids=["ASM-F-NOISE", "ASM-F-NOISE"]),
    ),
    _case("zone_modifier_float_minute", _env("set_ambient_noise", start_minute=2.0)),
    _case("zone_modifier_bool_minute", _env("set_ambient_noise", end_minute=True)),
    _case("zone_modifier_end_before_start", _env("set_ambient_noise", end_minute=2)),
    _case("zone_modifier_end_past_duration", _env("set_ambient_noise", end_minute=10**6)),
    _case("zone_modifier_parameters_range", _env("set_ambient_noise", parameters={"level": 2.0})),
    _case("zone_modifier_parameters_missing", _env("set_ambient_noise", parameters={})),
    _case("zone_modifier_parameters_list", _env("set_ambient_noise", parameters=[0.8])),
    _case(
        "zone_modifier_demand_parameters",
        _env("modify_demand_rate", parameters={"action": "rest"}),
    ),
    _case(
        "zone_modifier_demand_duration",
        _env(
            "modify_demand_rate",
            parameters={"action": "seek_help", "duration_multiplier": 1.5},
        ),
    ),
    _case(
        "zone_modifier_demand_limits",
        _config(min_probability_multiplier=1.0),
        _env("modify_demand_rate", parameters={"action": "rest", "probability_multiplier": 0.5}),
    ),
    _case("zone_modifier_source", _env("set_ambient_noise", source_id="OTHER")),
    # Its events.
    _case("zone_modifier_unknown_start", _env("set_ambient_noise", start_event_id="ghost")),
    _case("zone_modifier_swapped_events", _env("set_ambient_noise", end_event_id="effect-event-4")),
    _case("zone_modifier_start_payload", _payload(START, "noise", parameters={"level": 0.7})),
    _case("zone_modifier_end_payload", _payload(EXPIRY, "noise", zone_id="quiet")),
    _case("zone_modifier_end_due", _event_edit(EXPIRY, "noise", due_minute=41)),
    _case("zone_modifier_end_cause", _event_edit(EXPIRY, "noise", causation_id="effect-event-6")),
    _case(
        "zone_modifier_start_without_marker",
        _event_edit(START, "noise", provenance={"source": "test"}),
    ),
    _case("zone_modifier_marker_phase", _marker(START, "noise", phase="expiry")),
    _case("zone_modifier_marker_dependencies", _marker(START, "noise", policy_dependencies=[])),
    _case("zone_modifier_marker_assumptions", _marker(START, "noise", assumption_ids=[])),
    _case(
        "zone_modifier_assumptions_unsorted",
        _env("set_ambient_noise", assumption_ids=["ASM-F-NOISE", "ASM-A"]),
        _marker(START, "noise", assumption_ids=["ASM-F-NOISE", "ASM-A"]),
    ),
    _case("zone_modifier_per_zone_limit", _config(max_active_modifiers_per_zone=1)),
    # The EffectProgram state itself.
    _case("effect_state_extra_field", _set("model_state", EFFECTS, "extra", value=1)),
    _case("effect_state_schema_2", _set("model_state", EFFECTS, "schema_version", value=2)),
    _case("effect_state_schema_float", _set("model_state", EFFECTS, "schema_version", value=1.0)),
    _case("effect_state_programs_list", _set("model_state", EFFECTS, "programs", value=[])),
    _case("effect_state_missing", _drop("model_state", EFFECTS)),
    # A program record.
    _case("effect_program_extra_field", _program("extra", value=1)),
    _case("effect_program_bad_hash", _program("program_hash", value="ABC")),
    _case("effect_program_source_hash_number", _program("source_text_hash", value=1)),
    _case("effect_program_empty_narrative", _program("narrative", value="")),
    _case("effect_program_support_mode", _program("support_mode", value="magic")),
    _case("effect_program_effect_id_number", _program("effect_ids", value=["noise", 1])),
    _case("effect_program_operators_list", _program("operators", value=[])),
    _case(
        "effect_program_effect_ids_repeat",
        _program("effect_ids", value=["noise", "noise", "demand", "toilet-off"]),
    ),
    _case("effect_program_operator_unknown", _program("operators", "noise", value="fly")),
    _case(
        "effect_program_scheduled_repeat",
        _program("scheduled_event_ids", value=["effect-event-4", "effect-event-4"]),
    ),
    _case(
        "effect_program_compiler_kind",
        _program("compiler_provenance", "compiler_kind", value="human"),
    ),
    _case(
        "effect_program_compiler_fields",
        _program("compiler_provenance", value={"compiler_version": "bad id", "extra": 1}),
    ),
    _case("effect_program_compiler_list", _program("compiler_provenance", value=["llm"])),
    _case(
        "effect_program_assumption_empty_description",
        _program("assumptions", 0, "description", value=""),
    ),
    _case("effect_program_assumption_value_list", _program("assumptions", 0, "value", value=[1])),
    _case(
        "effect_program_assumption_value_dict",
        _program("assumptions", 1, "value", value={"a": 1}),
    ),
    _case("effect_program_assumption_extra", _program("assumptions", 1, "extra", value=None)),
    _case("effect_program_assumption_not_dict", _program("assumptions", 1, value="ASM")),
    _case(
        "effect_program_assumption_repeat",
        _program("assumptions", 1, "assumption_id", value="ASM-F-NOISE"),
    ),
    _case(
        "effect_program_effect_id_invalid",
        _program("effect_ids", 2, value="bad id"),
        _program(
            "operators",
            value={
                "noise": "set_ambient_noise",
                "demand": "modify_demand_rate",
                "bad id": "set_facility_availability",
            },
        ),
    ),
    _case(
        "effect_program_scheduled_shared",
        _program("scheduled_event_ids", 0, value="effect-event-0"),
    ),
    _case("effect_program_scheduled_unknown", _program("scheduled_event_ids", 0, value="ghost")),
    _case("effect_program_scheduled_other_program", _marker(APPLY, "toilet-off", program_id=MOVES)),
    # The events' markers.
    _case(
        "effect_marker_extra_field",
        _marker(FACILITY_EXPIRY, "toilet-off", extra=1),
    ),
    _case("effect_marker_effect_number", _marker(FACILITY_EXPIRY, "toilet-off", effect_id=5)),
    _case(
        "effect_marker_unknown_effect",
        _marker(FACILITY_EXPIRY, "toilet-off", effect_id="ghost"),
    ),
    _case("effect_marker_op", _marker(FACILITY_EXPIRY, "toilet-off", op="facility_outage")),
    _case(
        "effect_marker_source_hash",
        _marker(FACILITY_EXPIRY, "toilet-off", source_text_hash="0" * 64),
    ),
    _case("effect_marker_phase", _marker(FACILITY_EXPIRY, "toilet-off", phase="middle")),
    _case(
        "effect_marker_compiler",
        _marker(
            FACILITY_EXPIRY,
            "toilet-off",
            compiler_provenance={"compiler_kind": "human", "compiler_version": "v"},
        ),
    ),
    _case(
        "effect_marker_compiler_list",
        _marker(FACILITY_EXPIRY, "toilet-off", compiler_provenance=["llm", "v1"]),
    ),
    _case("effect_marker_units", _marker(FACILITY_EXPIRY, "toilet-off", units="bad id")),
    _case(
        "effect_marker_assumptions_text",
        _marker(FACILITY_EXPIRY, "toilet-off", assumption_ids="ASM"),
    ),
    _case(
        "effect_marker_dependency_number",
        _marker(FACILITY_EXPIRY, "toilet-off", policy_dependencies=[1]),
    ),
    _case(
        "effect_marker_guard_on_expiry",
        _marker(FACILITY_EXPIRY, "toilet-off", guard={"operator": "eq"}),
    ),
    _case("effect_marker_guard_invalid", _marker(APPLY, "toilet-off", guard={"operator": "xor"})),
    _case(
        "effect_marker_guard_shape",
        # Its keys sorted, as a world read from its document holds an event's values (the
        # text shows their order).
        _marker(
            APPLY, "toilet-off", guard={"field": "zone.occupancy", "operator": "gt", "value": "a"}
        ),
    ),
    _case(
        "effect_marker_unscheduled",
        _program(
            "scheduled_event_ids",
            value=[
                "effect-event-4",
                "effect-event-5",
                "effect-event-6",
                "effect-event-7",
                "effect-event-8",
            ],
        ),
    ),
    _case(
        "effect_marker_derived_type",
        _marker(FACILITY_EXPIRY, "toilet-off", phase="derived"),
    ),
    _case(
        "effect_marker_derived_orphan",
        _marker(FACILITY_EXPIRY, "toilet-off", phase="derived"),
        _event_edit(FACILITY_EXPIRY, "toilet-off", causation_id="ghost"),
    ),
    _case(
        "effect_marker_event_type",
        _event_edit(FACILITY_EXPIRY, "toilet-off", event_type="effect_program_apply"),
    ),
    # The facility modifier stacks.
    _case(
        "effect_stack_unknown_facility",
        lambda document: document["model_state"][EFFECTS]["active_facility_modifiers"].__setitem__(
            "ghost", document["model_state"][EFFECTS]["active_facility_modifiers"].pop(TOILET)
        ),
    ),
    _case("effect_stack_extra_field", _stack("extra", value=1)),
    _case("effect_stack_baseline_bool", _stack("baseline_working_units", value=True)),
    _case("effect_stack_baseline_over_capacity", _stack("baseline_working_units", value=10**6)),
    _case("effect_stack_empty", _stack("modifiers", value=[])),
    _case("effect_stack_entry_fields", _stack("modifiers", 0, "extra", value=1)),
    _case("effect_stack_entry_id", _stack("modifiers", 0, "modifier_id", value="modifier-0")),
    _case("effect_stack_entry_program_number", _stack("modifiers", 0, "program_id", value=1)),
    _case("effect_stack_entry_negative", _stack("modifiers", 0, "sequence", value=-1)),
    _case("effect_stack_working_units", _facility(TOILET, working_units=1)),
    _case("effect_stack_entry_over_baseline", _stack("modifiers", 0, "working_units", value=3)),
]


def validate_effects_fixture() -> dict[str, Any]:
    """`validate_effects.json`: the zone environment and EffectProgram cases (#351)."""
    return cases_fixture(CASES, BASES)
