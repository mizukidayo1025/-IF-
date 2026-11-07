"""Versioned effect-program contracts and deterministic event interpreter.

This module belongs to the event/domain layer and never imports the LLM provider or
application UI. Persisted program data is declarative; only explicitly registered
Python primitives can be scheduled.
"""

from __future__ import annotations

import hashlib
import json
import re
from collections.abc import Mapping
from dataclasses import dataclass
from enum import StrEnum
from types import MappingProxyType
from typing import TYPE_CHECKING, Annotated, Any, Literal

from pydantic import (
    BaseModel,
    ConfigDict,
    Field,
    StrictBool,
    StrictFloat,
    StrictInt,
    StrictStr,
    StringConstraints,
    field_validator,
    model_validator,
)

from sheltersim.contracts.operations import (
    SHARED_OPERATIONS_KEY,
    EquipmentTransferParameters,
    FacilityRepairParameters,
    StaffHandoffParameters,
    TemporaryRoomGeometry,
)
from sheltersim.contracts.population import DynamicArrivalPersonContext
from sheltersim.core.kernels import kernel_first, kernel_only
from sheltersim.domain.environment import (
    ZONE_ENVIRONMENT_POLICY,
    ZONE_ENVIRONMENT_STATE_KEY,
    AmbientNoiseParameters,
    DemandRateParameters,
    LightingParameters,
    PrivacyParameters,
    ScenarioZoneEnvironmentConfig,
    TemperatureParameters,
    VentilationParameters,
)
from sheltersim.events.models import (
    EventKind,
    EventSuppressed,
    ScheduledEvent,
)
from sheltersim.events.queue import EventsById

if TYPE_CHECKING:
    from sheltersim.core.world import WorldState
from sheltersim.domain.geometry_overlay import (
    GEOMETRY_OPERATORS,
    GEOMETRY_OVERLAY_KEY,
    MoveFacilityParameters,
    PartitionParameters,
    RouteAccessParameters,
    installed_facility_placement_enabled,
    installed_facility_point,
    parsed_facility_geometry,
    validate_geometry_change,
)
from sheltersim.domain.infrastructure import active_facility_outage
from sheltersim.domain.temporary_spaces import (
    TEMPORARY_SPACES_STATE_KEY,
)

StableId = Annotated[
    str,
    StringConstraints(
        strict=True, min_length=1, max_length=64, pattern=r"^[A-Za-z0-9][A-Za-z0-9._:-]*$"
    ),
]
NonNegativeMinute = Annotated[int, Field(strict=True, ge=0)]


class StrictContract(BaseModel):
    """Strict, versionable data contracts shared by planners and the interpreter."""

    model_config = ConfigDict(extra="forbid", frozen=True)


class SupportMode(StrEnum):
    NATIVE = "native"
    COMPOSED = "composed"
    APPROXIMATE = "approximate"
    CLARIFICATION_REQUIRED = "clarification_required"
    NOT_SIMULATABLE = "not_simulatable"


class TargetSelector(StrictContract):
    kind: StableId
    target_id: StableId | None = None
    candidate_ids: list[StableId] = Field(default_factory=list, max_length=100)

    @model_validator(mode="after")
    def selected_or_candidates(self) -> TargetSelector:
        if self.target_id is None and not self.candidate_ids:
            raise ValueError("a target ID or candidate IDs are required")
        if self.target_id is not None and self.candidate_ids:
            raise ValueError("a selected target cannot also contain candidate IDs")
        if len(set(self.candidate_ids)) != len(self.candidate_ids):
            raise ValueError("candidate target IDs must be unique")
        return self


GuardOperator = Literal["eq", "ne", "gt", "gte", "lt", "lte", "and", "or"]
GuardField = Literal[
    "simulation.minute",
    "target.capacity",
    "target.working_units",
    "target.queue_length",
    "zone.occupancy",
    "resource.available_quantity",
    "staff.available",
]
GuardValue = StrictStr | StrictInt | StrictFloat | StrictBool


class GuardExpression(StrictContract):
    """Bounded guard syntax; this contract never evaluates the expression."""

    operator: GuardOperator
    field: GuardField | None = None
    value: GuardValue | None = None
    operands: list[GuardExpression] = Field(default_factory=list, max_length=4)

    @field_validator("value")
    @classmethod
    def finite_guard_value(cls, value: GuardValue | None) -> GuardValue | None:
        if isinstance(value, float) and (value != value or abs(value) == float("inf")):
            raise ValueError("guard numeric values must be finite")
        if isinstance(value, str) and len(value) > 256:
            raise ValueError("guard string values are limited to 256 characters")
        return value

    @model_validator(mode="after")
    def operator_shape_and_complexity_are_valid(self) -> GuardExpression:
        if self.operator in {"eq", "ne", "gt", "gte", "lt", "lte"}:
            if self.field is None or self.value is None or self.operands:
                raise ValueError("comparison guards require one allowed field and scalar value")
            if self.operator in {"gt", "gte", "lt", "lte"} and (
                not isinstance(self.value, (int, float)) or isinstance(self.value, bool)
            ):
                raise ValueError("ordered comparisons require a numeric value")
        elif self.field is not None or self.value is not None or len(self.operands) < 2:
            raise ValueError("and/or guards require two or more operands and no leaf fields")

        def measure(node: GuardExpression, depth: int = 1) -> int:
            if depth > 4:
                raise ValueError("guard nesting depth cannot exceed four")
            return 1 + sum(measure(child, depth + 1) for child in node.operands)

        if measure(self) > 20:
            raise ValueError("guard AST cannot exceed 20 nodes")
        return self


class PlanAssumption(StrictContract):
    assumption_id: StableId
    description: Annotated[str, StringConstraints(strict=True, min_length=1, max_length=500)]
    source_id: StableId
    units: StableId | None = None
    value: str | int | float | bool | None = None

    @field_validator("value")
    @classmethod
    def finite_numeric_value(cls, value: Any) -> Any:
        if isinstance(value, float) and (value != value or abs(value) == float("inf")):
            raise ValueError("assumption values must be finite")
        return value


def _validate_bounded_json(value: Any, *, depth: int = 0) -> None:
    if depth > 8:
        raise ValueError("effect JSON parameters exceed maximum nesting depth")
    if value is None or isinstance(value, (str, bool, int)):
        if isinstance(value, str) and len(value) > 1000:
            raise ValueError("effect JSON strings are limited to 1000 characters")
        return
    if isinstance(value, float):
        if value != value or abs(value) == float("inf"):
            raise ValueError("effect JSON numbers must be finite")
        return
    if isinstance(value, list):
        if len(value) > 100:
            raise ValueError("effect JSON arrays are limited to 100 items")
        for item in value:
            _validate_bounded_json(item, depth=depth + 1)
        return
    if isinstance(value, dict):
        if len(value) > 100 or any(not isinstance(key, str) or len(key) > 256 for key in value):
            raise ValueError("effect JSON objects need short string keys and at most 100 fields")
        for item in value.values():
            _validate_bounded_json(item, depth=depth + 1)
        return
    raise ValueError("effect parameters must contain JSON values only")


class TimedAction(StrictContract):
    """One bounded, declarative effect; `op` remains open for future typed primitives."""

    effect_id: StableId
    op: StableId
    target_selector: TargetSelector
    start_minute: NonNegativeMinute
    end_minute: NonNegativeMinute | None = None
    guard: GuardExpression | None = None
    parameters: dict[str, Any] = Field(default_factory=dict)
    units: StableId
    source_id: StableId
    assumption_ids: list[StableId] = Field(default_factory=list, max_length=50)
    policy_dependencies: list[StableId] = Field(default_factory=list, max_length=50)
    causation_id: StableId | None = None
    reversible: bool = True

    @field_validator("parameters")
    @classmethod
    def parameters_are_bounded_json(cls, value: dict[str, Any]) -> dict[str, Any]:
        _validate_bounded_json(value)
        return value

    @model_validator(mode="after")
    def valid_time_window(self) -> TimedAction:
        if self.end_minute is not None and self.end_minute <= self.start_minute:
            raise ValueError("end_minute must be later than start_minute")
        return self


class CompilerProvenance(StrictContract):
    compiler_kind: Literal["offline_rule", "llm"]
    compiler_version: StableId
    model_id: StableId | None = None
    prompt_hash: Annotated[str, StringConstraints(strict=True, max_length=128)] | None = None


class EffectProgram(StrictContract):
    """Versioned program contract; it describes effects and never executes them."""

    schema_version: Literal[1] = 1
    program_id: StableId
    narrative: Annotated[str, StringConstraints(strict=True, min_length=1, max_length=1000)]
    source_text: Annotated[str, StringConstraints(strict=True, min_length=1, max_length=4000)]
    simulation_start_minute: NonNegativeMinute
    effects: list[TimedAction] = Field(min_length=1, max_length=50)
    assumptions: list[PlanAssumption] = Field(default_factory=list, max_length=100)
    support_mode: SupportMode
    compiler_provenance: CompilerProvenance

    @model_validator(mode="after")
    def effects_fit_the_program(self) -> EffectProgram:
        if any(effect.start_minute < self.simulation_start_minute for effect in self.effects):
            raise ValueError("effects cannot start before simulation_start_minute")
        if len({effect.effect_id for effect in self.effects}) != len(self.effects):
            raise ValueError("effect IDs must be unique")
        assumption_ids = {item.assumption_id for item in self.assumptions}
        if len(assumption_ids) != len(self.assumptions):
            raise ValueError("assumption IDs must be unique")
        if any(not set(effect.assumption_ids) <= assumption_ids for effect in self.effects):
            raise ValueError("effects may reference only declared assumptions")
        return self


class _AvailabilityParameters(StrictContract):
    working_units: Annotated[int, Field(strict=True, ge=0, le=1000)]


class _FacilityOutageParameters(StrictContract):
    units: Annotated[int, Field(strict=True, gt=0, le=1000)]


class _DeliveryParameters(StrictContract):
    warehouse_id: StableId
    resource_id: Literal["water_ml", "food_meal_units"]
    quantity: Annotated[int, Field(strict=True, gt=0, le=1_000_000_000)]


class _SupplyScheduleParameters(StrictContract):
    shipment_id: StableId
    to_minute: NonNegativeMinute


class _StaffAvailabilityParameters(StrictContract):
    available: Literal[False] = False


class _StaffRoleParameters(StrictContract):
    skills: list[StableId] = Field(min_length=1, max_length=20)

    @model_validator(mode="after")
    def unique_skills(self) -> _StaffRoleParameters:
        if len(set(self.skills)) != len(self.skills):
            raise ValueError("staff role skills must be unique")
        return self


class StaffTaskOrderParameters(StrictContract):
    """One immediate, explicit order for a registered routed staff task."""

    task_kind: Literal["cleaning", "maintenance", "information_outreach", "intake", "patrol"]
    target_zone_id: StableId
    target_id: StableId | None = None
    reason: Annotated[str, StringConstraints(strict=True, min_length=1, max_length=200)]

    @model_validator(mode="after")
    def facility_target_matches_task(self) -> StaffTaskOrderParameters:
        if (self.task_kind == "maintenance") != (self.target_id is not None):
            raise ValueError("only maintenance orders require a target facility ID")
        return self


def _staff_has_task_skill(skills: Any, task_kind: Any) -> bool:
    """Require a registered skill before assigning an active operation to staff."""
    requirements = {
        "cleaning": {"cleaning", "sanitation"},
        "maintenance": {"maintenance", "repair"},
        "information_outreach": {"support", "information"},
        "intake": {"support", "intake"},
        "patrol": {"support", "patrol"},
    }
    return isinstance(task_kind, str) and bool(set(skills) & requirements.get(task_kind, set()))


class _WaterOutageParameters(StrictContract):
    outage: Literal[True] = True


class _AdditionParameters(StrictContract):
    count: Annotated[int, Field(strict=True, gt=0, le=100)]


class _ToiletAdditionParameters(StrictContract):
    count: Annotated[int, Field(strict=True, gt=0, le=20)]


class _EvacueeArrivalParameters(StrictContract):
    count: Annotated[int, Field(strict=True, gt=0, le=1000)]
    household_size: Annotated[int, Field(strict=True, ge=1, le=12)] = 1
    mobility: Literal["independent", "assisted", "wheelchair"] = "independent"
    language: StableId = "ja"
    admission_zone_ids: tuple[StableId, ...] = Field(default=(), max_length=16)
    approach_route_id: StableId | None = None
    person_context_overrides: tuple[DynamicArrivalPersonContext, ...] = Field(
        default=(), max_length=1000
    )

    @model_validator(mode="after")
    def alternate_admission_zones_are_unique(self) -> _EvacueeArrivalParameters:
        if len(self.admission_zone_ids) != len(set(self.admission_zone_ids)):
            raise ValueError("admission_zone_ids cannot contain duplicates")
        member_indices = [item.member_index for item in self.person_context_overrides]
        if len(member_indices) != len(set(member_indices)):
            raise ValueError("arrival person context member indices cannot repeat")
        if any(index >= self.count for index in member_indices):
            raise ValueError("arrival person context member index exceeds count")
        return self


class _EvacueeDepartureParameters(StrictContract):
    count: Annotated[int, Field(strict=True, gt=0, le=1000)]
    include_initial: StrictBool = False


class _TemporarySpaceParameters(StrictContract):
    people_capacity: Annotated[int, Field(strict=True, gt=0, le=1000)]
    sleeping_capacity: Annotated[int, Field(strict=True, ge=0, le=1000)] = 0

    @model_validator(mode="after")
    def sleeping_capacity_fits(self) -> _TemporarySpaceParameters:
        if self.sleeping_capacity > self.people_capacity:
            raise ValueError("sleeping_capacity cannot exceed people_capacity")
        return self


class _PopulationPresenceParameters(StrictContract):
    """A bounded command for one retained person or a selected reunion group."""

    action: Literal["temporary_exit", "return", "transfer", "relocate", "reunite"]
    zone_id: StableId | None = None
    household_id: StableId | None = None
    expected_return_minute: NonNegativeMinute | None = None
    source_ids: list[StableId] = Field(default_factory=list, max_length=16)

    @model_validator(mode="after")
    def require_action_inputs(self) -> _PopulationPresenceParameters:
        if self.action == "relocate" and self.zone_id is None:
            raise ValueError("relocate requires a destination zone")
        if self.action == "reunite" and self.household_id is None:
            raise ValueError("reunite requires a household ID")
        if self.action != "temporary_exit" and self.expected_return_minute is not None:
            raise ValueError("expected_return_minute is valid only for temporary exits")
        return self


@dataclass(frozen=True)
class PrimitiveSpec:
    """Code-owned definition of one executable operation."""

    op: str
    parameters_model: type[StrictContract]
    target_kinds: frozenset[str]
    units: frozenset[str]
    required_policies: frozenset[str]
    timed: bool
    description: str


class PrimitiveRegistry:
    """Immutable registry; generated `op` values are looked up as data only."""

    def __init__(self, primitives: list[PrimitiveSpec]) -> None:
        by_name = {item.op: item for item in primitives}
        if len(by_name) != len(primitives):
            raise ValueError("primitive operator names must be unique")
        self._by_name = MappingProxyType(by_name)

    def get(self, op: str) -> PrimitiveSpec | None:
        return self._by_name.get(op)

    @property
    def operators(self) -> tuple[PrimitiveSpec, ...]:
        return tuple(self._by_name[name] for name in sorted(self._by_name))


DEFAULT_PRIMITIVE_REGISTRY = PrimitiveRegistry(
    [
        PrimitiveSpec(
            "move_facility",
            MoveFacilityParameters,
            frozenset({"facility"}),
            frozenset({"metre_2d"}),
            frozenset({GEOMETRY_OVERLAY_KEY}),
            False,
            "Permanently move an idle facility within its existing zone; cross-zone transfer "
            "is unsupported.",
        ),
        PrimitiveSpec(
            "transfer_equipment",
            EquipmentTransferParameters,
            frozenset({"equipment"}),
            frozenset({"metre_2d"}),
            frozenset({GEOMETRY_OVERLAY_KEY}),
            False,
            "Permanently transfer an idle positioned facility between connected zones and "
            "move its service points.",
        ),
        PrimitiveSpec(
            "set_partition",
            PartitionParameters,
            frozenset({"zone"}),
            frozenset({"metre_2d"}),
            frozenset({GEOMETRY_OVERLAY_KEY}),
            True,
            "Install a bounded polygon obstacle without narrowing occupied or declared passages.",
        ),
        PrimitiveSpec(
            "set_route_access",
            RouteAccessParameters,
            frozenset({"route"}),
            frozenset({"boolean"}),
            frozenset({GEOMETRY_OVERLAY_KEY}),
            True,
            "Temporarily close a route; active journeys replan and prior unrelated closures "
            "cannot be reopened.",
        ),
        PrimitiveSpec(
            "temporary_room_geometry",
            TemporaryRoomGeometry,
            frozenset({"zone"}),
            frozenset({"metre_2d"}),
            frozenset({GEOMETRY_OVERLAY_KEY}),
            True,
            "Create a bounded physical room polygon with explicit beds and validated route access.",
        ),
        PrimitiveSpec(
            "set_facility_availability",
            _AvailabilityParameters,
            frozenset({"facility"}),
            frozenset({"facility_unit"}),
            frozenset({"facility_capacity"}),
            True,
            "Temporarily cap the number of working units at a facility.",
        ),
        PrimitiveSpec(
            "facility_outage",
            _FacilityOutageParameters,
            frozenset({"facility"}),
            frozenset({"facility_unit"}),
            frozenset({"facility_capacity"}),
            True,
            "Compatibility primitive that removes a bounded number of working units.",
        ),
        PrimitiveSpec(
            "facility_repair",
            FacilityRepairParameters,
            frozenset({"facility"}),
            frozenset({"facility_unit"}),
            frozenset({"facility_capacity", SHARED_OPERATIONS_KEY}),
            False,
            "Restore explicitly identified working units when the observed capacity still "
            "matches the repair precondition.",
        ),
        PrimitiveSpec(
            "staff_handoff",
            StaffHandoffParameters,
            frozenset({"staff"}),
            frozenset({"staff_member"}),
            frozenset({"staff_operations_v1", SHARED_OPERATIONS_KEY}),
            False,
            "Reassign an on-site staff operation to a qualified replacement already at the "
            "work location.",
        ),
        PrimitiveSpec(
            "staff_task_order",
            StaffTaskOrderParameters,
            frozenset({"staff"}),
            frozenset({"staff_task_order"}),
            frozenset({"staff_operations_v1", SHARED_OPERATIONS_KEY}),
            False,
            "Order one idle qualified worker to perform a registered task now, with routed "
            "travel and an applied completion receipt.",
        ),
        PrimitiveSpec(
            "deliver_resource",
            _DeliveryParameters,
            frozenset({"resource"}),
            frozenset({"water_ml", "food_meal_units"}),
            frozenset({"resource_ledger_v1"}),
            False,
            "Deliver a positive quantity to the configured warehouse inventory ledger.",
        ),
        PrimitiveSpec(
            "modify_supply_schedule",
            _SupplyScheduleParameters,
            frozenset({"shipment"}),
            frozenset({"minute"}),
            frozenset({"operational_events_v1"}),
            False,
            "Move a pending, known shipment to a later minute.",
        ),
        PrimitiveSpec(
            "set_staff_availability",
            _StaffAvailabilityParameters,
            frozenset({"staff"}),
            frozenset({"staff_member"}),
            frozenset({"operational_events_v1"}),
            True,
            "Temporarily mark one known staff member unavailable.",
        ),
        PrimitiveSpec(
            "set_staff_role",
            _StaffRoleParameters,
            frozenset({"staff"}),
            frozenset({"staff_role"}),
            frozenset({"staff_operations_v1"}),
            False,
            "Replace the skills of one idle staff member; active work must finish first.",
        ),
        PrimitiveSpec(
            "add_staff",
            _AdditionParameters,
            frozenset({"zone"}),
            frozenset({"staff_member"}),
            frozenset({"staff_operations"}),
            False,
            "Add a bounded number of ordinary support staff to an existing zone.",
        ),
        PrimitiveSpec(
            "add_toilet",
            _ToiletAdditionParameters,
            frozenset({"zone"}),
            frozenset({"toilet_stall"}),
            frozenset({"facility_capacity"}),
            False,
            "Add bounded single-unit toilet facilities to an existing zone.",
        ),
        PrimitiveSpec(
            "add_evacuees",
            _EvacueeArrivalParameters,
            frozenset({"zone"}),
            frozenset({"evacuee"}),
            frozenset({"population_dynamics_v1"}),
            False,
            "Generate synthetic household arrivals and admit them within zone capacity.",
        ),
        PrimitiveSpec(
            "depart_evacuees",
            _EvacueeDepartureParameters,
            frozenset({"zone"}),
            frozenset({"evacuee"}),
            frozenset({"population_dynamics_v1"}),
            False,
            "Remove admitted dynamic residents and defer departure while tasks are active.",
        ),
        PrimitiveSpec(
            "population_presence",
            _PopulationPresenceParameters,
            frozenset({"person"}),
            frozenset({"population_transition"}),
            frozenset({"population_lifecycle_v1"}),
            False,
            "Record a typed same-ID exit, return, transfer, relocation, or household reunion.",
        ),
        PrimitiveSpec(
            "add_temporary_space",
            _TemporarySpaceParameters,
            frozenset({"zone"}),
            frozenset({"person_capacity"}),
            frozenset({"temporary_spaces_v1"}),
            True,
            "Add approved population and sleeping capacity in a zone for a bounded interval; "
            "no unsurveyed floorplan position is created.",
        ),
        PrimitiveSpec(
            "water_outage",
            _WaterOutageParameters,
            frozenset({"facility", "zone"}),
            frozenset({"water_supply"}),
            frozenset({"operational_events_v1", "resource_service_v1"}),
            True,
            "Block new water-service starts at an existing water station or its zone.",
        ),
        PrimitiveSpec(
            "set_ambient_noise",
            AmbientNoiseParameters,
            frozenset({"zone"}),
            frozenset({"normalized_noise_0_1"}),
            frozenset({ZONE_ENVIRONMENT_POLICY}),
            True,
            "Set an experimental zone noise level that reduces rest recovery.",
        ),
        PrimitiveSpec(
            "modify_demand_rate",
            DemandRateParameters,
            frozenset({"zone"}),
            frozenset({"relative_action_rate"}),
            frozenset({ZONE_ENVIRONMENT_POLICY}),
            True,
            (
                "Temporarily multiply one supported action's choice odds and, for rest/sleep, "
                "decision interval."
            ),
        ),
        PrimitiveSpec(
            "set_ventilation",
            VentilationParameters,
            frozenset({"zone"}),
            frozenset({"relative_ventilation_effectiveness_0_1"}),
            frozenset({ZONE_ENVIRONMENT_POLICY}),
            True,
            "Temporarily set relative ventilation effectiveness; it is not an air-change "
            "measurement.",
        ),
        PrimitiveSpec(
            "set_lighting",
            LightingParameters,
            frozenset({"zone"}),
            frozenset({"normalized_lighting_0_1"}),
            frozenset({ZONE_ENVIRONMENT_POLICY}),
            True,
            "Temporarily change normalized lighting and its experimental sleep-recovery effect.",
        ),
        PrimitiveSpec(
            "set_privacy",
            PrivacyParameters,
            frozenset({"zone"}),
            frozenset({"normalized_privacy_0_1"}),
            frozenset({ZONE_ENVIRONMENT_POLICY}),
            True,
            "Temporarily change normalized zone privacy and its discontent feedback.",
        ),
        PrimitiveSpec(
            "set_temperature",
            TemperatureParameters,
            frozenset({"zone"}),
            frozenset({"celsius"}),
            frozenset({ZONE_ENVIRONMENT_POLICY}),
            True,
            "Temporarily set an assumed zone temperature in degrees Celsius.",
        ),
    ]
)

# What native/src/rules/effects.rs asks to validate an event's parameters, by operator. The pydantic
# contracts stay here at the boundary of a typed command.
_PARAMETER_MODELS: Mapping[str, type[BaseModel]] = MappingProxyType(
    {
        "add_temporary_space": _TemporarySpaceParameters,
        "facility_repair": FacilityRepairParameters,
        "modify_demand_rate": DemandRateParameters,
        "set_ambient_noise": AmbientNoiseParameters,
        "set_lighting": LightingParameters,
        "set_privacy": PrivacyParameters,
        "set_staff_role": _StaffRoleParameters,
        "set_temperature": TemperatureParameters,
        "set_ventilation": VentilationParameters,
        "staff_handoff": StaffHandoffParameters,
        "staff_task_order": StaffTaskOrderParameters,
    }
)

EFFECT_PROGRAM_STATE_KEY = "effect_program_v1"
_EFFECT_STATE_FIELDS = {"schema_version", "programs", "active_facility_modifiers"}
_MAX_SCHEDULED_EFFECT_EVENTS = 100
_MAX_EFFECT_DURATION = 60 * 24 * 366


class EffectProgramValidationError(ValueError):
    """Whole-program semantic validation failed before any branch mutation."""

    def __init__(self, issues: list[str]) -> None:
        self.issues = tuple(issues)
        super().__init__("; ".join(issues))


@dataclass(frozen=True)
class ScheduledEffectProgram:
    program_id: str
    program_hash: str
    event_ids: tuple[str, ...]


def _modifier_id(program_id: str, effect_id: str) -> str:
    digest = hashlib.sha256(f"{program_id}:{effect_id}".encode("utf-8")).hexdigest()[:32]
    return f"modifier-{digest}"


def _source_hash(program: EffectProgram) -> str:
    return hashlib.sha256(program.source_text.encode("utf-8")).hexdigest()


def _program_hash(program: EffectProgram) -> str:
    serialized = json.dumps(
        program.model_dump(mode="json"),
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=False,
        allow_nan=False,
    )
    return hashlib.sha256(serialized.encode("utf-8")).hexdigest()


def _projected_facility_units(
    world: WorldState, program: EffectProgram, target: TimedAction
) -> int:
    facility_id = target.target_selector.target_id
    assert facility_id is not None
    current = world.facilities[facility_id].working_units
    active: list[tuple[int, int]] = []
    for effect in sorted(program.effects, key=lambda item: (item.start_minute, item.effect_id)):
        if (
            effect.op not in {"set_facility_availability", "facility_outage"}
            or effect.target_selector.target_id != facility_id
        ):
            continue
        active = [item for item in active if item[0] > effect.start_minute]
        available = min([current, *(item[1] for item in active)])
        parameters = DEFAULT_PRIMITIVE_REGISTRY.get(effect.op)
        assert parameters is not None
        values = parameters.parameters_model.model_validate(effect.parameters)
        requested = (
            values.working_units
            if effect.op == "set_facility_availability"
            else available - values.units
        )
        if effect.effect_id == target.effect_id:
            return requested
        active.append(
            (
                effect.end_minute if effect.end_minute is not None else world.duration_minutes,
                requested,
            )
        )
    raise ValueError(f"facility modifier effect {target.effect_id!r} was not found")


def available_primitives(
    registry: PrimitiveRegistry = DEFAULT_PRIMITIVE_REGISTRY,
) -> list[dict[str, Any]]:
    """Expose only operators that the runtime can actually execute."""
    return [
        {
            "op": item.op,
            "target_kinds": sorted(item.target_kinds),
            "units": sorted(item.units),
            "required_policies": sorted(item.required_policies),
            "timed": item.timed,
            "description": item.description,
            "parameters_schema": item.parameters_model.model_json_schema(),
        }
        for item in registry.operators
    ]


def _effect_candidate(world: WorldState) -> WorldState:
    """The copy an effect changes and checks before it takes the world's place.

    It is `WorldState.from_dict(world.to_dict())` except for the EventLog: the copy holds the
    world's own EventLog instead of a copy made from its documents. At day 4 of a 7-day Run the
    log has some 270,000 records, and turning them into documents and back was nearly all of a
    20 s stall at each effect's start. Every other part goes through the same canonical JSON as
    before, with its key order and checks; the world is validated first, as canonical_json does,
    and the copy is validated with its full history, as from_dict does. Nothing an effect does
    on the copy writes the EventLog (the engine records the event after the handler returns),
    so a refused effect leaves the shared log as it was.
    """
    from sheltersim.core.world import StateInvariantError
    from sheltersim.core.world import WorldState as MutableWorldState

    world.validate()
    document = json.loads(
        MutableWorldState._canonical_text(world._canonical_data(include_event_log=False))
    )
    # The document is new and only this copy holds it, so its containers are not copied again.
    candidate = MutableWorldState.from_dict(document, _validate=False, _borrow=True)
    candidate.event_log = world.event_log
    try:
        candidate.validate()
    except (KeyError, TypeError, ValueError) as exc:
        # As from_dict wraps what its validation raises.
        if isinstance(exc, StateInvariantError):
            raise
        raise StateInvariantError(f"invalid WorldState document: {exc}") from exc
    return candidate


def _toilet_placement_issues(world: WorldState, additions: list[tuple[str, str, int]]) -> list[str]:
    """Check that every added toilet gets a floor-plan point in the current state."""
    if not installed_facility_placement_enabled(world):
        return [
            f"{label} cannot place added toilets on this floor plan; it needs "
            "geometry_overlay schema_version=4 (create the Scenario again)"
            for label, _, _ in additions
        ]

    from sheltersim.domain.models import FacilityState

    candidate = _effect_candidate(world)
    for label, zone_id, count in additions:
        if zone_id not in candidate.zones:
            continue
        for index in range(count):
            facility_id = f"placement-check-{len(candidate.facilities)}-{index}"
            candidate.facilities[facility_id] = FacilityState(
                id=facility_id,
                kind="toilet",
                zone_id=zone_id,
                capacity=1,
                working_units=1,
            )
            point = installed_facility_point(candidate, facility_id)
            if point is None:
                return [
                    f"{label} has no free floor-plan point for toilet {index + 1} "
                    f"in zone {zone_id!r}"
                ]
            candidate.model_state[GEOMETRY_OVERLAY_KEY]["facility_points"][facility_id] = {
                "point_m": point.model_dump(mode="json"),
                "event_id": "placement-check",
                "source_ids": [],
                "minute": candidate.minute_index,
            }
    return []


def validate_effect_program(
    world: WorldState,
    program: EffectProgram,
    *,
    registry: PrimitiveRegistry = DEFAULT_PRIMITIVE_REGISTRY,
) -> tuple[str, ...]:
    """Check every effect and cross-effect conflict without changing `world`."""
    from sheltersim.domain.operational import OPERATIONAL_EVENTS_KEY
    from sheltersim.domain.resources import RESOURCE_LEDGER_KEY

    issues: list[str] = []
    try:
        world.validate()
    except ValueError as exc:
        return (f"world state is invalid: {exc}",)
    if program.support_mode in {SupportMode.CLARIFICATION_REQUIRED, SupportMode.NOT_SIMULATABLE}:
        issues.append(f"program support mode {program.support_mode.value!r} cannot be executed")
    if program.support_mode is SupportMode.APPROXIMATE and not program.assumptions:
        issues.append("approximate programs must include explicit assumptions")
    if program.simulation_start_minute < world.minute_index:
        issues.append("simulation_start_minute is before the current branch minute")
    if program.program_id in world.model_state.get(EFFECT_PROGRAM_STATE_KEY, {}).get(
        "programs", {}
    ):
        issues.append(f"program_id {program.program_id!r} was already scheduled on this branch")
    if EFFECT_PROGRAM_STATE_KEY in world.model_state:
        state = world.model_state[EFFECT_PROGRAM_STATE_KEY]
        if not isinstance(state, dict) or set(state) != _EFFECT_STATE_FIELDS:
            issues.append("effect_program_v1 state has an invalid exact schema")
    expected_event_count = sum(
        2 if effect.end_minute is not None else 1 for effect in program.effects
    )
    if expected_event_count > _MAX_SCHEDULED_EFFECT_EVENTS:
        issues.append("program exceeds the maximum scheduled effect event count")
    if len(world.event_queue.pending()) + expected_event_count > 10_000:
        issues.append("branch event queue exceeds the bounded 10000-event limit")
    if program.simulation_start_minute >= world.duration_minutes:
        issues.append("simulation_start_minute is outside the scenario duration")

    facility_intervals: dict[str, list[tuple[int, int, int, str, bool]]] = {}
    staff_intervals: dict[str, list[tuple[int, int, str]]] = {}
    staff_order_effects: dict[str, list[tuple[int, int]]] = {}
    temporary_capacity_by_zone: dict[str, int] = {}
    geometry_overlay = world.model_state.get(GEOMETRY_OVERLAY_KEY, {})
    temporary_room_ids = set(geometry_overlay.get("temporary_rooms", {}))
    for pending in world.event_queue.pending():
        if pending.event_type != "effect_program_geometry":
            continue
        parameters = pending.payload.get("parameters")
        is_room = pending.payload.get("op") == "temporary_room_geometry"
        if is_room and isinstance(parameters, Mapping):
            room_id = parameters.get("room_id")
            if isinstance(room_id, str):
                temporary_room_ids.add(room_id)
    temporary_state = world.model_state.get(TEMPORARY_SPACES_STATE_KEY, {})
    if isinstance(temporary_state, dict):
        for space in temporary_state.get("spaces", {}).values():
            if space.get("active"):
                zone_id = space["zone_id"]
                temporary_capacity_by_zone[zone_id] = (
                    temporary_capacity_by_zone.get(zone_id, 0) + space["people_capacity"]
                )
    for pending in world.event_queue.pending():
        if pending.event_type == "effect_program_temporary_space":
            pending_zone = pending.payload.get("zone_id")
            pending_params = pending.payload.get("parameters")
            if isinstance(pending_zone, str) and isinstance(pending_params, Mapping):
                try:
                    capacity = _TemporarySpaceParameters.model_validate(
                        pending_params
                    ).people_capacity
                except ValueError:
                    continue
                temporary_capacity_by_zone[pending_zone] = (
                    temporary_capacity_by_zone.get(pending_zone, 0) + capacity
                )
    added_staff = 0
    added_facilities = 0
    added_evacuees = 0
    toilet_additions: list[tuple[str, str, int]] = []
    for effect in program.effects:
        label = f"effect {effect.effect_id!r}"
        primitive = registry.get(effect.op)
        if primitive is None:
            issues.append(f"{label} uses unsupported operator {effect.op!r}")
            continue
        if effect.target_selector.kind not in primitive.target_kinds:
            issues.append(f"{label} target kind must be one of {sorted(primitive.target_kinds)}")
        if effect.units not in primitive.units:
            issues.append(f"{label} has unsupported units {effect.units!r}")
        if set(effect.policy_dependencies) != primitive.required_policies:
            issues.append(
                f"{label} policy_dependencies must be exactly {sorted(primitive.required_policies)}"
            )
        try:
            parameters = primitive.parameters_model.model_validate(effect.parameters)
        except ValueError as exc:
            issues.append(f"{label} parameters are invalid: {exc}")
            continue
        target_id = effect.target_selector.target_id
        if effect.op == "temporary_room_geometry":
            if parameters.room_id in temporary_room_ids:
                issues.append(
                    f"{label} temporary room ID {parameters.room_id!r} already exists or is pending"
                )
            temporary_room_ids.add(parameters.room_id)
        if effect.op == "add_temporary_space" and isinstance(target_id, str):
            temporary_capacity_by_zone[target_id] = (
                temporary_capacity_by_zone.get(target_id, 0) + parameters.people_capacity
            )
        if effect.op == "set_staff_role":
            staff = world.staff.get(target_id or "")
            operations = world.model_state.get("staff_operations_v1", {})
            operation = (
                operations.get("staff", {}).get(target_id) if isinstance(operations, dict) else None
            )
            if staff is None:
                issues.append(f"{label} references unknown staff member {target_id!r}")
            elif staff.active_task_id is not None or (
                isinstance(operation, dict) and operation.get("phase") != "idle"
            ):
                issues.append(f"{label} cannot reassign a staff member with active work")
        elif effect.op == "staff_task_order":
            if effect.start_minute != world.minute_index:
                issues.append(f"{label} staff task orders must start at the current branch minute")
            if target_id is not None:
                try:
                    from sheltersim.domain.staff_operations import (
                        validate_staff_operation_order,
                    )

                    route = validate_staff_operation_order(
                        world,
                        staff_id=target_id,
                        task_kind=parameters.task_kind,
                        target_zone_id=parameters.target_zone_id,
                        target_id=parameters.target_id,
                        minute=effect.start_minute,
                    )
                    duration = world.model_state["staff_operations_v1"]["config"][
                        "task_duration_minutes"
                    ][parameters.task_kind]
                    staff_order_effects.setdefault(target_id, []).append(
                        (
                            effect.start_minute,
                            effect.start_minute + route.travel_minutes + duration,
                        )
                    )
                except ValueError as exc:
                    issues.append(f"{label} {exc}")
        if effect.op in GEOMETRY_OPERATORS:
            try:
                if target_id is None or effect.target_selector.candidate_ids:
                    raise ValueError(
                        "target_missing: geometry operations require an explicit target"
                    )
                validate_geometry_change(
                    world,
                    effect.op,
                    target_id,
                    parameters.model_dump(mode="json"),
                    effect.end_minute,
                )
                if effect.op == "transfer_equipment" and (
                    effect.target_selector.kind != "equipment"
                    or parameters.equipment_id != target_id
                    or parameters.from_zone_id != world.facilities[target_id].zone_id
                ):
                    raise ValueError(
                        "precondition_failed: equipment target and source zone must match "
                        "the selected facility"
                    )
            except ValueError as exc:
                issues.append(f"{label} {exc}")
        if effect.op == "population_presence":
            if "population_presence_v1" not in world.model_state:
                issues.append(
                    f"{label} requires the common population_presence_v1 transition ledger"
                )
            selected_ids = (
                list(effect.target_selector.candidate_ids)
                if parameters.action == "reunite"
                else ([target_id] if target_id is not None else [])
            )
            if (
                effect.target_selector.kind != "person"
                or (
                    parameters.action == "reunite"
                    and (target_id is not None or len(selected_ids) < 2)
                )
                or (
                    parameters.action != "reunite"
                    and (target_id is None or effect.target_selector.candidate_ids)
                )
            ):
                issues.append(f"{label} requires one person ID or at least two reunion candidates")
                continue
            lifecycle = world.model_state.get("population_lifecycle_v1")
            archived = lifecycle.get("people", {}) if isinstance(lifecycle, dict) else {}
            if parameters.action == "return":
                if "population_lifecycle_v1" not in world.model_state:
                    issues.append(f"{label} requires population_lifecycle_v1 state")
                absent = selected_ids[0] not in world.agents and selected_ids[0] in archived
                # The same program may take the person out first (#185).
                leaves_first = selected_ids[0] in world.agents and any(
                    other.op == "population_presence"
                    and other.parameters.get("action") == "temporary_exit"
                    and other.target_selector.target_id == selected_ids[0]
                    and other.start_minute < effect.start_minute
                    for other in program.effects
                )
                if not (absent or leaves_first):
                    issues.append(
                        f"{label} return target must be a retained absent person or one "
                        "this program takes out earlier"
                    )
            elif parameters.action == "reunite":
                if any(person_id not in world.agents for person_id in selected_ids):
                    issues.append(f"{label} reunion candidates must be admitted people")
            elif selected_ids[0] not in world.agents:
                issues.append(f"{label} transition target must be an admitted person")
            if parameters.zone_id is not None and parameters.zone_id not in world.zones:
                issues.append(f"{label} references unknown zone {parameters.zone_id!r}")
            if parameters.action == "relocate" and target_id in world.agents:
                if "facility_geometry" in world.model_configs:
                    from sheltersim.domain.zones import ZoneGraph
                    from sheltersim.policies.space import SpaceRouter

                    route_config = world.model_configs.get("layout_routing", {})
                    graph = ZoneGraph.from_states(
                        world.zones,
                        world.zone_edges,
                        edge_capacities=route_config.get("max_agents_in_transit_by_edge", {}),
                    )
                    route = SpaceRouter(graph).route(
                        world.agents[target_id].zone_id, parameters.zone_id, target_id
                    )
                    if route is None:
                        issues.append(f"{label} has no accessible route to its destination")
        elif target_id is None or effect.target_selector.candidate_ids:
            issues.append(f"{label} requires one explicit target ID")
            continue
        if effect.guard is not None:
            for guard_field in _guard_fields(effect.guard):
                if guard_field.startswith("target.") and effect.target_selector.kind != "facility":
                    issues.append(f"{label} guard field {guard_field!r} requires a facility target")
                elif guard_field == "zone.occupancy" and effect.target_selector.kind not in {
                    "facility",
                    "zone",
                }:
                    issues.append(f"{label} guard field {guard_field!r} needs a zone-scoped target")
                elif (
                    guard_field == "resource.available_quantity" and effect.op != "deliver_resource"
                ):
                    issues.append(
                        f"{label} guard field {guard_field!r} requires a resource delivery target"
                    )
                elif guard_field == "staff.available" and effect.op != "set_staff_availability":
                    issues.append(
                        f"{label} guard field {guard_field!r} requires a staff availability target"
                    )
        if (
            effect.start_minute < world.minute_index
            or effect.start_minute >= world.duration_minutes
        ):
            issues.append(f"{label} start_minute must be within the remaining scenario")
        if effect.end_minute is not None:
            if (
                effect.end_minute <= effect.start_minute
                or effect.end_minute > world.duration_minutes
            ):
                issues.append(f"{label} end_minute is outside its valid time window")
            if effect.end_minute - effect.start_minute > _MAX_EFFECT_DURATION:
                issues.append(f"{label} duration exceeds the 366-day limit")
        elif world.duration_minutes - effect.start_minute > _MAX_EFFECT_DURATION:
            issues.append(f"{label} duration exceeds the 366-day limit")
        elif primitive.timed and effect.reversible:
            # A reversible modifier may last through run end, but a past-the-run
            # restoration cannot be scheduled, so this is still a bounded case.
            pass
        if not primitive.timed and effect.end_minute is not None:
            issues.append(f"{label} is a one-shot operation and cannot have end_minute")

        if "resource_ledger_v1" in primitive.required_policies:
            ledger = world.model_state.get(RESOURCE_LEDGER_KEY)
            if not isinstance(ledger, dict):
                issues.append(f"{label} requires the resource ledger policy")
            elif (
                parameters.warehouse_id != ledger.get("warehouse_id")
                or parameters.resource_id != target_id
                or effect.units != parameters.resource_id
            ):
                issues.append(f"{label} resource, warehouse, and units must match the ledger")
        if "operational_events_v1" in primitive.required_policies:
            if OPERATIONAL_EVENTS_KEY not in world.model_state:
                issues.append(f"{label} requires operational_events_v1 state")
        if "resource_service_v1" in primitive.required_policies:
            if world.schema_version != 6 or "resource_service_v1" not in world.model_state:
                issues.append(f"{label} requires the WorldState v6 resource-service policy")
        if "population_dynamics_v1" in primitive.required_policies:
            if "population_dynamics_v1" not in world.model_state:
                issues.append(f"{label} requires population_dynamics_v1 state")
        if "population_lifecycle_v1" in primitive.required_policies and (
            "population_lifecycle_v1" not in world.model_state
        ):
            issues.append(f"{label} requires population_lifecycle_v1 state")
        if TEMPORARY_SPACES_STATE_KEY in primitive.required_policies and (
            TEMPORARY_SPACES_STATE_KEY not in world.model_state
        ):
            issues.append(
                f"{label} requires simulator_extensions.enabled and temporary_spaces_v1 state"
            )
        if "staff_operations_v1" in primitive.required_policies and (
            "staff_operations_v1" not in world.model_state
        ):
            issues.append(f"{label} requires staff_operations_v1 state")
        if SHARED_OPERATIONS_KEY in primitive.required_policies and (
            SHARED_OPERATIONS_KEY not in world.model_state
        ):
            issues.append(f"{label} requires the common shared_operations_v1 receipt state")
        if ZONE_ENVIRONMENT_POLICY in primitive.required_policies and (
            "zone_environment" not in world.model_configs
            or ZONE_ENVIRONMENT_STATE_KEY not in world.model_state
        ):
            issues.append(f"{label} requires the zone-environment model policy")
        if effect.op in {"set_ventilation", "set_lighting", "set_privacy", "set_temperature"}:
            environment = world.model_configs.get("zone_environment", {})
            try:
                if (
                    ScenarioZoneEnvironmentConfig.model_validate(environment).policy_version
                    != "zone-environment-v2"
                ):
                    issues.append(f"{label} requires zone-environment-v2")
            except ValueError:
                pass
        if (
            effect.op
            in {
                "add_staff",
                "add_toilet",
                "add_evacuees",
                "depart_evacuees",
                "set_ambient_noise",
                "modify_demand_rate",
                "set_ventilation",
                "set_lighting",
                "set_privacy",
                "set_temperature",
                "add_temporary_space",
            }
            and target_id not in world.zones
        ):
            issues.append(f"{label} references unknown zone {target_id!r}")
        if effect.op == "add_temporary_space":
            if effect.end_minute is None:
                issues.append(f"{label} requires end_minute so approved capacity expires")
            elif effect.end_minute >= world.duration_minutes:
                issues.append(f"{label} expiry must occur before the scenario end")
        if effect.op == "add_evacuees":
            unknown_admission_zones = set(parameters.admission_zone_ids) - set(world.zones)
            if target_id in parameters.admission_zone_ids or unknown_admission_zones:
                issues.append(
                    f"{label} alternate admission zones must be known and exclude its "
                    "preferred zone"
                )
            overlay_config = world.model_configs.get("geometry_overlay", {})
            if parameters.admission_zone_ids and (
                not isinstance(overlay_config, dict)
                or overlay_config.get("schema_version") not in {3, 4}
            ):
                issues.append(
                    f"{label} alternate physical room admission requires geometry_overlay "
                    "schema_version=3 or 4"
                )
            if parameters.approach_route_id is not None:
                if parameters.admission_zone_ids:
                    issues.append(
                        f"{label} cannot combine one approach route with alternate admission zones"
                    )
                try:
                    geometry_raw = world.model_configs.get("facility_geometry")
                    if not isinstance(geometry_raw, dict):
                        raise ValueError("arrival approach requires approved physical Geometry")
                    geometry = parsed_facility_geometry(geometry_raw)
                    route = next(
                        (
                            item
                            for item in geometry.arrival_approach_routes
                            if item.route_id == parameters.approach_route_id
                        ),
                        None,
                    )
                    if route is None:
                        raise ValueError(
                            f"unknown arrival approach route {parameters.approach_route_id!r}"
                        )
                    if route.destination_zone_id != target_id:
                        raise ValueError(
                            "arrival approach destination must match the selected zone"
                        )
                except ValueError as exc:
                    issues.append(f"{label} arrival approach is invalid: {exc}")
        if effect.op in {
            "set_ambient_noise",
            "modify_demand_rate",
            "set_ventilation",
            "set_lighting",
            "set_privacy",
            "set_temperature",
        }:
            if effect.end_minute is None:
                issues.append(f"{label} requires end_minute so its modifier can expire")
            elif effect.end_minute >= world.duration_minutes:
                issues.append(f"{label} end_minute must be before the scenario end")
            if (
                effect.op == "modify_demand_rate"
                and parameters.action in {"fetch_water", "collect_meal"}
                and "resource_service_v1" not in world.model_state
            ):
                issues.append(f"{label} action requires the resource-service policy")
        if effect.op == "add_staff":
            added_staff += parameters.count
        if effect.op == "add_toilet":
            added_facilities += parameters.count
            toilet_additions.append((label, target_id, parameters.count))
        if effect.op == "add_evacuees":
            added_evacuees += parameters.count

        if effect.op in {"set_facility_availability", "facility_outage"}:
            facility = world.facilities.get(target_id)
            if facility is None:
                issues.append(f"{label} references unknown facility {target_id!r}")
                continue
            if effect.op == "set_facility_availability":
                requested_units = parameters.working_units
                is_delta = False
            else:
                requested_units = parameters.units
                is_delta = True
            active_occupancy = len(facility.active_agent_ids)
            if world.schema_version == 6:
                active_occupancy += len(facility.resource_active_agent_ids)
            if not is_delta and (requested_units < 0 or requested_units < active_occupancy):
                issues.append(f"{label} would set availability below active occupancy")
            if not is_delta and requested_units > facility.working_units:
                issues.append(f"{label} may only reduce or preserve current working availability")
            facility_intervals.setdefault(target_id, []).append(
                (
                    effect.start_minute,
                    effect.end_minute if effect.end_minute is not None else world.duration_minutes,
                    requested_units,
                    effect.effect_id,
                    is_delta,
                )
            )
        elif effect.op == "facility_repair":
            facility = world.facilities.get(target_id)
            if facility is None:
                issues.append(f"{label} references unknown facility {target_id!r}")
                continue
            if parameters.facility_id != target_id:
                issues.append(f"{label} repair facility_id must match its selected facility")
            if parameters.expected_working_units != facility.working_units:
                issues.append(
                    f"{label} expected_working_units must match the current facility state"
                )
            if not (
                facility.working_units < parameters.restored_working_units <= facility.capacity
            ):
                issues.append(
                    f"{label} restored_working_units must increase capacity without "
                    "exceeding installed units"
                )
            if active_facility_outage(world, target_id) or has_active_facility_modifier(
                world, target_id
            ):
                issues.append(f"{label} cannot repair a facility with an active outage or modifier")
        elif effect.op == "staff_handoff":
            if SHARED_OPERATIONS_KEY not in world.model_state:
                issues.append(f"{label} requires the common staff handoff receipt state")
            operations = world.model_state.get("staff_operations_v1", {})
            staff_records = operations.get("staff", {}) if isinstance(operations, dict) else {}
            record = staff_records.get(target_id) if isinstance(staff_records, dict) else None
            task = world.active_tasks.get(parameters.task_id)
            replacement_id = parameters.replacement_staff_id
            replacement = world.staff.get(replacement_id or "")
            if parameters.staff_id != target_id:
                issues.append(f"{label} handoff staff_id must match its selected staff member")
            if target_id not in world.staff or task is None or task.kind != "staff_operation":
                issues.append(f"{label} must reference an active staff operation task")
            elif (
                task.staff_id != target_id
                or not isinstance(record, dict)
                or record.get("phase") != "working"
                or record.get("task_id") != parameters.task_id
                or task.due_minute <= world.minute_index
            ):
                issues.append(
                    f"{label} can only hand off active on-site work before its completion"
                )
            if replacement is None:
                issues.append(f"{label} replacement_staff_id must identify a known staff member")
            elif replacement.active_task_id is not None or replacement.zone_id != (
                record.get("target_zone_id") if isinstance(record, dict) else None
            ):
                issues.append(f"{label} replacement must be idle and already at the work location")
            elif not _staff_has_task_skill(
                replacement.skills, record.get("task_kind") if isinstance(record, dict) else None
            ):
                issues.append(f"{label} replacement does not have the task skill")
            else:
                from sheltersim.domain.operational import staff_is_available
                from sheltersim.domain.staff_dynamics import staff_dynamics_is_available

                if not staff_is_available(world, replacement.id) or not staff_dynamics_is_available(
                    world, replacement.id, world.minute_index
                ):
                    issues.append(f"{label} replacement is not currently available for work")
            if replacement_id is None:
                issues.append(f"{label} requires replacement_staff_id for executable reassignment")
        elif effect.op == "modify_supply_schedule":
            state = world.model_state.get(OPERATIONAL_EVENTS_KEY, {})
            record = state.get("shipments", {}).get(parameters.shipment_id)
            pending = (
                next(
                    (
                        event
                        for event in world.event_queue.pending()
                        if event.event_id == record.get("event_id")
                    ),
                    None,
                )
                if isinstance(record, dict)
                else None
            )
            if (
                target_id != parameters.shipment_id
                or not isinstance(record, dict)
                or pending is None
                or pending.event_type != "resource_delivery"
            ):
                issues.append(f"{label} must reference a known pending shipment")
            elif (
                parameters.to_minute <= pending.due_minute
                or parameters.to_minute <= effect.start_minute
                or parameters.to_minute >= world.duration_minutes
            ):
                issues.append(f"{label} must delay the shipment to a later in-run minute")
        elif effect.op == "set_staff_availability":
            state = world.model_state.get(OPERATIONAL_EVENTS_KEY, {})
            status = state.get("staff_status", {}).get(target_id)
            if target_id not in world.staff or not isinstance(status, dict):
                issues.append(f"{label} references unknown staff or missing availability policy")
            elif not status.get("available", False):
                issues.append(f"{label} staff member is already unavailable")
            elif effect.end_minute is None:
                issues.append(f"{label} requires end_minute to restore staff availability")
            else:
                staff_intervals.setdefault(target_id, []).append(
                    (effect.start_minute, effect.end_minute, effect.effect_id)
                )
        elif effect.op == "water_outage":
            if effect.end_minute is None:
                issues.append(f"{label} requires end_minute so water service can be restored")
            if effect.target_selector.kind == "facility":
                facility = world.facilities.get(target_id)
                if facility is None or facility.kind != "water_station":
                    issues.append(f"{label} facility must be an existing water_station")
            elif target_id not in world.zones or not any(
                facility.kind == "water_station" and facility.zone_id == target_id
                for facility in world.facilities.values()
            ):
                issues.append(f"{label} zone must contain an existing water_station")

    for facility_id, intervals in facility_intervals.items():
        intervals.sort(key=lambda item: (item[0], item[3]))
        facility = world.facilities[facility_id]
        active: list[tuple[int, int, int, str]] = []
        for start, end, requested_units, effect_id, is_delta in intervals:
            active = [entry for entry in active if entry[1] > start]
            current_units = min([facility.working_units, *(entry[2] for entry in active)])
            target_units = current_units - requested_units if is_delta else requested_units
            occupancy = len(facility.active_agent_ids)
            if world.schema_version == 6:
                occupancy += len(facility.resource_active_agent_ids)
            if target_units < occupancy:
                issues.append(f"facility modifier {effect_id!r} would undercut active occupancy")
            if target_units > current_units:
                issues.append(
                    f"facility modifier {effect_id!r} conflicts with the restrictive stacking rule"
                )
            active.append((start, end, target_units, effect_id))
        for event in world.event_queue.pending():
            if (
                event.event_type
                in {
                    "facility_outage",
                    "facility_recovery",
                    "effect_program_apply",
                    "effect_program_expire",
                }
                and event.payload.get("facility_id") == facility_id
                and any(start <= event.due_minute < end for start, end, *_ in intervals)
            ):
                issues.append(
                    f"pending {event.event_type} conflicts with a new "
                    "EffectProgram facility modifier"
                )
    for staff_id, intervals in staff_intervals.items():
        intervals.sort()
        for previous, current in zip(intervals, intervals[1:], strict=False):
            if current[0] < previous[1]:
                issues.append(f"staff availability effects overlap for {staff_id!r}")
        for event in world.event_queue.pending():
            if (
                event.event_type == "staff_unavailable"
                and event.payload.get("staff_id") == staff_id
            ):
                for start, end, _ in intervals:
                    pending_end = event.payload.get("until_minute", world.duration_minutes)
                    if start < pending_end and event.due_minute < end:
                        issues.append(
                            "staff availability effect conflicts with pending absence "
                            f"for {staff_id!r}"
                        )
                        break
    for staff_id, intervals in staff_order_effects.items():
        if len(intervals) > 1:
            issues.append(f"multiple staff task orders target {staff_id!r} at one branch minute")
        for effect in program.effects:
            if effect.target_selector.target_id != staff_id:
                continue
            if effect.op in {"set_staff_role", "staff_handoff"} and any(
                start <= effect.start_minute < end for start, end in intervals
            ):
                issues.append(f"staff task order conflicts with {effect.op!r} for {staff_id!r}")
            if effect.op == "set_staff_availability" and effect.end_minute is not None:
                if any(effect.start_minute <= start < effect.end_minute for start, _ in intervals):
                    issues.append(
                        f"staff task order conflicts with availability change for {staff_id!r}"
                    )
    environment_config = world.model_configs.get("zone_environment")
    if environment_config is not None:
        max_per_zone = environment_config["max_active_modifiers_per_zone"]
        environment_intervals: dict[str, list[tuple[int, int, str]]] = {}
        for modifier_id, record in world.model_state[ZONE_ENVIRONMENT_STATE_KEY][
            "modifiers"
        ].items():
            environment_intervals.setdefault(record["zone_id"], []).append(
                (world.minute_index, record["end_minute"], modifier_id)
            )
        active_modifier_ids = set(world.model_state[ZONE_ENVIRONMENT_STATE_KEY]["modifiers"])
        for event in world.event_queue.pending():
            modifier_id = event.payload.get("modifier_id")
            zone_id = event.payload.get("zone_id")
            end_minute = event.payload.get("end_minute")
            if (
                event.event_type == "effect_program_environment"
                and isinstance(modifier_id, str)
                and modifier_id not in active_modifier_ids
                and isinstance(zone_id, str)
                and zone_id in world.zones
                and isinstance(end_minute, int)
                and not isinstance(end_minute, bool)
                and event.due_minute < end_minute < world.duration_minutes
            ):
                environment_intervals.setdefault(zone_id, []).append(
                    (event.due_minute, end_minute, modifier_id)
                )
        for effect in program.effects:
            if effect.op in {
                "set_ambient_noise",
                "modify_demand_rate",
                "set_ventilation",
                "set_lighting",
                "set_privacy",
                "set_temperature",
            }:
                if effect.end_minute is None or effect.target_selector.target_id is None:
                    continue
                environment_intervals.setdefault(effect.target_selector.target_id, []).append(
                    (
                        effect.start_minute,
                        effect.end_minute,
                        f"{program.program_id}:{effect.effect_id}",
                    )
                )
        for zone_id, intervals in environment_intervals.items():
            boundaries = sorted({minute for start, end, _ in intervals for minute in (start, end)})
            if any(
                sum(start <= minute < end for start, end, _ in intervals) > max_per_zone
                for minute in boundaries[:-1]
            ):
                issues.append(f"zone {zone_id!r} exceeds the active environment modifier limit")
    pending_staff_additions = sum(
        event.payload.get("count", 0)
        for event in world.event_queue.pending()
        if event.event_type == "effect_program_structure" and event.payload.get("op") == "add_staff"
    )
    pending_facility_additions = sum(
        event.payload.get("count", 0)
        for event in world.event_queue.pending()
        if event.event_type == "effect_program_structure"
        and event.payload.get("op") == "add_toilet"
    )
    if len(world.staff) + pending_staff_additions + added_staff > 100:
        issues.append("program exceeds the 100-staff runtime bound")
    if any(capacity > 1000 for capacity in temporary_capacity_by_zone.values()):
        issues.append("temporary person capacity exceeds the 1000-person per-zone bound")
    if len(world.facilities) + pending_facility_additions + added_facilities > 200:
        issues.append("program exceeds the 200-facility runtime bound")
    if toilet_additions and "facility_geometry" in world.model_configs:
        issues.extend(_toilet_placement_issues(world, toilet_additions))
    population_state = world.model_state.get("population_dynamics_v1")
    if isinstance(population_state, dict):
        known_population = set(population_state.get("initial_agent_ids", ())) | set(
            population_state.get("profile_traits", {})
        )
        pending_arrival_count = sum(
            event.payload.get("count", 0)
            for event in world.event_queue.pending()
            if event.event_type == "effect_program_population"
            and event.payload.get("op") == "add_evacuees"
        )
        if len(known_population) + pending_arrival_count + added_evacuees > 10_000:
            issues.append("program exceeds the 10000-profile population runtime bound")
    return tuple(dict.fromkeys(issues))


def derived_effect_marker(marker: Mapping[str, Any]) -> dict[str, Any]:
    """The marker of an event that a program's event schedules: phase `derived`, no guard.

    A guard decides whether its start event applies. What that event then schedules (a staff
    member's return, a delayed delivery, an arrival's approach) ends or continues what started,
    as an expiry does, so the guard is not copied and not evaluated again (#89).
    """
    derived = {key: value for key, value in marker.items() if key != "guard"}
    derived["phase"] = "derived"
    return derived


def schedule_effect_program(
    world: WorldState,
    program: EffectProgram,
    *,
    registry: PrimitiveRegistry = DEFAULT_PRIMITIVE_REGISTRY,
) -> ScheduledEffectProgram:
    """Atomically add a validated program and all its start/expiry events."""
    from sheltersim.domain.operational import OPERATIONAL_EVENTS_KEY

    issues = list(validate_effect_program(world, program, registry=registry))
    if issues:
        raise EffectProgramValidationError(issues)
    candidate = _effect_candidate(world)
    state = candidate.model_state.setdefault(
        EFFECT_PROGRAM_STATE_KEY,
        {"schema_version": 1, "programs": {}, "active_facility_modifiers": {}},
    )
    program_hash = _program_hash(program)
    source_text_hash = _source_hash(program)
    effect_ids = [effect.effect_id for effect in program.effects]
    program_record = {
        "program_hash": program_hash,
        "source_text_hash": source_text_hash,
        "narrative": program.narrative,
        "compiler_provenance": program.compiler_provenance.model_dump(mode="json"),
        "assumptions": [item.model_dump(mode="json") for item in program.assumptions],
        "support_mode": program.support_mode.value,
        "effect_ids": effect_ids,
        "operators": {effect.effect_id: effect.op for effect in program.effects},
        "scheduled_event_ids": [],
    }
    state["programs"][program.program_id] = program_record
    program_record = state["programs"][program.program_id]
    event_ids: list[str] = []

    def make_event(
        effect: TimedAction,
        *,
        due_minute: int,
        event_type: str,
        payload: dict[str, Any],
        phase: str,
        priority: int = 0,
        kind: EventKind = EventKind.INTERVENTION,
        causation_id: str | None = None,
    ) -> ScheduledEvent:
        sequence = candidate.allocate_event_sequence()
        event_id = f"effect-event-{sequence}"
        marker = {
            "program_id": program.program_id,
            "effect_id": effect.effect_id,
            "op": effect.op,
            "source_text_hash": source_text_hash,
            "compiler_provenance": program.compiler_provenance.model_dump(mode="json"),
            "assumption_ids": list(effect.assumption_ids),
            "policy_dependencies": list(effect.policy_dependencies),
            "units": effect.units,
            "phase": phase,
        }
        if phase == "start" and effect.guard is not None:
            marker["guard"] = effect.guard.model_dump(mode="json")
        event = ScheduledEvent(
            event_id=event_id,
            kind=kind,
            due_minute=due_minute,
            priority=priority,
            sequence=sequence,
            event_type=event_type,
            payload=payload,
            source_id=effect.source_id,
            causation_id=causation_id or effect.causation_id,
            provenance={"effect_program_v1": marker},
        )
        candidate.schedule_event(event)
        program_record["scheduled_event_ids"].append(event_id)
        event_ids.append(event_id)
        return event

    for effect in sorted(program.effects, key=lambda item: (item.start_minute, item.effect_id)):
        primitive = registry.get(effect.op)
        assert primitive is not None
        parameters = primitive.parameters_model.model_validate(effect.parameters).model_dump(
            mode="json"
        )
        target_id = effect.target_selector.target_id
        if effect.op in GEOMETRY_OPERATORS:
            modifier_id = _modifier_id(program.program_id, effect.effect_id)
            payload = {
                "program_id": program.program_id,
                "effect_id": effect.effect_id,
                "modifier_id": modifier_id,
                "target_id": target_id,
                "op": effect.op,
                "parameters": parameters,
                "end_minute": effect.end_minute,
            }
            start_event = make_event(
                effect,
                due_minute=effect.start_minute,
                event_type="effect_program_geometry",
                payload=payload,
                phase="start",
            )
            if effect.end_minute is not None:
                make_event(
                    effect,
                    due_minute=effect.end_minute,
                    event_type="effect_program_geometry_expire",
                    payload=payload,
                    phase="expiry",
                    priority=20,
                    causation_id=start_event.event_id,
                )
            continue
        if effect.op == "population_presence":
            command = {
                **parameters,
                "source_ids": sorted(set(parameters["source_ids"] + effect.assumption_ids)),
            }
            if target_id is not None:
                command["agent_id"] = target_id
            if effect.target_selector.candidate_ids:
                command["agent_ids"] = effect.target_selector.candidate_ids
            make_event(
                effect,
                due_minute=effect.start_minute,
                event_type="effect_program_population",
                payload={
                    "program_id": program.program_id,
                    "effect_id": effect.effect_id,
                    "op": effect.op,
                    "command": command,
                },
                phase="start",
            )
            continue
        assert target_id is not None
        if effect.op in {"set_facility_availability", "facility_outage"}:
            working_units = _projected_facility_units(candidate, program, effect)
            payload = {
                "program_id": program.program_id,
                "effect_id": effect.effect_id,
                "modifier_id": _modifier_id(program.program_id, effect.effect_id),
                "facility_id": target_id,
                "working_units": working_units,
            }
            start_event = make_event(
                effect,
                due_minute=effect.start_minute,
                event_type="effect_program_apply",
                payload=payload,
                phase="start",
            )
            if effect.end_minute is not None:
                make_event(
                    effect,
                    due_minute=effect.end_minute,
                    event_type="effect_program_expire",
                    payload={
                        "program_id": program.program_id,
                        "effect_id": effect.effect_id,
                        "modifier_id": payload["modifier_id"],
                        "facility_id": target_id,
                    },
                    phase="expiry",
                    priority=20,
                    causation_id=start_event.event_id,
                )
        elif effect.op == "facility_repair":
            make_event(
                effect,
                due_minute=effect.start_minute,
                event_type="effect_program_structure",
                payload={
                    "program_id": program.program_id,
                    "effect_id": effect.effect_id,
                    "modifier_id": _modifier_id(program.program_id, effect.effect_id),
                    "op": effect.op,
                    "facility_id": target_id,
                    "parameters": parameters,
                },
                phase="start",
            )
        elif effect.op == "staff_handoff":
            make_event(
                effect,
                due_minute=effect.start_minute,
                event_type="effect_program_structure",
                payload={
                    "program_id": program.program_id,
                    "effect_id": effect.effect_id,
                    "modifier_id": _modifier_id(program.program_id, effect.effect_id),
                    "op": effect.op,
                    "staff_id": target_id,
                    "parameters": parameters,
                },
                phase="start",
            )
        elif effect.op == "staff_task_order":
            make_event(
                effect,
                due_minute=effect.start_minute,
                event_type="effect_program_structure",
                payload={
                    "program_id": program.program_id,
                    "effect_id": effect.effect_id,
                    "modifier_id": _modifier_id(program.program_id, effect.effect_id),
                    "op": effect.op,
                    "staff_id": target_id,
                    "parameters": parameters,
                },
                phase="start",
            )
        elif effect.op == "deliver_resource":
            make_event(
                effect,
                due_minute=effect.start_minute,
                event_type="resource_delivery",
                payload=parameters,
                phase="start",
            )
        elif effect.op == "modify_supply_schedule":
            make_event(
                effect,
                due_minute=effect.start_minute,
                event_type="delivery_delayed",
                payload=parameters,
                phase="start",
            )
        elif effect.op == "set_staff_availability":
            make_event(
                effect,
                due_minute=effect.start_minute,
                event_type="staff_unavailable",
                payload={"staff_id": target_id, "until_minute": effect.end_minute},
                phase="start",
            )
        elif effect.op == "set_staff_role":
            make_event(
                effect,
                due_minute=effect.start_minute,
                event_type="effect_program_structure",
                payload={
                    "program_id": program.program_id,
                    "effect_id": effect.effect_id,
                    "modifier_id": _modifier_id(program.program_id, effect.effect_id),
                    "op": effect.op,
                    "staff_id": target_id,
                    "skills": parameters["skills"],
                },
                phase="start",
            )
        elif effect.op == "add_temporary_space":
            modifier_id = _modifier_id(program.program_id, effect.effect_id)
            payload = {
                "program_id": program.program_id,
                "effect_id": effect.effect_id,
                "modifier_id": modifier_id,
                "op": effect.op,
                "zone_id": target_id,
                "parameters": parameters,
                "end_minute": effect.end_minute,
            }
            start_event = make_event(
                effect,
                due_minute=effect.start_minute,
                event_type="effect_program_temporary_space",
                payload=payload,
                phase="start",
            )
            assert effect.end_minute is not None
            make_event(
                effect,
                due_minute=effect.end_minute,
                event_type="effect_program_temporary_space_expire",
                payload={
                    "program_id": program.program_id,
                    "effect_id": effect.effect_id,
                    "modifier_id": modifier_id,
                    "zone_id": target_id,
                },
                phase="expiry",
                priority=20,
                causation_id=start_event.event_id,
            )
        elif effect.op == "water_outage":
            outage_digest = hashlib.sha256(
                f"{program.program_id}:{effect.effect_id}:water".encode("utf-8")
            ).hexdigest()[:32]
            outage_id = f"outage-{outage_digest}"
            outage_payload = {"outage_id": outage_id}
            outage_payload[
                "zone_id" if effect.target_selector.kind == "zone" else "facility_id"
            ] = target_id
            start_event = make_event(
                effect,
                due_minute=effect.start_minute,
                event_type="water_outage",
                payload=outage_payload,
                phase="start",
            )
            restore_id: str | None = None
            if effect.end_minute is not None and effect.end_minute < candidate.duration_minutes:
                restore = make_event(
                    effect,
                    due_minute=effect.end_minute,
                    event_type="water_restored",
                    payload={"outage_id": outage_id},
                    phase="expiry",
                    priority=10,
                    causation_id=start_event.event_id,
                )
                restore_id = restore.event_id
            operational = candidate.model_state[OPERATIONAL_EVENTS_KEY]
            if outage_id in operational["water_outages"]:
                raise EffectProgramValidationError([f"water outage ID collision for {outage_id!r}"])
            operational["water_outages"][outage_id] = {
                "target_kind": "zone" if "zone_id" in outage_payload else "facility",
                "target_id": target_id,
                "outage_event_id": start_event.event_id,
                "restore_event_id": restore_id,
                "active": False,
            }
        elif effect.op in {"add_staff", "add_toilet"}:
            payload = {
                "program_id": program.program_id,
                "effect_id": effect.effect_id,
                "modifier_id": _modifier_id(program.program_id, effect.effect_id),
                "op": effect.op,
                "zone_id": target_id,
                **parameters,
            }
            make_event(
                effect,
                due_minute=effect.start_minute,
                event_type="effect_program_structure",
                payload=payload,
                phase="start",
            )
        elif effect.op in {"add_evacuees", "depart_evacuees"}:
            make_event(
                effect,
                due_minute=effect.start_minute,
                event_type="effect_program_population",
                payload={
                    "program_id": program.program_id,
                    "effect_id": effect.effect_id,
                    "op": effect.op,
                    "zone_id": target_id,
                    **parameters,
                },
                phase="start",
            )
        elif effect.op in {
            "set_ambient_noise",
            "modify_demand_rate",
            "set_ventilation",
            "set_lighting",
            "set_privacy",
            "set_temperature",
        }:
            modifier_id = _modifier_id(program.program_id, effect.effect_id)
            start_event = make_event(
                effect,
                due_minute=effect.start_minute,
                event_type="effect_program_environment",
                payload={
                    "program_id": program.program_id,
                    "effect_id": effect.effect_id,
                    "modifier_id": modifier_id,
                    "zone_id": target_id,
                    "op": effect.op,
                    "parameters": parameters,
                    "end_minute": effect.end_minute,
                },
                phase="start",
            )
            assert effect.end_minute is not None
            make_event(
                effect,
                due_minute=effect.end_minute,
                event_type="effect_program_environment_expire",
                payload={
                    "program_id": program.program_id,
                    "effect_id": effect.effect_id,
                    "modifier_id": modifier_id,
                    "zone_id": target_id,
                },
                phase="expiry",
                priority=20,
                causation_id=start_event.event_id,
            )

    candidate.validate()
    world.__dict__.clear()
    world.__dict__.update(candidate.__dict__)
    return ScheduledEffectProgram(program.program_id, program_hash, tuple(event_ids))


@kernel_only("effects", "should_apply_guard")
def should_apply_effect_guard(world: WorldState, event: ScheduledEvent) -> bool:
    """Evaluate a compiled guard against fixed state fields; no dynamic evaluation.

    True for an event without an EffectProgram marker or guard. A guard the GuardExpression contract
    turns down is an EventApplicationError ("EffectProgram guard is invalid: ..."); "and" and "or"
    stop at the first answer. A leaf reads simulation.minute, the target facility's capacity,
    working_units or queue length, the zone's occupancy, the resource's available quantity or the
    staff member's availability; the target is the payload's first str of facility_id, staff_id,
    zone_id, resource_id, target_id (a single-effect program's shipment_id otherwise), and a missing
    one is an EventApplicationError of its field. Numbers compare by value (a count equals 0.0,
    #196), exactly at any size; a bool never equals a number; an ordered comparison of a bool is
    false.
    """


def _guard_fields(guard: GuardExpression) -> set[str]:
    if guard.field is not None:
        return {guard.field}
    return set().union(*(_guard_fields(item) for item in guard.operands))


@kernel_only("effects", "has_active_modifier")
def has_active_facility_modifier(world: WorldState, facility_id: str) -> bool:
    """Whether effect_program_v1 holds active availability modifiers for the facility."""


@kernel_only("effects", "has_pending_modifier")
def has_pending_facility_modifier(world: WorldState, facility_id: str) -> bool:
    """Whether a pending effect_program_apply or _expire event targets the facility."""


class EffectProgramGeometryHandler:
    """Apply approved geometry effects transactionally with runtime revalidation."""

    def __init__(self, *, expire: bool = False) -> None:
        self.expire = expire

    @kernel_only("effects", "geometry")
    def apply(self, world: WorldState, event: ScheduledEvent) -> None:
        """Apply or expire an approved geometry effect through `apply_geometry_change`, on a copy.

        EventApplicationError when the payload fields or the operator are not a geometry effect's,
        or the effect and modifier ID are not its approved program's; then the guard
        (`effect_event_guard_or_suppress`). An expiry of a partition, route modifier or temporary
        room that is not active (a room: not this start's or end) is EventSuppressed. The
        ValueError, KeyError or TypeError of `apply_geometry_change` or of the copy's validation is
        the EventApplicationError's text.
        """


class EffectProgramTemporarySpaceHandler:
    """Apply and expire typed temporary population/bed capacity transactionally."""

    def __init__(self, *, expire: bool = False) -> None:
        self.expire = expire

    @kernel_only("effects", "temporary_space")
    def apply(self, world: WorldState, event: ScheduledEvent) -> None:
        """Start or expire an approved temporary space, on a copy that takes the world's place once
        it validates.

        EventApplicationError "temporary-space event does not reference an approved installed
        effect" without temporary_spaces_v1, an approved add_temporary_space effect of the program
        in effect_program_v1 with its modifier ID, a marker that lists temporary_spaces_v1, or a
        known zone. An expiry needs exactly program_id, effect_id, modifier_id and zone_id; a space
        that is not active, or not this start's with this end minute, is EventSuppressed, as is one
        `expire_temporary_space` finds already expired. A start needs its payload fields and op
        (and the program's operator), then the guard ("EffectProgram guard evaluated false"),
        parameters that validate ("temporary-space parameters are invalid: ..."), and an int
        end_minute after the start and before the Run's end. It registers the space (zone-only
        capacity, its people and sleeping capacity, the event's source and the marker's
        assumptions as sources) with `register_temporary_space`, whose ValueError is an
        EventApplicationError with its text.
        """


class EffectProgramFacilityHandler:
    """Apply and expire bounded facility availability modifiers."""

    def __init__(self, *, expire: bool = False) -> None:
        self.expire = expire

    @kernel_only("effects", "facility")
    def apply(self, world: WorldState, event: ScheduledEvent) -> None:
        """Start or expire a facility availability modifier, on a copy that takes the world's place
        once it validates (the validation's error is raised as it is, the world unchanged).

        EventApplicationError when the payload fields are not the start's (with working_units) or
        the expiry's ("effect program apply/expire payload fields do not match"), when the effect
        is not one of its program's, for an unknown facility, and under an active infrastructure
        outage of it. A start needs an int working_units of at least the occupancy (and resource
        occupancy in WorldState v6), at most the facility's working units and capacity; the first
        modifier captures the baseline working units, a modifier already active is EventSuppressed,
        and the facility works the lowest of its baseline and its modifiers. An expiry of a modifier
        not active is EventSuppressed; the facility returns to the lowest of the baseline and the
        others, unless that is below its occupancy (EventApplicationError), and the stack goes with
        its last modifier.
        """


class EffectProgramStructureHandler:
    """Apply typed structural primitives using existing WorldState domain records."""

    @kernel_only("effects", "structure")
    def apply(self, world: WorldState, event: ScheduledEvent) -> tuple[Any, ...] | None:
        """Apply a structural primitive of an approved program, by its payload's fields.

        - staff_task_order: a current start of the approved program for a str staff member, due
          now; valid StaffTaskOrderParameters; the guard; then `order_staff_operation` on a copy.
        - staff_handoff: an approved start with valid StaffHandoffParameters for known staff, a
          known task and shared_operations_v1; the guard; then, on a copy, the working operation
          and its one pending completion go to a qualified, idle, available replacement at the
          work, the previous member is idle. Returns the handoff receipt.
        - facility_repair: an approved start with valid FacilityRepairParameters for a known
          facility and shared_operations_v1; the guard; the facility's working units must still be
          the expected ones, below the restored ones and with no outage or modifier. The facility
          (and the infrastructure baseline) gets the restored units in place. Returns the facility
          recovery receipt.
        - set_staff_role: valid skills for known staff with no active work, set sorted on a copy.
        - add_staff and add_toilet: a positive int count for a known zone, on a copy, within 100
          staff and 200 facilities: new support staff "effect-staff-<modifier>-<n>" with their
          operational, dynamics and operations records (and staff positions with movement_v1), or
          new one-place toilets "effect-toilet-<modifier>-<n>", placed on the floor plan when
          installed facility placement is on (its ValueError is an EventApplicationError with its
          text) and announced.

        Each mismatch is an EventApplicationError with its own text, the parameters' ValueError or
        TypeError with theirs; a guard that holds false is EventSuppressed. A copy takes the world's
        place once it validates.
        """


class EffectProgramEnvironmentHandler:
    """Start or expire a typed, zone-scoped environment modifier."""

    def __init__(self, *, expire: bool = False) -> None:
        self.expire = expire

    @kernel_only("effects", "environment")
    def apply(self, world: WorldState, event: ScheduledEvent) -> None:
        """Start or expire a zone environment modifier of an approved program, on a copy.

        EventApplicationError, in this order: payload fields, IDs that are not str, no installed
        program policy, a modifier ID that is not its effect's; for an expiry, a record that doesn't
        match its start (EventSuppressed when none is active); for a start, an unknown operator or
        one that is not the program's, an unknown zone, invalid parameters, a demand action without
        the resource service, an expiry minute out of the Run, no unique expiry event, a condition
        operator before zone-environment-v2, invalid assumption provenance (EventSuppressed when it
        is already active). A ValueError of the copy's validation is "zone environment modifier is
        invalid: ...".
        """


# The event type of an operator's event, by the phase in its marker.
EFFECT_EVENT_TYPES: Mapping[str, Mapping[str, str]] = MappingProxyType(
    {
        "start": {
            "set_facility_availability": "effect_program_apply",
            "facility_outage": "effect_program_apply",
            "facility_repair": "effect_program_structure",
            "staff_handoff": "effect_program_structure",
            "staff_task_order": "effect_program_structure",
            "set_staff_role": "effect_program_structure",
            "deliver_resource": "resource_delivery",
            "modify_supply_schedule": "delivery_delayed",
            "set_staff_availability": "staff_unavailable",
            "water_outage": "water_outage",
            "add_staff": "effect_program_structure",
            "add_toilet": "effect_program_structure",
            "set_ambient_noise": "effect_program_environment",
            "modify_demand_rate": "effect_program_environment",
            "set_ventilation": "effect_program_environment",
            "set_lighting": "effect_program_environment",
            "set_privacy": "effect_program_environment",
            "set_temperature": "effect_program_environment",
            "add_evacuees": "effect_program_population",
            "depart_evacuees": "effect_program_population",
            "population_presence": "effect_program_population",
            "add_temporary_space": "effect_program_temporary_space",
            "move_facility": "effect_program_geometry",
            "transfer_equipment": "effect_program_geometry",
            "set_partition": "effect_program_geometry",
            "set_route_access": "effect_program_geometry",
            "temporary_room_geometry": "effect_program_geometry",
        },
        "expiry": {
            "set_facility_availability": "effect_program_expire",
            "facility_outage": "effect_program_expire",
            "water_outage": "water_restored",
            "set_ambient_noise": "effect_program_environment_expire",
            "modify_demand_rate": "effect_program_environment_expire",
            "set_ventilation": "effect_program_environment_expire",
            "set_lighting": "effect_program_environment_expire",
            "set_privacy": "effect_program_environment_expire",
            "set_temperature": "effect_program_environment_expire",
            "add_temporary_space": "effect_program_temporary_space_expire",
            "set_partition": "effect_program_geometry_expire",
            "set_route_access": "effect_program_geometry_expire",
            "temporary_room_geometry": "effect_program_geometry_expire",
        },
        "derived": {
            "modify_supply_schedule": "resource_delivery",
            "set_staff_availability": "staff_restored",
            "add_evacuees": "population_approach_complete",
        },
    }
)


@kernel_first("effects", "validate_state")
def validate_effect_program_state(
    world: WorldState,
    *,
    validate_event_history: bool = True,
) -> None:
    """Validate versioned modifier state and its persisted event/provenance references."""
    state = world.model_state.get(EFFECT_PROGRAM_STATE_KEY)
    if state is None:
        if not validate_event_history:
            if any(
                isinstance(event.provenance.get("effect_program_v1"), Mapping)
                for event in world.event_queue.pending()
            ):
                raise ValueError("EffectProgram events require effect_program_v1 state")
            return
        if world.event_log.with_provenance_object("effect_program_v1") or any(
            isinstance(event.provenance.get("effect_program_v1"), Mapping)
            for event in world.event_queue.pending()
        ):
            raise ValueError("EffectProgram events require effect_program_v1 state")
        return
    if (
        not isinstance(state, dict)
        or set(state) != _EFFECT_STATE_FIELDS
        or state.get("schema_version") != 1
        or not isinstance(state.get("programs"), dict)
        or not isinstance(state.get("active_facility_modifiers"), dict)
    ):
        raise ValueError("effect_program_v1 has an invalid exact schema")
    if not validate_event_history:
        return
    known_effects: dict[tuple[str, str], dict[str, Any]] = {}
    scheduled_events: dict[str, str] = {}
    for program_id, program in state["programs"].items():
        if (
            not isinstance(program_id, str)
            or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._:-]{0,63}", program_id)
            or not isinstance(program, dict)
            or set(program)
            != {
                "program_hash",
                "source_text_hash",
                "narrative",
                "compiler_provenance",
                "assumptions",
                "support_mode",
                "effect_ids",
                "operators",
                "scheduled_event_ids",
            }
        ):
            raise ValueError("effect program record fields are invalid")
        for name in ("program_hash", "source_text_hash"):
            value = program[name]
            if not isinstance(value, str) or not re.fullmatch(r"[0-9a-f]{64}", value):
                raise ValueError(f"effect program {name} is invalid")
        if (
            not isinstance(program["narrative"], str)
            or not 1 <= len(program["narrative"]) <= 1000
            or not isinstance(program["assumptions"], list)
            or not isinstance(program["support_mode"], str)
            or program["support_mode"] not in {mode.value for mode in SupportMode}
            or not isinstance(program["effect_ids"], list)
            or any(not isinstance(item, str) for item in program["effect_ids"])
            or not isinstance(program["operators"], dict)
            or not isinstance(program["scheduled_event_ids"], list)
            or any(not isinstance(item, str) for item in program["scheduled_event_ids"])
        ):
            raise ValueError("effect program metadata values are invalid")
        if len(program["effect_ids"]) != len(set(program["effect_ids"])):
            raise ValueError("effect program effect IDs must be unique")
        if set(program["operators"]) != set(program["effect_ids"]) or any(
            not isinstance(op, str) or DEFAULT_PRIMITIVE_REGISTRY.get(op) is None
            for op in program["operators"].values()
        ):
            raise ValueError("effect program operator registry references are invalid")
        if len(program["scheduled_event_ids"]) != len(set(program["scheduled_event_ids"])):
            raise ValueError("effect program scheduled event IDs must be unique")
        CompilerProvenance.model_validate(program["compiler_provenance"])
        assumption_ids: set[str] = set()
        for assumption in program["assumptions"]:
            parsed = PlanAssumption.model_validate(assumption)
            if parsed.assumption_id in assumption_ids:
                raise ValueError("effect program assumption IDs must be unique")
            assumption_ids.add(parsed.assumption_id)
        for effect_id in program["effect_ids"]:
            if not isinstance(effect_id, str) or not re.fullmatch(
                r"[A-Za-z0-9][A-Za-z0-9._:-]{0,63}", effect_id
            ):
                raise ValueError("effect program effect ID is invalid")
            known_effects[(program_id, effect_id)] = program
        for event_id in program["scheduled_event_ids"]:
            if (
                not isinstance(event_id, str)
                or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._:-]{0,63}", event_id)
                or event_id in scheduled_events
            ):
                raise ValueError("effect program event IDs must be unique stable IDs")
            scheduled_events[event_id] = program_id
    # Events without an EffectProgram marker pass the loop below unread.
    all_events = [
        *world.event_queue.pending(),
        *(record.event for record in world.event_log.with_provenance_object("effect_program_v1")),
    ]
    by_id = EventsById(world.event_log, world.event_queue.pending())
    for event_id, program_id in scheduled_events.items():
        if event_id not in by_id:
            raise ValueError("effect program references an unknown scheduled or realized event")
        marker = by_id[event_id].provenance.get("effect_program_v1")
        if not isinstance(marker, Mapping) or marker.get("program_id") != program_id:
            raise ValueError("effect program provenance does not match its event")
    for event in all_events:
        marker = event.provenance.get("effect_program_v1")
        if not isinstance(marker, Mapping):
            continue
        base_marker_fields = {
            "program_id",
            "effect_id",
            "op",
            "source_text_hash",
            "compiler_provenance",
            "assumption_ids",
            "policy_dependencies",
            "units",
            "phase",
        }
        if set(marker) not in {
            frozenset(base_marker_fields),
            frozenset(base_marker_fields | {"guard"}),
        }:
            raise ValueError("EffectProgram event provenance fields are invalid")
        if not isinstance(marker.get("program_id"), str) or not isinstance(
            marker.get("effect_id"), str
        ):
            raise ValueError("EffectProgram event provenance IDs are invalid")
        key = (marker.get("program_id"), marker.get("effect_id"))
        if key not in known_effects:
            raise ValueError("event provenance references an unknown EffectProgram effect")
        program_record = known_effects[key]
        if marker.get("op") != program_record["operators"].get(key[1]):
            raise ValueError("effect event operator does not match the scheduled program")
        if marker.get("source_text_hash") != known_effects[key]["source_text_hash"]:
            raise ValueError("event source hash does not match its EffectProgram")
        if not isinstance(marker.get("phase"), str) or marker.get("phase") not in {
            "start",
            "expiry",
            "derived",
        }:
            raise ValueError("effect event phase is invalid")
        CompilerProvenance.model_validate(marker["compiler_provenance"])
        if (
            not isinstance(marker["assumption_ids"], (list, tuple))
            or not isinstance(marker["policy_dependencies"], (list, tuple))
            or not isinstance(marker["units"], str)
            or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._:-]{0,63}", marker["units"])
            or any(not isinstance(item, str) for item in marker["assumption_ids"])
            or any(not isinstance(item, str) for item in marker["policy_dependencies"])
        ):
            raise ValueError("EffectProgram event provenance values are invalid")
        if "guard" in marker:
            if marker["phase"] != "start":
                raise ValueError("only a start event may store a guard")
            GuardExpression.model_validate(marker["guard"])
        if event.event_id not in scheduled_events and marker.get("phase") != "derived":
            raise ValueError("unscheduled EffectProgram event lacks derived-event marking")
        if marker.get("phase") == "derived" and event.causation_id not in by_id:
            raise ValueError("derived EffectProgram event lacks a known causal parent")
        op = marker["op"]
        expected_event_type = EFFECT_EVENT_TYPES[marker["phase"]].get(op)
        if event.event_type != expected_event_type:
            raise ValueError("EffectProgram event type does not match its registered operator")
    for facility_id, record in state["active_facility_modifiers"].items():
        if (
            facility_id not in world.facilities
            or not isinstance(record, dict)
            or set(record)
            != {
                "baseline_working_units",
                "modifiers",
            }
        ):
            raise ValueError("active facility modifier record is invalid")
        baseline = record["baseline_working_units"]
        modifiers = record["modifiers"]
        if (
            not isinstance(baseline, int)
            or isinstance(baseline, bool)
            or not 0 <= baseline <= world.facilities[facility_id].capacity
            or not isinstance(modifiers, list)
            or not modifiers
        ):
            raise ValueError("active facility modifier baseline or stack is invalid")
        modifier_ids: set[str] = set()
        target_units: list[int] = []
        for item in modifiers:
            if not isinstance(item, dict) or set(item) != {
                "modifier_id",
                "program_id",
                "effect_id",
                "working_units",
                "start_minute",
                "sequence",
            }:
                raise ValueError("facility modifier stack entry is invalid")
            expected_id = _modifier_id(item["program_id"], item["effect_id"])
            if (
                not isinstance(item["modifier_id"], str)
                or not isinstance(item["program_id"], str)
                or not isinstance(item["effect_id"], str)
                or item["modifier_id"] != expected_id
                or item["modifier_id"] in modifier_ids
                or (item["program_id"], item["effect_id"]) not in known_effects
                or any(
                    not isinstance(item[name], int)
                    or isinstance(item[name], bool)
                    or item[name] < 0
                    for name in ("working_units", "start_minute", "sequence")
                )
            ):
                raise ValueError("facility modifier stack identity or numeric values are invalid")
            if item["working_units"] > baseline:
                raise ValueError("facility modifier exceeds its captured baseline")
            modifier_ids.add(item["modifier_id"])
            target_units.append(item["working_units"])
        facility = world.facilities[facility_id]
        if facility.working_units != min(baseline, *target_units):
            raise ValueError("facility working availability does not match its modifier stack")


def effect_event_guard_or_suppress(world: WorldState, event: ScheduledEvent) -> None:
    """Raise a normal suppressed outcome when an optional program guard is false."""
    if not should_apply_effect_guard(world, event):
        raise EventSuppressed("EffectProgram guard evaluated false")


# No source text is persisted in WorldState. Runtime state stores only a digest and
# declarative audit metadata; executable Python remains the code-owned registry above.
