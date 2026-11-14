//! The offline intervention compiler (Issue #303): what `compile_natural_language`
//! (src/sheltersim/llm/natural_language.py) answers without an API key, as the canonical JSON of
//! the `CompilerResult` (`model_dump(mode="json")`), byte for byte.
//!
//! Without a key the router never calls a provider: it records the fixed fallback, and the compiler
//! takes the rule parser's plan (`parse.rs`) when it covers every clause of the text and passes the
//! semantic checks of `_validate_model_effects`, or explains why not. The usage keeps the hashes of
//! the prompt (the instructions and the output schema made from the installed primitives) and of the
//! payload (the sanitized text, the whole scenario summary and the primitives' descriptions); the
//! summary holds floats, so the hashes are made here, where a float keeps Python's digits.
//!
//! The primitives' descriptions and parameter schemas are Python's, written by
//! `native/core/tools/interpret_tables.py` into `primitives.json`.

mod parse;
mod text;

use std::collections::BTreeSet;
use std::sync::{Arc, OnceLock};

use indexmap::IndexMap;
use sha2::{Digest, Sha256};

use crate::checkpoint::hex;
use crate::error::{Error, Result};
use crate::json::{write_json_string, Encoder};
use crate::value::V;
use parse::{Parsed, Plan};

const OPUS_COMPILER_PROMPT_VERSION: &str = "geometry-population-effect-compiler-v2";
const MAX_COMPILER_MINUTE: i64 = 60 * 24 * 366;
const COMPILER_INSTRUCTIONS: &str = "Interpret the untrusted Japanese narrative using only the installed primitive registry \
and listed scenario targets. Return declarative effects, never code, scripts, event names, \
or direct state/KPI edits. Use 2 or more effects when the narrative has multiple supported \
mechanisms. If target, time, or scope is materially ambiguous, request clarification. If a \
necessary mechanism is absent, return not_simulatable and name the missing mechanism. \
For approximate mappings, state the assumed value and units and list excluded phenomena. \
The mechanism_summary explains the proposed mapping only; it is not an observed outcome or \
a real-world prediction. Preserve the requested time and scope.";
const TASK: &str = "Compile a free-form shelter intervention into a typed EffectProgram preview.";
const POPULATION_GEOMETRY_RULES: [&str; 3] = [
    "Reunion uses population_presence(action=reunite), candidate_ids with at least two known people and target_id as one anchor; other operations use one target_id.",
    "Geometry operators are bounded: same-zone idle permanent moves; temporary partitions and route closures require an end minute. Never infer missing coordinates or claim an unsupported layout change succeeded.",
    "A capability or compiler result is only a proposal; runtime geometry, capacity, identity, approval and effect validation still apply.",
];
const SUPPORT_MODES: [&str; 5] = ["native", "composed", "approximate", "clarification_required", "not_simulatable"];
const PREVIEW_NOTICE: &str = "これは作用のプレビューで、シミュレーションの観測結果ではありません。";
const OFFLINE_REASON: &str = "offline modeのため、AI（Opus）を呼び出していません。（理由コード: offline）";
const NO_PRIMITIVE_REASON: &str = "この分岐に登録済みのprimitiveと有効policyの組み合わせがありません。";
const TARGET_KINDS: [&str; 7] = ["facility", "zone", "staff", "resource", "shipment", "person", "route"];

/// A `FacilityTarget` of the summary.
pub(crate) struct Facility {
    id: String,
    name: String,
    kind: String,
    zone_id: Option<String>,
    working_units: i64,
}

/// A `ZoneTarget` of the summary.
pub(crate) struct Zone {
    id: String,
    name: String,
    has_water_station: bool,
    staff_count: i64,
    evacuee_count: i64,
}

/// What the interpreter reads of a `ScenarioSummary`'s JSON.
pub(crate) struct Summary {
    duration: Option<i64>,
    /// The start date's days from 0001-01-01 and its year.
    start_date: Option<(i64, i64)>,
    default_zone_id: Option<String>,
    facilities: Vec<Facility>,
    zones: Vec<Zone>,
    staff_count: usize,
    available_policies: Vec<String>,
    target_ids: Vec<(&'static str, Vec<String>)>,
}

/// One primitive of the registry (`available_primitives()`).
struct Primitive {
    op: String,
    target_kinds: Vec<String>,
    units: Vec<String>,
    required_policies: Vec<String>,
    timed: bool,
    description: V,
}

fn bad(what: &str) -> Error {
    Error::unsupported(format!("interpreter input: {what}"))
}

/// Python raised a ValueError (pydantic's ValidationError): the route answers 400.
fn refused(what: &str) -> Error {
    Error::Value(format!("invalid intervention request: {what}"))
}

fn field<'v>(map: &'v IndexMap<Arc<str>, V>, key: &str) -> Option<&'v V> {
    map.get(key)
}

fn string(value: Option<&V>, what: &str) -> Result<String> {
    match value {
        Some(V::Str(text)) => Ok(text.to_string()),
        _ => Err(bad(what)),
    }
}

fn optional_string(value: Option<&V>, what: &str) -> Result<Option<String>> {
    match value {
        None | Some(V::Null) => Ok(None),
        Some(V::Str(text)) => Ok(Some(text.to_string())),
        _ => Err(bad(what)),
    }
}

fn integer(value: Option<&V>, what: &str) -> Result<i64> {
    match value {
        Some(V::Int(number)) => Ok(*number),
        None => Ok(0),
        _ => Err(bad(what)),
    }
}

fn strings(value: Option<&V>, what: &str) -> Result<Vec<String>> {
    match value {
        None => Ok(Vec::new()),
        Some(V::List(items)) => items.iter().map(|item| string(Some(item), what)).collect(),
        _ => Err(bad(what)),
    }
}

fn map_of<'v>(value: &'v V, what: &str) -> Result<&'v IndexMap<Arc<str>, V>> {
    match value {
        V::Map(map) => Ok(map),
        _ => Err(bad(what)),
    }
}

fn list_of<'v>(value: Option<&'v V>, what: &str) -> Result<&'v [V]> {
    match value {
        None => Ok(&[]),
        Some(V::List(items)) => Ok(items),
        _ => Err(bad(what)),
    }
}

/// `^[A-Za-z0-9][A-Za-z0-9._:-]*$`, at most 64 characters: pydantic's `StableId`.
fn stable_id(text: &str) -> bool {
    let mut chars = text.chars();
    text.len() <= 64
        && chars.next().is_some_and(|char| char.is_ascii_alphanumeric())
        && chars.all(|char| char.is_ascii_alphanumeric() || "._:-".contains(char))
}

impl Summary {
    fn read(value: &V) -> Result<Summary> {
        let map = map_of(value, "scenario_summary")?;
        let duration = match field(map, "duration_minutes") {
            None | Some(V::Null) => None,
            Some(V::Int(number)) => Some(*number),
            _ => return Err(bad("duration_minutes")),
        };
        let start_date = match optional_string(field(map, "start_date"), "start_date")? {
            None => None,
            Some(text) => {
                let parts: Vec<&str> = text.split('-').collect();
                let number = |index: usize| parts.get(index).and_then(|part| part.parse::<i64>().ok());
                let (Some(year), Some(month), Some(day)) = (number(0), number(1), number(2)) else {
                    return Err(bad("start_date"));
                };
                Some((parse::ordinal(year, month, day).ok_or_else(|| bad("start_date"))?, year))
            }
        };
        let mut facilities = Vec::new();
        for item in list_of(field(map, "facilities"), "facilities")? {
            let item = map_of(item, "facility")?;
            facilities.push(Facility {
                id: string(field(item, "id"), "facility id")?,
                name: string(field(item, "name"), "facility name")?,
                kind: string(field(item, "kind"), "facility kind")?,
                zone_id: optional_string(field(item, "zone_id"), "facility zone_id")?,
                working_units: integer(field(item, "working_units"), "working_units")?,
            });
        }
        let mut zones = Vec::new();
        for item in list_of(field(map, "zones"), "zones")? {
            let item = map_of(item, "zone")?;
            zones.push(Zone {
                id: string(field(item, "id"), "zone id")?,
                name: string(field(item, "name"), "zone name")?,
                has_water_station: matches!(field(item, "has_water_station"), Some(V::Bool(true))),
                staff_count: integer(field(item, "staff_count"), "staff_count")?,
                evacuee_count: integer(field(item, "evacuee_count"), "evacuee_count")?,
            });
        }
        let mut shipments = Vec::new();
        for item in list_of(field(map, "pending_shipments"), "pending_shipments")? {
            shipments.push(string(field(map_of(item, "shipment")?, "id"), "shipment id")?);
        }
        let target_ids = vec![
            ("facility", facilities.iter().map(|item| item.id.clone()).collect()),
            ("zone", zones.iter().map(|item| item.id.clone()).collect()),
            ("staff", strings(field(map, "available_staff_ids"), "available_staff_ids")?),
            ("resource", strings(field(map, "resource_ids"), "resource_ids")?),
            ("shipment", shipments),
            ("person", strings(field(map, "person_ids"), "person_ids")?),
            ("route", strings(field(map, "route_ids"), "route_ids")?),
        ];
        Ok(Summary {
            duration,
            start_date,
            default_zone_id: optional_string(field(map, "default_zone_id"), "default_zone_id")?,
            facilities,
            zones,
            staff_count: strings(field(map, "staff_ids"), "staff_ids")?.len(),
            available_policies: strings(field(map, "available_policies"), "available_policies")?,
            target_ids,
        })
    }

    fn ids(&self, kind: &str) -> &[String] {
        self.target_ids.iter().find(|(name, _)| *name == kind).map(|(_, ids)| ids.as_slice()).unwrap_or(&[])
    }
}

fn primitives() -> Result<&'static [Primitive]> {
    static TABLE: OnceLock<std::result::Result<Vec<Primitive>, String>> = OnceLock::new();
    TABLE
        .get_or_init(|| read_primitives().map_err(|error| error.to_string()))
        .as_deref()
        .map_err(|error| Error::unsupported(error.clone()))
}

fn read_primitives() -> Result<Vec<Primitive>> {
    let json: serde_json::Value =
        serde_json::from_str(include_str!("primitives.json")).map_err(|_| bad("primitives.json"))?;
    let value = V::from_json(&json).ok_or_else(|| bad("primitives.json"))?;
    let V::List(items) = &value else { return Err(bad("primitives.json")) };
    items
        .iter()
        .map(|item| {
            let map = map_of(item, "primitive")?;
            Ok(Primitive {
                op: string(field(map, "op"), "op")?,
                target_kinds: strings(field(map, "target_kinds"), "target_kinds")?,
                units: strings(field(map, "units"), "units")?,
                required_policies: strings(field(map, "required_policies"), "required_policies")?,
                timed: matches!(field(map, "timed"), Some(V::Bool(true))),
                description: item.clone(),
            })
        })
        .collect()
}

fn sha256_text(text: &[u8]) -> String {
    hex(&Sha256::digest(text))
}

fn canonical(value: &V) -> Result<Vec<u8>> {
    let mut encoder = Encoder::with_capacity(4096);
    value.write_json(&mut encoder)?;
    Ok(encoder.out)
}

fn s(text: &str) -> V {
    V::Str(Arc::from(text))
}

fn strs<S: AsRef<str>>(items: &[S]) -> V {
    V::List(items.iter().map(|item| s(item.as_ref())).collect())
}

fn obj<const N: usize>(entries: [(&str, V); N]) -> V {
    V::Map(entries.into_iter().map(|(key, value)| (Arc::from(key), value)).collect())
}

fn opt(value: &Option<String>) -> V {
    value.as_deref().map(s).unwrap_or(V::Null)
}

/// `_inline_parameter_schema(value, definitions)`.
fn inline_schema(value: &V, definitions: &IndexMap<Arc<str>, V>, depth: usize) -> Result<V> {
    if depth > 24 {
        return Err(refused("recursive or excessively nested primitive schema"));
    }
    match value {
        V::List(items) => Ok(V::List(
            items.iter().map(|item| inline_schema(item, definitions, depth + 1)).collect::<Result<_>>()?,
        )),
        V::Map(map) => {
            if let Some(reference) = map.get("$ref") {
                let target = match reference {
                    V::Str(text) => text.strip_prefix("#/$defs/").and_then(|name| definitions.get(name)),
                    _ => None,
                };
                let Some(V::Map(target)) = target else {
                    return Err(refused("primitive schema has an unsupported external reference"));
                };
                let mut merged = target.clone();
                for (key, item) in map {
                    if &**key != "$ref" {
                        merged.insert(key.clone(), item.clone());
                    }
                }
                return inline_schema(&V::Map(merged), definitions, depth + 1);
            }
            Ok(V::Map(
                map.iter()
                    .map(|(key, item)| Ok((key.clone(), inline_schema(item, definitions, depth + 1)?)))
                    .collect::<Result<_>>()?,
            ))
        }
        _ => Ok(value.clone()),
    }
}

fn number(value: &V) -> Option<f64> {
    match value {
        V::Int(number) => Some(*number as f64),
        V::Float(number) => Some(*number),
        _ => None,
    }
}

/// `_merge_parameter_schema(properties, key, incoming)`.
fn merge_schema(properties: &mut IndexMap<Arc<str>, V>, key: &Arc<str>, incoming: V) {
    let Some(V::Map(current)) = properties.get_mut(key) else {
        properties.insert(key.clone(), incoming);
        return;
    };
    let V::Map(incoming) = incoming else { return };
    let (mine, theirs) = (current.get("type").cloned(), incoming.get("type").cloned());
    if mine != theirs {
        let numeric = |value: &Option<V>| matches!(value.as_ref(), Some(V::Str(text)) if &**text == "integer" || &**text == "number");
        let kind = if numeric(&mine) && numeric(&theirs) { "number" } else { "string" };
        current.insert(Arc::from("type"), s(kind));
        current.shift_remove("enum");
        return;
    }
    for bound in ["minimum", "maximum", "minLength", "maxLength"] {
        let Some(new) = incoming.get(bound) else { continue };
        let old = current.get(bound).cloned().unwrap_or_else(|| new.clone());
        let lower = bound == "minimum" || bound == "minLength";
        let (a, b) = (number(&old), number(new));
        let pick_new = match (a, b) {
            (Some(a), Some(b)) => if lower { b < a } else { b > a },
            _ => false,
        };
        current.insert(Arc::from(bound), if pick_new { new.clone() } else { old });
    }
    if let Some(V::List(values)) = incoming.get("enum") {
        let mut all: Vec<V> = match current.get("enum") {
            Some(V::List(old)) => old.clone(),
            _ => Vec::new(),
        };
        for value in values {
            if !all.contains(value) {
                all.push(value.clone());
            }
        }
        all.sort_by(|a, b| match (a, b) {
            (V::Str(a), V::Str(b)) => a.as_bytes().cmp(b.as_bytes()),
            _ => number(a).partial_cmp(&number(b)).unwrap_or(std::cmp::Ordering::Equal),
        });
        current.insert(Arc::from("enum"), V::List(all));
    }
}

/// `_compiler_output_schema(summary, target_ids, eligible)`.
fn output_schema(summary: &Summary, eligible: &[&Primitive]) -> Result<V> {
    let operator_names: Vec<&str> = eligible.iter().map(|item| item.op.as_str()).collect();
    let target_kinds: BTreeSet<&str> = eligible
        .iter()
        .flat_map(|item| item.target_kinds.iter())
        .map(String::as_str)
        .filter(|kind| !summary.ids(kind).is_empty())
        .collect();
    let valid_target_ids: BTreeSet<&str> =
        target_kinds.iter().flat_map(|kind| summary.ids(kind).iter().map(String::as_str)).collect();
    let valid_target_ids: Vec<&str> = valid_target_ids.into_iter().collect();
    let target_kinds: Vec<&str> = target_kinds.into_iter().collect();
    let maximum_minute = summary.duration.map(|duration| duration - 1).unwrap_or(MAX_COMPILER_MINUTE - 1);
    let mut parameter_properties: IndexMap<Arc<str>, V> = IndexMap::new();
    for primitive in eligible {
        let V::Map(description) = &primitive.description else { continue };
        let Some(V::Map(model_schema)) = description.get("parameters_schema") else { continue };
        let empty = IndexMap::new();
        let definitions = match model_schema.get("$defs") {
            Some(V::Map(definitions)) => definitions,
            _ => &empty,
        };
        if let Some(V::Map(properties)) = model_schema.get("properties") {
            for (key, property) in properties {
                merge_schema(&mut parameter_properties, key, inline_schema(property, definitions, 0)?);
            }
        }
    }
    let ids_enum = || obj([("type", s("string")), ("enum", strs(&valid_target_ids))]);
    let effect_schema = obj([
        ("type", s("object")),
        (
            "properties",
            obj([
                ("op", obj([("type", s("string")), ("enum", strs(&operator_names))])),
                ("target_kind", obj([("type", s("string")), ("enum", strs(&target_kinds))])),
                ("target_id", ids_enum()),
                (
                    "candidate_ids",
                    obj([("type", s("array")), ("items", ids_enum()), ("minItems", V::Int(2)), ("maxItems", V::Int(100))]),
                ),
                (
                    "start_minute",
                    obj([("type", s("integer")), ("minimum", V::Int(0)), ("maximum", V::Int(maximum_minute))]),
                ),
                (
                    "end_minute",
                    obj([
                        ("type", s("integer")),
                        ("minimum", V::Int(0)),
                        ("maximum", V::Int(summary.duration.unwrap_or(MAX_COMPILER_MINUTE))),
                    ]),
                ),
                (
                    "parameters",
                    obj([
                        ("type", s("object")),
                        ("properties", V::Map(parameter_properties)),
                        ("required", V::List(Vec::new())),
                        ("additionalProperties", V::Bool(false)),
                    ]),
                ),
                (
                    "assumption_indexes",
                    obj([
                        ("type", s("array")),
                        ("items", obj([("type", s("integer")), ("minimum", V::Int(0)), ("maximum", V::Int(99))])),
                        ("maxItems", V::Int(50)),
                    ]),
                ),
            ]),
        ),
        ("required", strs(&["op", "target_kind", "target_id", "start_minute", "parameters"])),
        ("additionalProperties", V::Bool(false)),
    ]);
    let bounded_text = |maximum: i64| obj([("type", s("string")), ("maxLength", V::Int(maximum))]);
    let text_list = |maximum: i64| {
        obj([("type", s("array")), ("minItems", V::Int(0)), ("maxItems", V::Int(maximum)), ("items", bounded_text(500))])
    };
    Ok(obj([
        ("type", s("object")),
        (
            "properties",
            obj([
                (
                    "status",
                    obj([("type", s("string")), ("enum", strs(&["compiled", "clarification_required", "not_simulatable"]))]),
                ),
                ("support_mode", obj([("type", s("string")), ("enum", strs(&SUPPORT_MODES))])),
                ("mechanism_summary", bounded_text(500)),
                (
                    "effects",
                    obj([
                        ("type", s("array")),
                        ("minItems", V::Int(0)),
                        ("maxItems", V::Int(if eligible.is_empty() { 0 } else { 50 })),
                        ("items", effect_schema),
                    ]),
                ),
                (
                    "assumptions",
                    obj([
                        ("type", s("array")),
                        ("minItems", V::Int(0)),
                        ("maxItems", V::Int(100)),
                        (
                            "items",
                            obj([
                                ("type", s("object")),
                                (
                                    "properties",
                                    obj([
                                        ("description", bounded_text(500)),
                                        ("units", bounded_text(64)),
                                        ("value", bounded_text(120)),
                                    ]),
                                ),
                                ("required", strs(&["description", "units", "value"])),
                                ("additionalProperties", V::Bool(false)),
                            ]),
                        ),
                    ]),
                ),
                ("limitations", text_list(50)),
                ("questions", text_list(10)),
                (
                    "choices",
                    obj([("type", s("array")), ("minItems", V::Int(0)), ("maxItems", V::Int(100)), ("items", ids_enum())]),
                ),
            ]),
        ),
        (
            "required",
            strs(&["status", "support_mode", "mechanism_summary", "effects", "assumptions", "limitations", "questions", "choices"]),
        ),
        ("additionalProperties", V::Bool(false)),
    ]))
}

static EMAIL: text::Pattern = text::Pattern::new(r"\b[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}\b", true);
static PHONE: text::Pattern =
    text::Pattern::new(r"(?<!\w)(?:\+?81[- ]?)?0\d{1,4}[- ]?\d{1,4}[- ]?\d{3,4}(?!\w)", false);
static LONG_NUMBER: text::Pattern = text::Pattern::new(r"(?<!\w)\d{10,13}(?!\w)", false);

/// `_sanitize_narrative(text)`.
fn sanitize(raw: &str) -> Result<String> {
    let normalized: String = text::nfkc(raw)
        .chars()
        .map(|char| if char == '\n' || char == '\t' || char as u32 >= 32 { char } else { ' ' })
        .collect();
    let normalized = text::blank(EMAIL.get()?, &normalized, "[redacted-email]")?;
    let normalized = text::blank(PHONE.get()?, &normalized, "[redacted-phone]")?;
    let normalized = text::blank(LONG_NUMBER.get()?, &normalized, "[redacted-phone]")?;
    Ok(text::head(&normalized, 4000).to_owned())
}

/// The request: `ParseRequest` as pydantic reads it.
struct Request {
    text: String,
    branch_id: String,
    selected_minute: i64,
    summary: Summary,
    summary_value: V,
}

fn read_request(bytes: &[u8]) -> Result<Request> {
    let json: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| refused("not JSON"))?;
    let value = V::from_json(&json).ok_or_else(|| refused("a number Python does not read"))?;
    let V::Map(map) = value else { return Err(refused("not an object")) };
    if map.keys().any(|key| !["text", "branch_id", "selected_minute", "scenario_summary"].contains(&&**key)) {
        return Err(refused("extra field"));
    }
    let Some(V::Str(text)) = map.get("text") else { return Err(refused("text")) };
    if text.is_empty() || text::chars(text) > 4000 {
        return Err(refused("text length"));
    }
    let Some(V::Str(branch_id)) = map.get("branch_id") else { return Err(refused("branch_id")) };
    if !stable_id(branch_id) {
        return Err(refused("branch_id"));
    }
    let Some(V::Int(selected_minute)) = map.get("selected_minute") else { return Err(refused("selected_minute")) };
    if *selected_minute < 0 {
        return Err(refused("selected_minute"));
    }
    let mut summary_value = map.get("scenario_summary").cloned().ok_or_else(|| refused("scenario_summary"))?;
    summary_value.canonicalize();
    let summary = Summary::read(&summary_value)?;
    if summary.duration.is_some_and(|duration| *selected_minute >= duration) {
        return Err(refused("selected_minute must be before the scenario end"));
    }
    Ok(Request {
        text: text.to_string(),
        branch_id: branch_id.to_string(),
        selected_minute: *selected_minute,
        summary,
        summary_value,
    })
}

/// The semantic checks of `_validate_existing_plan` and `_validate_model_effects` on the parser's
/// plan: Err with the ValueError's text.
fn validate_plan(plan: &Plan, summary: &Summary, eligible: &[&Primitive]) -> std::result::Result<(), String> {
    let spec = |op: &str| eligible.iter().find(|item| item.op == op);
    for effect in &plan.effects {
        let Some(primitive) = spec(effect.op) else {
            return Err(format!("offline primitive {} is not installed", crate::text::py_repr(effect.op)));
        };
        let units_ok = match primitive.units.as_slice() {
            [only] => only == effect.units,
            _ => false,
        };
        if !units_ok {
            return Err(format!("offline effect {} has invalid units", crate::text::py_repr(&effect.effect_id)));
        }
        if effect.dependencies != primitive.required_policies.iter().map(String::as_str).collect::<Vec<_>>() {
            return Err(format!(
                "offline effect {} has invalid policy dependencies",
                crate::text::py_repr(&effect.effect_id)
            ));
        }
    }
    let effects = &plan.effects;
    if effects.len() > 50 || effects.iter().map(|effect| if effect.end.is_some() { 2 } else { 1 }).sum::<usize>() > 100 {
        return Err("program exceeds the bounded effect/event count".to_owned());
    }
    let mut staff_additions = 0;
    let mut toilet_additions = 0;
    for (index, effect) in effects.iter().enumerate() {
        let Some(primitive) = spec(effect.op) else {
            return Err(format!("effect {index} uses an unavailable primitive {}", crate::text::py_repr(effect.op)));
        };
        if !primitive.required_policies.iter().all(|policy| summary.available_policies.contains(policy)) {
            return Err(format!("effect {index} requires an unavailable policy"));
        }
        let kind = effect.selector.kind;
        let target = effect.selector.target_id.as_deref();
        if !primitive.target_kinds.iter().any(|item| item == kind)
            || !target.is_some_and(|id| summary.ids(kind).iter().any(|item| item == id))
        {
            return Err(format!("effect {index} references an unknown or incompatible target"));
        }
        let target = target.unwrap_or_default();
        if effect.start >= summary.duration.unwrap_or(MAX_COMPILER_MINUTE) {
            return Err(format!("effect {index} starts outside the scenario"));
        }
        if let Some(end) = effect.end {
            if end <= effect.start {
                return Err(format!("effect {index} has an invalid time window"));
            }
            if summary.duration.is_some_and(|duration| end > duration) {
                return Err(format!("effect {index} ends outside the scenario"));
            }
            if !primitive.timed {
                return Err(format!("effect {index} adds end_minute to a one-shot operation"));
            }
        }
        let value = match &effect.parameters.1 {
            V::Int(number) => *number,
            _ => 0,
        };
        match effect.op {
            "facility_outage" => {
                let Some(facility) = summary.facilities.iter().find(|item| item.id == target) else {
                    return Err(format!("effect {index} references an unknown facility"));
                };
                if value > facility.working_units {
                    return Err(format!("effect {index} outage exceeds working units"));
                }
            }
            "water_outage" => {
                if effect.end.is_none() {
                    return Err(format!("effect {index} requires an explicit water-restoration minute"));
                }
                if kind == "facility" {
                    if !summary.facilities.iter().any(|item| item.id == target && item.kind == "water_station") {
                        return Err(format!("effect {index} must target a water station"));
                    }
                } else if !summary.zones.iter().any(|zone| zone.id == target && zone.has_water_station) {
                    return Err(format!("effect {index} zone has no water station"));
                }
            }
            "add_staff" => staff_additions += value,
            "add_toilet" => toilet_additions += value,
            _ => {}
        }
    }
    if summary.staff_count as i64 + staff_additions > 100 {
        return Err("program exceeds the 100-staff runtime limit".to_owned());
    }
    if summary.facilities.len() as i64 + toilet_additions > 200 {
        return Err("program exceeds the 200-facility runtime limit".to_owned());
    }
    Ok(())
}

/// `DraftPlan.model_dump(mode="json")` of the parser's plan.
fn plan_value(plan: &Plan) -> V {
    let effects = plan
        .effects
        .iter()
        .map(|effect| {
            obj([
                ("effect_id", s(&effect.effect_id)),
                ("op", s(effect.op)),
                (
                    "target_selector",
                    obj([
                        ("kind", s(effect.selector.kind)),
                        ("target_id", opt(&effect.selector.target_id)),
                        ("candidate_ids", strs(&effect.selector.candidate_ids)),
                    ]),
                ),
                ("start_minute", V::Int(effect.start)),
                ("end_minute", effect.end.map(V::Int).unwrap_or(V::Null)),
                ("guard", V::Null),
                ("parameters", parse::parameters_value(&effect.parameters)),
                ("units", s(effect.units)),
                ("source_id", s(parse::PARSER_SOURCE_ID)),
                ("assumption_ids", strs(&effect.assumption_ids)),
                ("policy_dependencies", strs(&effect.dependencies)),
                ("causation_id", V::Null),
                ("reversible", V::Bool(true)),
            ])
        })
        .collect();
    let assumptions = plan
        .assumptions
        .iter()
        .map(|item| {
            obj([
                ("assumption_id", s(&item.id)),
                ("description", s(&item.description)),
                ("source_id", s(parse::PARSER_SOURCE_ID)),
                ("units", V::Null),
                ("value", V::Null),
            ])
        })
        .collect();
    let ambiguities = plan
        .ambiguities
        .iter()
        .map(|item| {
            obj([
                ("code", s("ambiguous_target")),
                ("field", s("target_selector")),
                ("message", s(item.message)),
                ("choices", strs(&item.choices)),
            ])
        })
        .collect();
    obj([
        ("schema_version", V::Int(1)),
        ("program_id", s(&plan.program_id)),
        ("narrative", s("Offline parser proposal; validate the selected mechanism and target before approval.")),
        ("source_text", s(&plan.source_text)),
        ("simulation_start_minute", V::Int(plan.fork_at_minute)),
        ("effects", V::List(effects)),
        ("assumptions", V::List(assumptions)),
        ("support_mode", s(plan.support_mode)),
        (
            "compiler_provenance",
            obj([
                ("compiler_kind", s("offline_rule")),
                ("compiler_version", s(parse::PARSER_VERSION)),
                ("model_id", V::Null),
                ("prompt_hash", V::Null),
            ]),
        ),
        ("branch_id", s(&plan.branch_id)),
        ("fork_at_minute", V::Int(plan.fork_at_minute)),
        ("ambiguities", V::List(ambiguities)),
        ("requires_confirmation", V::Bool(true)),
    ])
}

/// A `CompilerResult` dump.
struct Answer {
    status: &'static str,
    support_mode: &'static str,
    plan: Option<V>,
    mechanism_summary: &'static str,
    limitations: Vec<String>,
    questions: Vec<String>,
    choices: Vec<String>,
    fallback_used: bool,
}

/// `_with_fallback_reason(limitations, reason)`.
fn with_reason(mut limitations: Vec<String>, reason: &str) -> Vec<String> {
    if !reason.is_empty() {
        limitations.push(text::head(&format!("AI（Opus）の解釈を使わなかった理由: {reason}"), 500).to_owned());
    }
    limitations
}

/// `_offline_compiler_result(...)`.
fn offline_result(request: &Request, parsed: Parsed, eligible: &[&Primitive], fallback_used: bool, reason: &str) -> Result<Answer> {
    let mut reason = reason.to_owned();
    let issues = match parsed {
        Parsed::Parsed(plan) => {
            if parse::covers_all_clauses(&request.text)? {
                return Ok(match validate_plan(&plan, &request.summary, eligible) {
                    Ok(()) => Answer {
                        status: "compiled",
                        support_mode: plan.support_mode,
                        plan: Some(plan_value(&plan)),
                        mechanism_summary: "Opusが利用できないため、認識済みのオフライン規則で対象と作用を構成しました。",
                        limitations: with_reason(
                            vec!["これは規則ベースのfallbackで、自由文全体をOpusが解釈した結果ではありません。".to_owned()],
                            &reason,
                        ),
                        questions: Vec::new(),
                        choices: Vec::new(),
                        fallback_used: true,
                    },
                    Err(message) => Answer {
                        status: "not_simulatable",
                        support_mode: "not_simulatable",
                        plan: None,
                        mechanism_summary: "オフライン案は現在のscenario/policy条件を満たしません。",
                        limitations: vec![text::head(&format!("{reason}; offline proposal rejected: {message}"), 500).to_owned()],
                        questions: Vec::new(),
                        choices: Vec::new(),
                        fallback_used: true,
                    },
                });
            }
            reason = format!(
                "{reason}; offline fallback recognized only part of the request, so the partial plan was discarded"
            );
            Vec::new()
        }
        Parsed::Clarification(plan) => {
            let mut choices: Vec<String> = Vec::new();
            for choice in plan.ambiguities.iter().flat_map(|item| item.choices.iter()) {
                if !choices.contains(choice) {
                    choices.push(choice.clone());
                }
            }
            if choices.len() > 100 || plan.ambiguities.len() > 10 {
                return Err(refused("CompilerResult choices"));
            }
            return Ok(Answer {
                status: "clarification_required",
                support_mode: "clarification_required",
                plan: None,
                mechanism_summary: "オフライン規則では候補まで特定しました。対象を選択してください。",
                limitations: with_reason(vec!["対象が確定するまで実行可能な案として扱いません。".to_owned()], &reason),
                questions: plan.ambiguities.iter().map(|item| item.message.to_owned()).collect(),
                choices,
                fallback_used,
            });
        }
        Parsed::Rejected(issues) => issues,
    };
    let time_questions: Vec<String> =
        issues.iter().filter(|item| item.code == "ambiguous_time").map(|item| item.message.clone()).collect();
    if !time_questions.is_empty() && !eligible.is_empty() {
        return Ok(Answer {
            status: "clarification_required",
            support_mode: "clarification_required",
            plan: None,
            mechanism_summary: "時刻を一意に決められないため、作用案を作成していません。",
            limitations: with_reason(vec!["各イベントの時刻を明示するまで作用を適用できません。".to_owned()], &reason),
            questions: time_questions.into_iter().take(10).collect(),
            choices: vec!["HH:MM".to_owned()],
            fallback_used,
        });
    }
    let mut limitation = if reason.is_empty() {
        "この入力を現在のシミュレーション機構へ安全に対応付けられません。".to_owned()
    } else {
        reason
    };
    if !issues.is_empty() {
        let reasons: Vec<&str> = issues.iter().map(|item| item.message.as_str()).collect();
        limitation.push_str(" オフライン判定: ");
        limitation.push_str(&reasons.join(" / "));
    }
    Ok(Answer {
        status: "not_simulatable",
        support_mode: "not_simulatable",
        plan: None,
        mechanism_summary: "利用可能な作用へ変換できる案はありません。",
        limitations: vec![text::head(&limitation, 500).to_owned()],
        questions: Vec::new(),
        choices: Vec::new(),
        fallback_used,
    })
}

/// What `compile_natural_language(request, router)` answers without an API key: the canonical JSON
/// of the `CompilerResult`'s `model_dump(mode="json")`. The request is the JSON of a `ParseRequest`
/// (`text`, `branch_id`, `selected_minute` and `scenario_summary`, the summary's dump). `Error::Value`
/// is a request Python refuses with a ValueError (the route's 400); other errors are input that is
/// not of that form.
pub fn compile_offline(request: &[u8]) -> Result<Vec<u8>> {
    let request = read_request(request)?;
    let summary = &request.summary;
    let policies: BTreeSet<&str> = summary.available_policies.iter().map(String::as_str).collect();
    let eligible: Vec<&Primitive> = primitives()?
        .iter()
        .filter(|item| item.required_policies.iter().all(|policy| policies.contains(policy.as_str())))
        .filter(|item| item.target_kinds.iter().any(|kind| !summary.ids(kind).is_empty()))
        .collect();
    let schema = output_schema(summary, &eligible)?;
    let prompt_hash = sha256_text(&canonical(&obj([
        ("instructions", s(COMPILER_INSTRUCTIONS)),
        ("prompt_version", s(OPUS_COMPILER_PROMPT_VERSION)),
        ("output_schema", schema),
    ]))?);
    let target_ids = V::Map(
        TARGET_KINDS.iter().map(|kind| (Arc::from(*kind), strs(summary.ids(kind)))).collect(),
    );
    let payload = obj([
        ("task", s(TASK)),
        ("instructions", s(COMPILER_INSTRUCTIONS)),
        ("population_geometry_rules", strs(&POPULATION_GEOMETRY_RULES)),
        ("untrusted_narrative", s(&sanitize(&request.text)?)),
        (
            "branch",
            obj([("branch_id", s(&request.branch_id)), ("selected_minute", V::Int(request.selected_minute))]),
        ),
        ("scenario", request.summary_value.clone()),
        ("target_ids_by_kind", target_ids),
        ("installed_primitives", V::List(eligible.iter().map(|item| item.description.clone()).collect())),
    ]);
    let payload_text = canonical(&payload)?;
    let parsed = parse::parse(&request.text, &request.branch_id, request.selected_minute, summary)?;
    let (input_hash, fallback_used, reason) = if eligible.is_empty() {
        (sha256_text(&payload_text), false, NO_PRIMITIVE_REASON)
    } else {
        // The router's input hash: the sha256 of the input text written as a JSON string.
        let mut quoted = Vec::with_capacity(payload_text.len() + 16);
        write_json_string(&mut quoted, std::str::from_utf8(&payload_text).map_err(|_| bad("payload"))?);
        (sha256_text(&quoted), true, OFFLINE_REASON)
    };
    let answer = offline_result(&request, parsed, &eligible, fallback_used, reason)?;
    let usage = obj([
        ("model_id", V::Null),
        ("prompt_version", s(OPUS_COMPILER_PROMPT_VERSION)),
        ("prompt_hash", s(&prompt_hash)),
        ("input_hash", s(&input_hash)),
        ("api_request_count", V::Int(0)),
        ("input_tokens", V::Int(0)),
        ("output_tokens", V::Int(0)),
        ("estimated_cost_usd", V::Float(0.0)),
        ("cache_hit", V::Bool(false)),
        ("error_kind", V::Null),
    ]);
    let result = obj([
        ("status", s(answer.status)),
        ("support_mode", s(answer.support_mode)),
        ("draft_plan", answer.plan.unwrap_or(V::Null)),
        ("mechanism_summary", s(answer.mechanism_summary)),
        ("limitations", strs(&answer.limitations)),
        ("questions", strs(&answer.questions)),
        ("choices", strs(&answer.choices)),
        ("preview_notice", s(PREVIEW_NOTICE)),
        ("fallback_used", V::Bool(answer.fallback_used)),
        ("usage", usage),
    ]);
    canonical(&result)
}

// ---- What the routes around the compiler need (#306, #308) -------------------------------------

/// The canonical JSON of the object `base` with the fields of the object `extra` put in (an
/// answer the core wrote, with the route's fields added; a float keeps its digits).
pub fn merge_objects(base: &[u8], extra: &[u8]) -> Result<Vec<u8>> {
    let read = |bytes: &[u8]| -> Result<IndexMap<Arc<str>, V>> {
        let json: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| bad("not JSON"))?;
        match V::from_json(&json) {
            Some(V::Map(map)) => Ok(map),
            _ => Err(bad("not a JSON object")),
        }
    };
    let mut map = read(base)?;
    for (key, value) in read(extra)? {
        map.insert(key, value);
    }
    canonical(&V::Map(map))
}

static SECRET: text::Pattern = text::Pattern::new_raw(
    r"(?i:sk-(?:ant-)?[A-Za-z0-9_-]{12,}|Bearer[\s\x1c-\x1f]+[^\s\x1c-\x1f]+|(?:(?:(?<=[\p{L}\p{N}_])(?![\p{L}\p{N}_])|(?<![\p{L}\p{N}_])(?=[\p{L}\p{N}_]))[^\s\x1c-\x1f@]+@[^\s\x1c-\x1f@]+\.[\p{L}\p{Nl}\p{No}]{2,}(?:(?<=[\p{L}\p{N}_])(?![\p{L}\p{N}_])|(?<![\p{L}\p{N}_])(?=[\p{L}\p{N}_]))))",
);
static SECRET_WITHOUT_ADDRESS: text::Pattern =
    text::Pattern::new_raw(r"(?i:sk-(?:ant-)?[A-Za-z0-9_-]{12,}|Bearer[\s\x1c-\x1f]+[^\s\x1c-\x1f]+)");
/// `PHONE_NUMBER`: three separated digit groups, but not the three middle groups of a UUID (#321).
static PHONE_NUMBER: text::Pattern = text::Pattern::new(
    r"(?<!\w)(?!(?<=[0-9A-Fa-f]{8}-)\d{4}-\d{4}-\d{4}-[0-9A-Fa-f]{12}(?!\w))(?:\+\d{1,3}[ -])?\d{2,4}[- ]\d{2,4}[- ]\d{3,4}(?!\w)",
    false,
);

/// Whether `safe_document` (src/sheltersim/storage/projects.py `_redact`) changes the text: it
/// holds an API key, a bearer token, an e-mail address or a phone number.
pub fn redacts(value: &str) -> Result<bool> {
    let secret = if value.contains('@') {
        text::found(SECRET.get()?, value)?
    } else {
        let folded = crate::text::casefold(value);
        (folded.contains("sk-") || folded.contains("bearer")) && text::found(SECRET_WITHOUT_ADDRESS.get()?, value)?
    };
    Ok(secret || text::found(PHONE_NUMBER.get()?, value)?)
}

/// Whether `safe_document(document) != document` for a JSON document of strings, numbers and
/// containers whose keys are not private: some string of it is redacted.
pub fn document_redacts(document: &[u8]) -> Result<bool> {
    fn walk(value: &V) -> Result<bool> {
        Ok(match value {
            V::Str(text) => redacts(text)?,
            V::List(items) => {
                for item in items {
                    if walk(item)? {
                        return Ok(true);
                    }
                }
                false
            }
            V::Map(map) => {
                for item in map.values() {
                    if walk(item)? {
                        return Ok(true);
                    }
                }
                false
            }
            _ => false,
        })
    }
    let json: serde_json::Value = serde_json::from_slice(document).map_err(|_| bad("not JSON"))?;
    walk(&V::from_json(&json).ok_or_else(|| bad("a number Python does not read"))?)
}

static REDACT_ENTITY: text::Pattern = text::Pattern::new(
    r"\b(?:agent|person|staff|group|event|run|branch)[ _:#-]*[A-Za-z0-9_.:-]{1,80}\b|\b[A-Z]{1,8}[-_:]?\d{1,8}\b|[\x{4e00}-\x{9fff}]{1,4}(?:さん|様|氏)",
    true,
);

/// The question a result question stores (src/sheltersim/web/results_chat.py `_ask`):
/// `_REDACT_ENTITY.sub("[selected entity]", _sanitize_narrative(question)).strip()`. The route
/// then puts "[selected entity]" for the request's own IDs.
pub fn question_text(question: &str) -> Result<String> {
    let sanitized = sanitize(question)?;
    Ok(text::strip(&text::blank(REDACT_ENTITY.get()?, &sanitized, "[selected entity]")?).to_owned())
}

/// The `metrics` of a comparison's difference (src/sheltersim/web/replay.py
/// `comparison_projection`) of the frame `other` from the frame `base`, as canonical JSON: for each
/// KPI of `base` that is an int or a float in both, `other`'s minus `base`'s, an int when both are
/// ints, else a float, as Python subtracts them (#307).
pub fn kpi_differences(base: &[u8], other: &[u8]) -> Result<Vec<u8>> {
    let kpis = |bytes: &[u8]| -> Result<IndexMap<Arc<str>, V>> {
        let json: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| bad("not JSON"))?;
        match V::from_json(&json) {
            Some(V::Map(mut frame)) => match frame.shift_remove("kpis") {
                Some(V::Map(kpis)) => Ok(kpis),
                _ => Err(bad("a frame without kpis")),
            },
            _ => Err(bad("a frame that is not an object")),
        }
    };
    let (base, other) = (kpis(base)?, kpis(other)?);
    let mut metrics = IndexMap::new();
    for (key, value) in &base {
        let difference = match (other.get(key), value) {
            (Some(V::Int(a)), V::Int(b)) => V::Int(a.checked_sub(*b).ok_or_else(|| bad("a difference past i64"))?),
            (Some(V::Int(a)), V::Float(b)) => V::Float(*a as f64 - b),
            (Some(V::Float(a)), V::Int(b)) => V::Float(a - *b as f64),
            (Some(V::Float(a)), V::Float(b)) => V::Float(a - b),
            _ => continue,
        };
        metrics.insert(key.clone(), difference);
    }
    canonical(&V::Map(metrics))
}
