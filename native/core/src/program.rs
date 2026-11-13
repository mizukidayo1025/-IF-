//! The EffectProgram of an intervention, before it is scheduled (#304, under #292): its contract
//! (`EffectProgram` of `src/sheltersim/events/effects.py`), read strictly and dumped with its
//! defaults as `program.model_dump(mode="json")` writes it, and the checks of
//! `validate_effect_program` for the six operators the offline reading of a text makes
//! (`facility_outage`, `add_staff`, `add_toilet`, `water_outage`, `add_evacuees`,
//! `depart_evacuees`), with Python's texts.
//!
//! The approval's hash is the sha256 of the dump's canonical JSON (`digest(program.model_dump())`,
//! `storage/projects.py`): `dump_json` gives those bytes.
//!
//! What the browser does not repeat, and says so:
//! - A program the contract refuses is a `Value` error whose text is the core's, not pydantic's.
//!   The reader takes the JSON types a dump has (an int where an int is, a bool where a bool is),
//!   and refuses a value pydantic would coerce. The screen sends no typed program; the reading of
//!   a text makes its dump.
//! - The world's own check (`world.validate()`, E6 of #250) is not run: a valid world gives no
//!   issue there.
//! - An effect of another operator of the registry, or parameters its model refuses (whose issue
//!   holds pydantic's text), is `Unsupported`.
//! - Python reads a guard's fields as a set, whose order changes between processes; they are read
//!   here in sorted order.

use std::collections::BTreeSet;
use std::sync::Arc;

use indexmap::IndexMap;

use crate::entities::FacilityRow;
use crate::error::{Error, Result};
use crate::events::EventRow;
use crate::geometry::Geo;
use crate::row::ValueRow;
use crate::rules::kit::Held;
use crate::rules::overlay;
use crate::state::effect_program::{EffectProgramRow, NAME as EFFECT_PROGRAM};
use crate::text::py_repr;
use crate::value::V;
use crate::world::{Place, World, MODEL_CONFIGS};

type Map = IndexMap<Arc<str>, V>;

const MAX_EFFECTS: usize = 50;
const MAX_ASSUMPTIONS: usize = 100;
const MAX_IDS: usize = 50;
const MAX_CANDIDATES: usize = 100;
const MAX_SCHEDULED_EVENTS: usize = 100;
const MAX_QUEUE: usize = 10_000;
const MAX_EFFECT_DURATION: i64 = 60 * 24 * 366;
const SUPPORT_MODES: [&str; 5] = ["native", "composed", "approximate", "clarification_required", "not_simulatable"];
const GUARD_OPERATORS: [&str; 8] = ["eq", "ne", "gt", "gte", "lt", "lte", "and", "or"];
const GUARD_FIELDS: [&str; 7] =
    ["simulation.minute", "target.capacity", "target.working_units", "target.queue_length", "zone.occupancy", "resource.available_quantity", "staff.available"];

// ---- The contract -----------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct Selector {
    pub kind: Arc<str>,
    pub target_id: Option<Arc<str>>,
    pub candidate_ids: Vec<Arc<str>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Guard {
    pub operator: Arc<str>,
    pub field: Option<Arc<str>>,
    /// The scalar of a comparison, `V::Null` for none.
    pub value: V,
    pub operands: Vec<Guard>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Effect {
    pub effect_id: Arc<str>,
    pub op: Arc<str>,
    pub target_selector: Selector,
    pub start_minute: i64,
    pub end_minute: Option<i64>,
    pub guard: Option<Guard>,
    /// The parameters as given (`dict[str, Any]`), a `V::Map`.
    pub parameters: V,
    pub units: Arc<str>,
    pub source_id: Arc<str>,
    pub assumption_ids: Vec<Arc<str>>,
    pub policy_dependencies: Vec<Arc<str>>,
    pub causation_id: Option<Arc<str>>,
    pub reversible: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Assumption {
    pub assumption_id: Arc<str>,
    pub description: Arc<str>,
    pub source_id: Arc<str>,
    pub units: Option<Arc<str>>,
    pub value: V,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Provenance {
    pub compiler_kind: Arc<str>,
    pub compiler_version: Arc<str>,
    pub model_id: Option<Arc<str>>,
    pub prompt_hash: Option<Arc<str>>,
}

/// `EffectProgram`, as its contract holds it.
#[derive(Clone, Debug, PartialEq)]
pub struct EffectProgram {
    pub program_id: Arc<str>,
    pub narrative: Arc<str>,
    pub source_text: Arc<str>,
    pub simulation_start_minute: i64,
    pub effects: Vec<Effect>,
    pub assumptions: Vec<Assumption>,
    pub support_mode: Arc<str>,
    pub compiler_provenance: Provenance,
}

fn refuse<T>(what: impl std::fmt::Display) -> Result<T> {
    Err(Error::Value(format!("invalid EffectProgram: {what}")))
}

/// The fields of a dict with no others (`extra="forbid"`).
struct Fields<'a> {
    map: &'a Map,
    what: &'static str,
}

impl<'a> Fields<'a> {
    fn of(value: &'a V, what: &'static str, allowed: &[&str]) -> Result<Self> {
        let V::Map(map) = value else { return refuse(format!("{what} is not an object")) };
        if let Some(extra) = map.keys().find(|key| !allowed.contains(&&***key)) {
            return refuse(format!("{what} has the extra field {}", py_repr(extra)));
        }
        Ok(Self { map, what })
    }

    fn get(&self, name: &str) -> Option<&'a V> {
        self.map.get(name)
    }

    fn required(&self, name: &str) -> Result<&'a V> {
        self.map.get(name).map_or_else(|| refuse(format!("{} has no {name}", self.what)), Ok)
    }

    fn stable_id(&self, name: &str) -> Result<Arc<str>> {
        stable_id(self.required(name)?, self.what, name)
    }

    fn optional_id(&self, name: &str) -> Result<Option<Arc<str>>> {
        match self.get(name) {
            None | Some(V::Null) => Ok(None),
            Some(value) => stable_id(value, self.what, name).map(Some),
        }
    }

    fn text(&self, name: &str, max: usize) -> Result<Arc<str>> {
        match self.required(name)? {
            V::Str(text) if (1..=max).contains(&text.chars().count()) => Ok(text.clone()),
            _ => refuse(format!("{}.{name} is not a string of 1 to {max} characters", self.what)),
        }
    }

    fn minute(&self, name: &str) -> Result<i64> {
        minute(self.required(name)?, self.what, name)
    }

    fn ids(&self, name: &str, max: usize) -> Result<Vec<Arc<str>>> {
        match self.get(name) {
            None => Ok(Vec::new()),
            Some(V::List(items)) if items.len() <= max => items.iter().map(|item| stable_id(item, self.what, name)).collect(),
            Some(_) => refuse(format!("{}.{name} is not a list of at most {max} IDs", self.what)),
        }
    }
}

/// `StableId`: 1 to 64 characters, a letter or digit of ASCII first, then those or `._:-`.
pub fn is_stable_id(text: &str) -> bool {
    let bytes = text.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_alphanumeric()
        && bytes.iter().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

fn stable_id(value: &V, what: &str, name: &str) -> Result<Arc<str>> {
    match value {
        V::Str(text) if is_stable_id(text) => Ok(text.clone()),
        _ => refuse(format!("{what}.{name} is not a stable ID")),
    }
}

fn minute(value: &V, what: &str, name: &str) -> Result<i64> {
    match value {
        V::Int(minute) if *minute >= 0 => Ok(*minute),
        _ => refuse(format!("{what}.{name} is not a whole minute, not negative")),
    }
}

fn finite(value: &V) -> bool {
    !matches!(value, V::Float(number) if !number.is_finite())
}

fn selector(value: &V) -> Result<Selector> {
    let fields = Fields::of(value, "target_selector", &["kind", "target_id", "candidate_ids"])?;
    let found = Selector { kind: fields.stable_id("kind")?, target_id: fields.optional_id("target_id")?, candidate_ids: fields.ids("candidate_ids", MAX_CANDIDATES)? };
    if found.target_id.is_none() && found.candidate_ids.is_empty() {
        return refuse("a target ID or candidate IDs are required");
    }
    if found.target_id.is_some() && !found.candidate_ids.is_empty() {
        return refuse("a selected target cannot also contain candidate IDs");
    }
    if found.candidate_ids.iter().collect::<BTreeSet<_>>().len() != found.candidate_ids.len() {
        return refuse("candidate target IDs must be unique");
    }
    Ok(found)
}

fn guard(value: &V) -> Result<Guard> {
    let fields = Fields::of(value, "guard", &["operator", "field", "value", "operands"])?;
    let operator = match fields.required("operator")? {
        V::Str(operator) if GUARD_OPERATORS.contains(&&**operator) => operator.clone(),
        _ => return refuse("a guard's operator is not one of the guard operators"),
    };
    let field = match fields.get("field") {
        None | Some(V::Null) => None,
        Some(V::Str(field)) if GUARD_FIELDS.contains(&&**field) => Some(field.clone()),
        Some(_) => return refuse("a guard's field is not one of the guard fields"),
    };
    let value = match fields.get("value") {
        None => V::Null,
        Some(value @ (V::Null | V::Bool(_) | V::Int(_) | V::Float(_))) => value.clone(),
        Some(V::Str(text)) if text.chars().count() <= 256 => V::Str(text.clone()),
        Some(V::Str(_)) => return refuse("guard string values are limited to 256 characters"),
        Some(_) => return refuse("a guard's value is not a scalar"),
    };
    if !finite(&value) {
        return refuse("guard numeric values must be finite");
    }
    let operands = match fields.get("operands") {
        None => Vec::new(),
        Some(V::List(items)) if items.len() <= 4 => items.iter().map(guard).collect::<Result<_>>()?,
        Some(_) => return refuse("a guard's operands are not a list of at most 4"),
    };
    let found = Guard { operator, field, value, operands };
    if ["eq", "ne", "gt", "gte", "lt", "lte"].contains(&&*found.operator) {
        if found.field.is_none() || matches!(found.value, V::Null) || !found.operands.is_empty() {
            return refuse("comparison guards require one allowed field and scalar value");
        }
        if ["gt", "gte", "lt", "lte"].contains(&&*found.operator) && !matches!(found.value, V::Int(_) | V::Float(_)) {
            return refuse("ordered comparisons require a numeric value");
        }
    } else if found.field.is_some() || !matches!(found.value, V::Null) || found.operands.len() < 2 {
        return refuse("and/or guards require two or more operands and no leaf fields");
    }
    fn measure(node: &Guard, depth: usize) -> Result<usize> {
        if depth > 4 {
            return refuse("guard nesting depth cannot exceed four");
        }
        let mut count = 1;
        for child in &node.operands {
            count += measure(child, depth + 1)?;
        }
        Ok(count)
    }
    if measure(&found, 1)? > 20 {
        return refuse("guard AST cannot exceed 20 nodes");
    }
    Ok(found)
}

/// `_validate_bounded_json`.
fn bounded(value: &V, depth: usize) -> Result<()> {
    if depth > 8 {
        return refuse("effect JSON parameters exceed maximum nesting depth");
    }
    match value {
        V::Null | V::Bool(_) | V::Int(_) => Ok(()),
        V::Big(_) => refuse("effect JSON numbers must fit 64 bits"),
        V::Str(text) if text.chars().count() > 1000 => refuse("effect JSON strings are limited to 1000 characters"),
        V::Str(_) => Ok(()),
        V::Float(number) if !number.is_finite() => refuse("effect JSON numbers must be finite"),
        V::Float(_) => Ok(()),
        V::List(items) if items.len() > 100 => refuse("effect JSON arrays are limited to 100 items"),
        V::List(items) => items.iter().try_for_each(|item| bounded(item, depth + 1)),
        V::Map(map) if map.len() > 100 || map.keys().any(|key| key.chars().count() > 256) => {
            refuse("effect JSON objects need short string keys and at most 100 fields")
        }
        V::Map(map) => map.values().try_for_each(|item| bounded(item, depth + 1)),
    }
}

fn effect(value: &V) -> Result<Effect> {
    let fields = Fields::of(
        value,
        "effect",
        &[
            "effect_id",
            "op",
            "target_selector",
            "start_minute",
            "end_minute",
            "guard",
            "parameters",
            "units",
            "source_id",
            "assumption_ids",
            "policy_dependencies",
            "causation_id",
            "reversible",
        ],
    )?;
    let parameters = match fields.get("parameters") {
        None => V::Map(Map::new()),
        Some(parameters @ V::Map(_)) => parameters.clone(),
        Some(_) => return refuse("an effect's parameters are not an object"),
    };
    bounded(&parameters, 0)?;
    let end_minute = match fields.get("end_minute") {
        None | Some(V::Null) => None,
        Some(value) => Some(minute(value, "effect", "end_minute")?),
    };
    let found = Effect {
        effect_id: fields.stable_id("effect_id")?,
        op: fields.stable_id("op")?,
        target_selector: selector(fields.required("target_selector")?)?,
        start_minute: fields.minute("start_minute")?,
        end_minute,
        guard: match fields.get("guard") {
            None | Some(V::Null) => None,
            Some(value) => Some(guard(value)?),
        },
        parameters,
        units: fields.stable_id("units")?,
        source_id: fields.stable_id("source_id")?,
        assumption_ids: fields.ids("assumption_ids", MAX_IDS)?,
        policy_dependencies: fields.ids("policy_dependencies", MAX_IDS)?,
        causation_id: fields.optional_id("causation_id")?,
        reversible: match fields.get("reversible") {
            None => true,
            Some(V::Bool(flag)) => *flag,
            Some(_) => return refuse("an effect's reversible is not a bool"),
        },
    };
    if found.end_minute.is_some_and(|end| end <= found.start_minute) {
        return refuse("end_minute must be later than start_minute");
    }
    Ok(found)
}

fn assumption(value: &V) -> Result<Assumption> {
    let fields = Fields::of(value, "assumption", &["assumption_id", "description", "source_id", "units", "value"])?;
    let value = match fields.get("value") {
        None => V::Null,
        Some(value @ (V::Null | V::Bool(_) | V::Int(_) | V::Float(_) | V::Str(_))) => value.clone(),
        Some(_) => return refuse("an assumption's value is not a scalar"),
    };
    if !finite(&value) {
        return refuse("assumption values must be finite");
    }
    Ok(Assumption {
        assumption_id: fields.stable_id("assumption_id")?,
        description: fields.text("description", 500)?,
        source_id: fields.stable_id("source_id")?,
        units: fields.optional_id("units")?,
        value,
    })
}

fn provenance(value: &V) -> Result<Provenance> {
    let fields = Fields::of(value, "compiler_provenance", &["compiler_kind", "compiler_version", "model_id", "prompt_hash"])?;
    let compiler_kind = match fields.required("compiler_kind")? {
        V::Str(kind) if matches!(&**kind, "offline_rule" | "llm") => kind.clone(),
        _ => return refuse("compiler_kind is not offline_rule or llm"),
    };
    let prompt_hash = match fields.get("prompt_hash") {
        None | Some(V::Null) => None,
        Some(V::Str(hash)) if hash.chars().count() <= 128 => Some(hash.clone()),
        Some(_) => return refuse("prompt_hash is not a string of at most 128 characters"),
    };
    Ok(Provenance { compiler_kind, compiler_version: fields.stable_id("compiler_version")?, model_id: fields.optional_id("model_id")?, prompt_hash })
}

impl EffectProgram {
    /// `EffectProgram.model_validate(value)`, strictly (above).
    pub fn read(value: &V) -> Result<Self> {
        let fields = Fields::of(
            value,
            "program",
            &["schema_version", "program_id", "narrative", "source_text", "simulation_start_minute", "effects", "assumptions", "support_mode", "compiler_provenance"],
        )?;
        if !matches!(fields.get("schema_version"), None | Some(V::Int(1))) {
            return refuse("schema_version is not 1");
        }
        let effects = match fields.required("effects")? {
            V::List(items) if (1..=MAX_EFFECTS).contains(&items.len()) => items.iter().map(effect).collect::<Result<Vec<_>>>()?,
            _ => return refuse(format!("effects is not a list of 1 to {MAX_EFFECTS}")),
        };
        let assumptions = match fields.get("assumptions") {
            None => Vec::new(),
            Some(V::List(items)) if items.len() <= MAX_ASSUMPTIONS => items.iter().map(assumption).collect::<Result<Vec<_>>>()?,
            Some(_) => return refuse(format!("assumptions is not a list of at most {MAX_ASSUMPTIONS}")),
        };
        let support_mode = match fields.required("support_mode")? {
            V::Str(mode) if SUPPORT_MODES.contains(&&**mode) => mode.clone(),
            _ => return refuse("support_mode is not a support mode"),
        };
        let program = EffectProgram {
            program_id: fields.stable_id("program_id")?,
            narrative: fields.text("narrative", 1000)?,
            source_text: fields.text("source_text", 4000)?,
            simulation_start_minute: fields.minute("simulation_start_minute")?,
            effects,
            assumptions,
            support_mode,
            compiler_provenance: provenance(fields.required("compiler_provenance")?)?,
        };
        if program.effects.iter().any(|effect| effect.start_minute < program.simulation_start_minute) {
            return refuse("effects cannot start before simulation_start_minute");
        }
        if program.effects.iter().map(|effect| &effect.effect_id).collect::<BTreeSet<_>>().len() != program.effects.len() {
            return refuse("effect IDs must be unique");
        }
        let declared: BTreeSet<&Arc<str>> = program.assumptions.iter().map(|item| &item.assumption_id).collect();
        if declared.len() != program.assumptions.len() {
            return refuse("assumption IDs must be unique");
        }
        if program.effects.iter().any(|effect| effect.assumption_ids.iter().any(|id| !declared.contains(id))) {
            return refuse("effects may reference only declared assumptions");
        }
        Ok(program)
    }

    /// `read` of a program's JSON bytes.
    pub fn of_json(raw: &[u8]) -> Result<Self> {
        let value: serde_json::Value = serde_json::from_slice(raw).map_err(|error| Error::Value(format!("invalid EffectProgram: not JSON: {error}")))?;
        let value = V::from_json(&value).ok_or_else(|| Error::Value("invalid EffectProgram: a number that is not Python's".to_owned()))?;
        Self::read(&value)
    }

    /// `program.model_dump(mode="json")`: every field, the defaults too.
    pub fn dump(&self) -> V {
        object([
            ("schema_version", V::Int(1)),
            ("program_id", string(&self.program_id)),
            ("narrative", string(&self.narrative)),
            ("source_text", string(&self.source_text)),
            ("simulation_start_minute", V::Int(self.simulation_start_minute)),
            ("effects", V::List(self.effects.iter().map(Effect::dump).collect())),
            ("assumptions", V::List(self.assumptions.iter().map(Assumption::dump).collect())),
            ("support_mode", string(&self.support_mode)),
            ("compiler_provenance", self.compiler_provenance.dump()),
        ])
    }

    /// The canonical JSON of `dump`: the bytes whose sha256 is the approval's hash.
    pub fn dump_json(&self) -> Result<Vec<u8>> {
        self.dump().to_json()
    }
}

impl Effect {
    fn dump(&self) -> V {
        object([
            ("effect_id", string(&self.effect_id)),
            ("op", string(&self.op)),
            ("target_selector", self.target_selector.dump()),
            ("start_minute", V::Int(self.start_minute)),
            ("end_minute", self.end_minute.map_or(V::Null, V::Int)),
            ("guard", self.guard.as_ref().map_or(V::Null, Guard::dump)),
            ("parameters", self.parameters.clone()),
            ("units", string(&self.units)),
            ("source_id", string(&self.source_id)),
            ("assumption_ids", ids(&self.assumption_ids)),
            ("policy_dependencies", ids(&self.policy_dependencies)),
            ("causation_id", optional(&self.causation_id)),
            ("reversible", V::Bool(self.reversible)),
        ])
    }
}

impl Selector {
    fn dump(&self) -> V {
        object([("kind", string(&self.kind)), ("target_id", optional(&self.target_id)), ("candidate_ids", ids(&self.candidate_ids))])
    }
}

impl Guard {
    fn dump(&self) -> V {
        object([
            ("operator", string(&self.operator)),
            ("field", optional(&self.field)),
            ("value", self.value.clone()),
            ("operands", V::List(self.operands.iter().map(Guard::dump).collect())),
        ])
    }

    /// `_guard_fields`: the leaves' fields.
    fn fields(&self, into: &mut BTreeSet<Arc<str>>) {
        match &self.field {
            Some(field) => {
                into.insert(field.clone());
            }
            None => self.operands.iter().for_each(|operand| operand.fields(into)),
        }
    }
}

impl Assumption {
    fn dump(&self) -> V {
        object([
            ("assumption_id", string(&self.assumption_id)),
            ("description", string(&self.description)),
            ("source_id", string(&self.source_id)),
            ("units", optional(&self.units)),
            ("value", self.value.clone()),
        ])
    }
}

impl Provenance {
    fn dump(&self) -> V {
        object([
            ("compiler_kind", string(&self.compiler_kind)),
            ("compiler_version", string(&self.compiler_version)),
            ("model_id", optional(&self.model_id)),
            ("prompt_hash", optional(&self.prompt_hash)),
        ])
    }
}

fn object<const N: usize>(fields: [(&str, V); N]) -> V {
    V::Map(fields.into_iter().map(|(name, value)| (Arc::from(name), value)).collect())
}

fn string(text: &Arc<str>) -> V {
    V::Str(text.clone())
}

fn optional(text: &Option<Arc<str>>) -> V {
    text.clone().map_or(V::Null, V::Str)
}

fn ids(items: &[Arc<str>]) -> V {
    V::List(items.iter().cloned().map(V::Str).collect())
}

// ---- The operators ----------------------------------------------------------------------------------

/// What `DEFAULT_PRIMITIVE_REGISTRY` says of an operator the browser checks.
struct Primitive {
    target_kinds: &'static [&'static str],
    units: &'static [&'static str],
    policies: &'static [&'static str],
    timed: bool,
}

fn primitive(op: &str) -> Option<Primitive> {
    let (target_kinds, units, policies, timed): (&[&str], &[&str], &[&str], bool) = match op {
        "facility_outage" => (&["facility"], &["facility_unit"], &["facility_capacity"], true),
        "add_staff" => (&["zone"], &["staff_member"], &["staff_operations"], false),
        "add_toilet" => (&["zone"], &["toilet_stall"], &["facility_capacity"], false),
        "add_evacuees" | "depart_evacuees" => (&["zone"], &["evacuee"], &["population_dynamics_v1"], false),
        "water_outage" => (&["facility", "zone"], &["water_supply"], &["operational_events_v1", "resource_service_v1"], true),
        _ => return None,
    };
    Some(Primitive { target_kinds, units, policies, timed })
}

/// The parameters of an effect, as its model reads them; `Unsupported` where the model refuses
/// them (the issue would hold pydantic's text).
#[derive(Default)]
struct Parameters {
    count: i64,
    units: i64,
    admission_zone_ids: Vec<Arc<str>>,
    approach_route_id: Option<Arc<str>>,
}

fn parameters(effect: &Effect) -> Result<Parameters> {
    let unsupported = || Error::unsupported("parameters their model refuses: the issue holds pydantic's text");
    let V::Map(map) = &effect.parameters else { return Err(unsupported()) };
    let allowed: &[&str] = match &*effect.op {
        "facility_outage" => &["units"],
        "add_staff" | "add_toilet" => &["count"],
        "water_outage" => &["outage"],
        "add_evacuees" => &["count", "household_size", "mobility", "language", "admission_zone_ids", "approach_route_id", "person_context_overrides"],
        "depart_evacuees" => &["count", "include_initial"],
        _ => return Err(unsupported()),
    };
    if map.keys().any(|key| !allowed.contains(&&**key)) {
        return Err(unsupported());
    }
    let int = |name: &str, low: i64, high: i64, default: Option<i64>| -> Result<i64> {
        match (map.get(name), default) {
            (Some(V::Int(value)), _) if (low..=high).contains(value) => Ok(*value),
            (None, Some(default)) => Ok(default),
            _ => Err(unsupported()),
        }
    };
    let mut found = Parameters::default();
    match &*effect.op {
        "facility_outage" => found.units = int("units", 1, 1000, None)?,
        "add_staff" => found.count = int("count", 1, 100, None)?,
        "add_toilet" => found.count = int("count", 1, 20, None)?,
        "water_outage" => {
            if !matches!(map.get("outage"), None | Some(V::Bool(true))) {
                return Err(unsupported());
            }
        }
        "depart_evacuees" => {
            found.count = int("count", 1, 1000, None)?;
            if !matches!(map.get("include_initial"), None | Some(V::Bool(_))) {
                return Err(unsupported());
            }
        }
        _ => {
            found.count = int("count", 1, 1000, None)?;
            int("household_size", 1, 12, Some(1))?;
            match map.get("mobility") {
                None => {}
                Some(V::Str(mobility)) if matches!(&**mobility, "independent" | "assisted" | "wheelchair") => {}
                _ => return Err(unsupported()),
            }
            match map.get("language") {
                None => {}
                Some(V::Str(language)) if is_stable_id(language) => {}
                _ => return Err(unsupported()),
            }
            found.admission_zone_ids = match map.get("admission_zone_ids") {
                None => Vec::new(),
                Some(V::List(items)) if items.len() <= 16 => items
                    .iter()
                    .map(|item| match item {
                        V::Str(id) if is_stable_id(id) => Ok(id.clone()),
                        _ => Err(unsupported()),
                    })
                    .collect::<Result<_>>()?,
                Some(_) => return Err(unsupported()),
            };
            if found.admission_zone_ids.iter().collect::<BTreeSet<_>>().len() != found.admission_zone_ids.len() {
                return Err(unsupported());
            }
            found.approach_route_id = match map.get("approach_route_id") {
                None | Some(V::Null) => None,
                Some(V::Str(id)) if is_stable_id(id) => Some(id.clone()),
                Some(_) => return Err(unsupported()),
            };
            // The people's contexts are a contract of their own, which the reading of a text
            // never gives.
            match map.get("person_context_overrides") {
                None => {}
                Some(V::List(items)) if items.is_empty() => {}
                _ => return Err(Error::unsupported("arrivals with people's contexts are not checked in the browser")),
            }
        }
    }
    Ok(found)
}

// ---- The world ---------------------------------------------------------------------------------------

fn has(world: &World, name: &str) -> Result<bool> {
    Ok(match world.slots.get(name) {
        Some(slot) => {
            let slot = slot.try_borrow()?;
            slot.place != Place::Field && (slot.row.is_some() || slot.taint.is_some())
        }
        None => false,
    })
}

/// `model_state.get(name)` of a namespace held as a value; None without the key.
fn value(world: &World, name: &str) -> Result<Option<V>> {
    if !has(world, name)? {
        return Ok(None);
    }
    let slot = world.slots.get(name).ok_or_else(|| Error::Runtime("a slot left the world".to_owned()))?.try_borrow()?;
    if slot.row.is_none() {
        return Err(Error::unsupported(format!("{name} is tainted")));
    }
    Ok(Some(slot.row::<ValueRow>()?.0.clone()))
}

fn configs(world: &World) -> Result<Map> {
    match &world.slots.try_row::<ValueRow>(MODEL_CONFIGS)?.0 {
        V::Map(configs) => Ok(configs.clone()),
        _ => Err(Error::unsupported("the model configs are not a dict")),
    }
}

fn pending(world: &World) -> Result<Vec<Arc<EventRow>>> {
    let queue = world.queue()?;
    queue.ordered().iter().map(|(_, serial)| queue.slot(*serial).row.clone().ok_or_else(|| Error::unsupported("a pending event has no row"))).collect()
}

fn payload<'a>(row: &'a EventRow, key: &str) -> Option<&'a V> {
    match &row.payload {
        V::Map(map) => map.get(key),
        _ => None,
    }
}

/// `sum(event.payload.get("count", 0) ...)` of the pending events of this type and operator.
fn pending_count(events: &[Arc<EventRow>], event_type: &str, op: &str) -> Result<i64> {
    let mut total = 0i64;
    for event in events {
        if &*event.event_type != event_type || !matches!(payload(event, "op"), Some(V::Str(found)) if &**found == op) {
            continue;
        }
        match payload(event, "count") {
            None => {}
            Some(V::Int(count)) => total = total.checked_add(*count).ok_or_else(|| Error::unsupported("a count past 64 bits"))?,
            Some(V::Bool(flag)) => total += i64::from(*flag),
            Some(_) => return Err(Error::unsupported("a pending count that is not an int")),
        }
    }
    Ok(total)
}

fn occupancy(facility: &FacilityRow, schema_version: i64) -> i64 {
    let mut count = facility.active_agent_ids.len();
    if schema_version == 6 {
        count += facility.resource_active_agent_ids.len();
    }
    count as i64
}

fn list_repr(items: &[&str]) -> String {
    let mut sorted: Vec<&str> = items.to_vec();
    sorted.sort_unstable();
    format!("[{}]", sorted.iter().map(|item| py_repr(item)).collect::<Vec<_>>().join(", "))
}

/// How the checks place added toilets: whether the Scenario places installed facilities on its
/// floor plan (`installed_facility_placement_enabled`), and the floor plan of a world with its
/// overlay as it is (`Host::floor_plan` of that world).
pub struct Placement<'a> {
    pub enabled: bool,
    pub plan: &'a mut dyn FnMut(&World) -> Result<Arc<Geo>>,
}

// ---- validate_effect_program -------------------------------------------------------------------------

/// `validate_effect_program(world, program)`: the issues, in Python's order and texts, each once.
/// It changes nothing. `Unsupported` for a program the browser does not check (above).
pub fn validate(world: &World, program: &EffectProgram, placement: &mut Placement<'_>) -> Result<Vec<String>> {
    for effect in &program.effects {
        if primitive(&effect.op).is_none() && crate::rules::effects::OPERATORS.contains(&&*effect.op) {
            return Err(Error::unsupported("an operator the offline reading of a text never makes"));
        }
    }
    let rows = world.rows()?;
    if let Some(taint) = rows.tainted().or(rows.zones_tainted()) {
        return Err(Error::unsupported(format!("the world's tables are tainted: {taint}")));
    }
    let minute = world.minute_index()?;
    let duration = world.duration_minutes()?;
    let schema_version = world.schema_version()?;
    let configs = configs(world)?;
    let events = pending(world)?;
    let mut issues: Vec<String> = Vec::new();

    let mode = &*program.support_mode;
    if matches!(mode, "clarification_required" | "not_simulatable") {
        issues.push(format!("program support mode {} cannot be executed", py_repr(mode)));
    }
    if mode == "approximate" && program.assumptions.is_empty() {
        issues.push("approximate programs must include explicit assumptions".to_owned());
    }
    if program.simulation_start_minute < minute {
        issues.push("simulation_start_minute is before the current branch minute".to_owned());
    }
    if has(world, EFFECT_PROGRAM)? {
        let slot = world.slots.get(EFFECT_PROGRAM).ok_or_else(|| Error::Runtime("a slot left the world".to_owned()))?.try_borrow()?;
        let scheduled = match slot.row::<EffectProgramRow>() {
            Ok(row) => row.programs.contains_key(&*program.program_id),
            Err(_) => return Err(Error::unsupported("effect_program_v1 is not held as its row")),
        };
        if scheduled {
            issues.push(format!("program_id {} was already scheduled on this branch", py_repr(&program.program_id)));
        }
    }
    let expected_events: usize = program.effects.iter().map(|effect| if effect.end_minute.is_some() { 2 } else { 1 }).sum();
    if expected_events > MAX_SCHEDULED_EVENTS {
        issues.push("program exceeds the maximum scheduled effect event count".to_owned());
    }
    if events.len() + expected_events > MAX_QUEUE {
        issues.push("branch event queue exceeds the bounded 10000-event limit".to_owned());
    }
    if program.simulation_start_minute >= duration {
        issues.push("simulation_start_minute is outside the scenario duration".to_owned());
    }

    // The people's capacity of the temporary spaces, active and pending, by zone.
    let mut temporary: IndexMap<Arc<str>, i64> = IndexMap::new();
    if let Some(V::Map(state)) = value(world, overlay::SPACES_KEY)? {
        if let Some(spaces) = state.get("spaces") {
            let V::Map(spaces) = spaces else { return Err(Error::unsupported("the temporary spaces are not a dict")) };
            for space in spaces.values() {
                let V::Map(space) = space else { return Err(Error::unsupported("a temporary space is not a dict")) };
                if space.get("active").is_some_and(V::is_truthy) {
                    let (Some(V::Str(zone)), Some(V::Int(capacity))) = (space.get("zone_id"), space.get("people_capacity")) else {
                        return Err(Error::unsupported("a temporary space without its zone or capacity"));
                    };
                    *temporary.entry(zone.clone()).or_default() += capacity;
                }
            }
        }
    }
    for event in &events {
        if &*event.event_type != "effect_program_temporary_space" {
            continue;
        }
        if let (Some(V::Str(zone)), Some(V::Map(parameters))) = (payload(event, "zone_id"), payload(event, "parameters")) {
            if let Some(capacity) = space_capacity(parameters) {
                *temporary.entry(zone.clone()).or_default() += capacity;
            }
        }
    }

    let zone_known = |id: &str| rows.zones.contains_key(id);
    let mut facility_intervals: IndexMap<Arc<str>, Vec<(i64, i64, i64, Arc<str>)>> = IndexMap::new();
    let (mut added_staff, mut added_facilities, mut added_evacuees) = (0i64, 0i64, 0i64);
    let mut toilet_additions: Vec<(String, Arc<str>, i64)> = Vec::new();
    for effect in &program.effects {
        let label = format!("effect {}", py_repr(&effect.effect_id));
        let Some(primitive) = primitive(&effect.op) else {
            issues.push(format!("{label} uses unsupported operator {}", py_repr(&effect.op)));
            continue;
        };
        let kind = &*effect.target_selector.kind;
        if !primitive.target_kinds.contains(&kind) {
            issues.push(format!("{label} target kind must be one of {}", list_repr(primitive.target_kinds)));
        }
        if !primitive.units.contains(&&*effect.units) {
            issues.push(format!("{label} has unsupported units {}", py_repr(&effect.units)));
        }
        let dependencies: BTreeSet<&str> = effect.policy_dependencies.iter().map(|id| &**id).collect();
        if dependencies != primitive.policies.iter().copied().collect() {
            issues.push(format!("{label} policy_dependencies must be exactly {}", list_repr(primitive.policies)));
        }
        let parameters = parameters(effect)?;
        let target = match (&effect.target_selector.target_id, effect.target_selector.candidate_ids.is_empty()) {
            (Some(target), true) => target.clone(),
            _ => {
                issues.push(format!("{label} requires one explicit target ID"));
                continue;
            }
        };
        if let Some(guard) = &effect.guard {
            let mut fields = BTreeSet::new();
            guard.fields(&mut fields);
            for field in fields {
                let shown = py_repr(&field);
                if field.starts_with("target.") && kind != "facility" {
                    issues.push(format!("{label} guard field {shown} requires a facility target"));
                } else if &*field == "zone.occupancy" && !matches!(kind, "facility" | "zone") {
                    issues.push(format!("{label} guard field {shown} needs a zone-scoped target"));
                } else if &*field == "resource.available_quantity" {
                    issues.push(format!("{label} guard field {shown} requires a resource delivery target"));
                } else if &*field == "staff.available" {
                    issues.push(format!("{label} guard field {shown} requires a staff availability target"));
                }
            }
        }
        if effect.start_minute < minute || effect.start_minute >= duration {
            issues.push(format!("{label} start_minute must be within the remaining scenario"));
        }
        match effect.end_minute {
            Some(end) => {
                if end <= effect.start_minute || end > duration {
                    issues.push(format!("{label} end_minute is outside its valid time window"));
                }
                if end - effect.start_minute > MAX_EFFECT_DURATION {
                    issues.push(format!("{label} duration exceeds the 366-day limit"));
                }
            }
            None if duration - effect.start_minute > MAX_EFFECT_DURATION => issues.push(format!("{label} duration exceeds the 366-day limit")),
            None => {}
        }
        if !primitive.timed && effect.end_minute.is_some() {
            issues.push(format!("{label} is a one-shot operation and cannot have end_minute"));
        }
        if primitive.policies.contains(&"operational_events_v1") && !has(world, "operational_events_v1")? {
            issues.push(format!("{label} requires operational_events_v1 state"));
        }
        if primitive.policies.contains(&"resource_service_v1") && (schema_version != 6 || !has(world, "resource_service_v1")?) {
            issues.push(format!("{label} requires the WorldState v6 resource-service policy"));
        }
        if primitive.policies.contains(&"population_dynamics_v1") && !has(world, "population_dynamics_v1")? {
            issues.push(format!("{label} requires population_dynamics_v1 state"));
        }
        if matches!(&*effect.op, "add_staff" | "add_toilet" | "add_evacuees" | "depart_evacuees") && !zone_known(&target) {
            issues.push(format!("{label} references unknown zone {}", py_repr(&target)));
        }
        if &*effect.op == "add_evacuees" {
            let admission = &parameters.admission_zone_ids;
            if admission.contains(&target) || admission.iter().any(|zone| !zone_known(zone)) {
                issues.push(format!("{label} alternate admission zones must be known and exclude its preferred zone"));
            }
            let overlay_schema = match configs.get("geometry_overlay") {
                Some(V::Map(config)) => match config.get("schema_version") {
                    Some(V::Int(version)) => Some(*version),
                    _ => None,
                },
                _ => None,
            };
            if !admission.is_empty() && !matches!(overlay_schema, Some(3 | 4)) {
                issues.push(format!("{label} alternate physical room admission requires geometry_overlay schema_version=3 or 4"));
            }
            if let Some(route_id) = &parameters.approach_route_id {
                if !admission.is_empty() {
                    issues.push(format!("{label} cannot combine one approach route with alternate admission zones"));
                }
                if let Some(problem) = approach_problem(&configs, route_id, &target)? {
                    issues.push(format!("{label} arrival approach is invalid: {problem}"));
                }
            }
        }
        match &*effect.op {
            "add_staff" => added_staff += parameters.count,
            "add_toilet" => {
                added_facilities += parameters.count;
                toilet_additions.push((label.clone(), target.clone(), parameters.count));
            }
            "add_evacuees" => added_evacuees += parameters.count,
            _ => {}
        }
        if &*effect.op == "facility_outage" {
            if !rows.facilities.contains_key(&target) {
                issues.push(format!("{label} references unknown facility {}", py_repr(&target)));
                continue;
            }
            facility_intervals.entry(target.clone()).or_default().push((
                effect.start_minute,
                effect.end_minute.unwrap_or(duration),
                parameters.units,
                effect.effect_id.clone(),
            ));
        } else if &*effect.op == "water_outage" {
            if effect.end_minute.is_none() {
                issues.push(format!("{label} requires end_minute so water service can be restored"));
            }
            if kind == "facility" {
                if !rows.facilities.get(&target).is_some_and(|facility| &*facility.kind == "water_station") {
                    issues.push(format!("{label} facility must be an existing water_station"));
                }
            } else if !zone_known(&target) || !rows.facilities.iter().any(|(_, facility)| &*facility.kind == "water_station" && facility.zone_id == target) {
                issues.push(format!("{label} zone must contain an existing water_station"));
            }
        }
    }

    for (facility_id, intervals) in &mut facility_intervals {
        intervals.sort_by(|first, second| first.0.cmp(&second.0).then_with(|| first.3.as_bytes().cmp(second.3.as_bytes())));
        let facility = rows.facilities.get(facility_id).ok_or_else(|| Error::Runtime("a facility left the table".to_owned()))?;
        let mut active: Vec<(i64, i64)> = Vec::new();
        for (start, end, units, effect_id) in intervals.iter() {
            active.retain(|(entry_end, _)| entry_end > start);
            let current = active.iter().map(|(_, units)| *units).fold(facility.working_units, i64::min);
            let target = current - units;
            if target < occupancy(facility, schema_version) {
                issues.push(format!("facility modifier {} would undercut active occupancy", py_repr(effect_id)));
            }
            if target > current {
                issues.push(format!("facility modifier {} conflicts with the restrictive stacking rule", py_repr(effect_id)));
            }
            active.push((*end, target));
        }
        for event in &events {
            if matches!(&*event.event_type, "facility_outage" | "facility_recovery" | "effect_program_apply" | "effect_program_expire")
                && matches!(payload(event, "facility_id"), Some(V::Str(id)) if id == facility_id)
                && intervals.iter().any(|(start, end, _, _)| *start <= event.due_minute && event.due_minute < *end)
            {
                issues.push(format!("pending {} conflicts with a new EffectProgram facility modifier", event.event_type));
            }
        }
    }

    if let Some(config) = configs.get("zone_environment") {
        environment_limits(world, config, &events, &zone_known, minute, duration, &mut issues)?;
    }

    let staff_total = rows.staff.len() as i64 + pending_count(&events, "effect_program_structure", "add_staff")? + added_staff;
    if staff_total > 100 {
        issues.push("program exceeds the 100-staff runtime bound".to_owned());
    }
    if temporary.values().any(|capacity| *capacity > 1000) {
        issues.push("temporary person capacity exceeds the 1000-person per-zone bound".to_owned());
    }
    let facility_total = rows.facilities.len() as i64 + pending_count(&events, "effect_program_structure", "add_toilet")? + added_facilities;
    if facility_total > 200 {
        issues.push("program exceeds the 200-facility runtime bound".to_owned());
    }
    drop(rows);
    if !toilet_additions.is_empty() && configs.contains_key("facility_geometry") {
        issues.extend(toilet_placement(world, &toilet_additions, placement)?);
    }
    if let Some(V::Map(population)) = value(world, "population_dynamics_v1")? {
        let mut known: BTreeSet<Arc<str>> = BTreeSet::new();
        match population.get("initial_agent_ids") {
            None => {}
            Some(V::List(items)) => {
                for item in items {
                    let V::Str(id) = item else { return Err(Error::unsupported("an initial agent ID that is not a string")) };
                    known.insert(id.clone());
                }
            }
            Some(_) => return Err(Error::unsupported("the initial agent IDs are not a list")),
        }
        match population.get("profile_traits") {
            None => {}
            Some(V::Map(traits)) => known.extend(traits.keys().cloned()),
            Some(_) => return Err(Error::unsupported("the profile traits are not a dict")),
        }
        let arriving = pending_count(&events, "effect_program_population", "add_evacuees")?;
        if known.len() as i64 + arriving + added_evacuees > 10_000 {
            issues.push("program exceeds the 10000-profile population runtime bound".to_owned());
        }
    }
    let mut seen = BTreeSet::new();
    issues.retain(|issue| seen.insert(issue.clone()));
    Ok(issues)
}

/// `_TemporarySpaceParameters.model_validate(parameters).people_capacity`, None where it refuses.
fn space_capacity(parameters: &Map) -> Option<i64> {
    if parameters.keys().any(|key| !matches!(&**key, "people_capacity" | "sleeping_capacity")) {
        return None;
    }
    let people = match parameters.get("people_capacity") {
        Some(V::Int(people)) if (1..=1000).contains(people) => *people,
        _ => return None,
    };
    let sleeping = match parameters.get("sleeping_capacity") {
        None => 0,
        Some(V::Int(sleeping)) if (0..=1000).contains(sleeping) => *sleeping,
        _ => return None,
    };
    (sleeping <= people).then_some(people)
}

/// The arrival approach's problem, as `ValueError`'s text, or None: the route must be one of the
/// approved geometry's, into the selected zone. The geometry a world holds is one its Scenario
/// validated (`parsed_facility_geometry` gives it back).
fn approach_problem(configs: &Map, route_id: &str, zone_id: &str) -> Result<Option<String>> {
    let Some(V::Map(geometry)) = configs.get("facility_geometry") else {
        return Ok(Some("arrival approach requires approved physical Geometry".to_owned()));
    };
    let routes = match geometry.get("arrival_approach_routes") {
        None => &[][..],
        Some(V::List(routes)) => &routes[..],
        Some(_) => return Err(Error::unsupported("the approach routes are not a list")),
    };
    for route in routes {
        let V::Map(route) = route else { return Err(Error::unsupported("an approach route is not a dict")) };
        if matches!(route.get("route_id"), Some(V::Str(id)) if &**id == route_id) {
            let to_zone = matches!(route.get("destination_zone_id"), Some(V::Str(id)) if &**id == zone_id);
            return Ok((!to_zone).then(|| "arrival approach destination must match the selected zone".to_owned()));
        }
    }
    Ok(Some(format!("unknown arrival approach route {}", py_repr(route_id))))
}

/// The limit of active environment modifiers in each zone, over the modifiers active now, those
/// pending, and the program's (none of the six operators adds one).
fn environment_limits(world: &World, config: &V, events: &[Arc<EventRow>], zone_known: &dyn Fn(&str) -> bool, minute: i64, duration: i64, issues: &mut Vec<String>) -> Result<()> {
    let limit = match config {
        V::Map(config) => match config.get("max_active_modifiers_per_zone") {
            Some(V::Int(limit)) => *limit,
            _ => return Err(Error::unsupported("the zone environment config has no int limit")),
        },
        _ => return Err(Error::unsupported("the zone environment config is not a dict")),
    };
    let Some(V::Map(state)) = value(world, crate::rules::effects::ENVIRONMENT)? else {
        return Err(Error::unsupported("the zone environment state is missing"));
    };
    let Some(V::Map(modifiers)) = state.get("modifiers") else {
        return Err(Error::unsupported("the zone environment modifiers are not a dict"));
    };
    let mut intervals: IndexMap<Arc<str>, Vec<(i64, i64)>> = IndexMap::new();
    for record in modifiers.values() {
        let V::Map(record) = record else { return Err(Error::unsupported("a modifier is not a dict")) };
        let (Some(V::Str(zone)), Some(V::Int(end))) = (record.get("zone_id"), record.get("end_minute")) else {
            return Err(Error::unsupported("a modifier without its zone or int end"));
        };
        intervals.entry(zone.clone()).or_default().push((minute, *end));
    }
    for event in events {
        if &*event.event_type != "effect_program_environment" {
            continue;
        }
        let (Some(V::Str(modifier)), Some(V::Str(zone)), Some(V::Int(end))) = (payload(event, "modifier_id"), payload(event, "zone_id"), payload(event, "end_minute")) else {
            continue;
        };
        if !modifiers.contains_key(modifier) && zone_known(zone) && event.due_minute < *end && *end < duration {
            intervals.entry(zone.clone()).or_default().push((event.due_minute, *end));
        }
    }
    for (zone, intervals) in &intervals {
        let boundaries: BTreeSet<i64> = intervals.iter().flat_map(|(start, end)| [*start, *end]).collect();
        let boundaries: Vec<i64> = boundaries.into_iter().collect();
        let over = boundaries[..boundaries.len().saturating_sub(1)]
            .iter()
            .any(|at| intervals.iter().filter(|(start, end)| start <= at && at < end).count() as i64 > limit);
        if over {
            issues.push(format!("zone {} exceeds the active environment modifier limit", py_repr(zone)));
        }
    }
    Ok(())
}

/// `_toilet_placement_issues`: each added toilet gets a free point on the floor plan of a copy of
/// the world, the toilets before it placed there.
fn toilet_placement(world: &World, additions: &[(String, Arc<str>, i64)], placement: &mut Placement<'_>) -> Result<Vec<String>> {
    if !placement.enabled {
        return Ok(additions
            .iter()
            .map(|(label, _, _)| format!("{label} cannot place added toilets on this floor plan; it needs geometry_overlay schema_version=4 (create the Scenario again)"))
            .collect());
    }
    let candidate = world.fork()?;
    let minute = candidate.minute_index()?;
    for (label, zone_id, count) in additions {
        if !candidate.rows()?.zones.contains_key(zone_id) {
            continue;
        }
        for index in 0..*count {
            let facility_id: Arc<str> = Arc::from(format!("placement-check-{}-{index}", candidate.rows()?.facilities.len()));
            candidate.rows_mut()?.facilities.insert(
                facility_id.clone(),
                FacilityRow {
                    id: facility_id.clone(),
                    kind: Arc::from("toilet"),
                    zone_id: zone_id.clone(),
                    capacity: 1,
                    working_units: 1,
                    active_agent_ids: Vec::new(),
                    queue_agent_ids: Vec::new(),
                    resource_active_agent_ids: Vec::new(),
                    resource_queue_agent_ids: Vec::new(),
                },
            );
            let plan = (placement.plan)(&candidate)?;
            let Some(point) = overlay::installed_point(&candidate, &Held::Held, Some(&plan), &facility_id)? else {
                return Ok(vec![format!("{label} has no free floor-plan point for toilet {} in zone {}", index + 1, py_repr(zone_id))]);
            };
            let mut state = candidate.row_mut::<ValueRow>(overlay::OVERLAY_KEY)?;
            let V::Map(state) = &mut state.0 else { return Err(Error::unsupported("the overlay is not a dict")) };
            let Some(V::Map(points)) = state.get_mut("facility_points") else {
                return Err(Error::unsupported("the overlay's facility points are not a dict"));
            };
            points.insert(
                facility_id.clone(),
                object([
                    ("point_m", object([("x", V::Float(point.x)), ("y", V::Float(point.y))])),
                    ("event_id", V::Str(Arc::from("placement-check"))),
                    ("source_ids", V::List(Vec::new())),
                    ("minute", V::Int(minute)),
                ]),
            );
        }
    }
    Ok(Vec::new())
}
