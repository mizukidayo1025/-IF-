//! `parse_natural_language` (src/sheltersim/llm/natural_language.py): the offline parser of a small,
//! explicit set of intervention requests, with Python's results (Issue #303). It reads the text and
//! the scenario summary only, and proposes a plan; it changes nothing.

use std::sync::Arc;

use indexmap::IndexMap;
use sha2::{Digest, Sha256};

use super::text::{self, Pattern};
use super::{Facility, Summary, Zone};
use crate::checkpoint::hex;
use crate::error::{Error, Result};
use crate::value::V;

pub(super) const PARSER_VERSION: &str = "h06-offline-v1";
pub(super) const PARSER_SOURCE_ID: &str = "offline-parser-h06";
const MAX_STAFF: i64 = 100;
const MAX_TOILETS: i64 = 20;

static FACILITY_FAILURE: Pattern =
    Pattern::new(r"故障|壊れ|使えな|使用できな|停止|故障|outage|fail(?:ed|s|ure)?|broken|unavailable", true);
static WATER_FAILURE: Pattern = Pattern::new(
    r"断水|水.{0,5}(?:止ま|停止|出な)|給水.{0,5}(?:止ま|停止)|water.{0,12}(?:outage|stop|stoppage|cut|fail)|(?:outage|cut).{0,12}water",
    true,
);
static ADD: Pattern = Pattern::new(r"追加|増員|増や|設置|導入|add(?:ed|ing)?|increase|install(?:ed|ing)?|provide", true);
static TOILET: Pattern = Pattern::new(r"トイレ|便器|restroom|toilet", true);
static STAFF: Pattern = Pattern::new(r"スタッフ|職員|職員|staff|worker", true);
static WATER: Pattern = Pattern::new(r"水|給水|water", true);
static EVACUEE_ARRIVAL: Pattern = Pattern::new(
    r"避難者.{0,12}(?:到着|来る|受け入れ|追加)|(?:到着|来る|受け入れ).{0,12}避難者|evacuees?.{0,24}(?:arriv|admit|add)|(?:arriv|admit|add).{0,24}evacuees?",
    true,
);
static EVACUEE_DEPARTURE: Pattern = Pattern::new(
    r"避難者.{0,12}(?:帰宅|退所|帰る|退出)|(?:帰宅|退所|退出|帰る).{0,12}避難者|evacuees?.{0,24}(?:depart|leave|go home)|(?:depart|remove|leave).{0,24}evacuees?",
    true,
);
static CLAUSES: Pattern = Pattern::new(r"[,、。;+]|\b(?:and|then)\b|さらに|そして|および", false);
static FACILITY_WORD: Pattern = Pattern::new(r"施設|facility", true);

static DAY_WORDS: Pattern = Pattern::new(r"\bday\s*\d{1,3}\b|\d{1,3}\s*日目", false);
static DATE_WORDS: Pattern = Pattern::new(r"\b\d{4}[-/]\d{1,2}[-/]\d{1,2}\b|\b\d{1,2}/\d{1,2}\b", false);
static CLOCK_WORDS: Pattern = Pattern::new(r"(?<!\d)\d{1,2}:\d{2}\s*(?:am|pm)?(?![a-z0-9])", false);
static FACILITY_COUNT: [Pattern; 2] = [
    Pattern::new(
        r"(?:トイレ|便器|restrooms?|toilets?)(?:を|が|は)?\s*(?P<value>(?<![A-Za-z0-9_])[+\-]\d{1,6}|\d{1,6}|[零〇一二三四五六七八九十]+)\s*(?:基|台|個|unit(?:s)?)?",
        true,
    ),
    Pattern::new(
        r"(?P<value>(?<![A-Za-z0-9_])[+\-]\d{1,6}|\d{1,6}|[零〇一二三四五六七八九十]+)\s*(?:基|台|個|unit(?:s)?)?(?:の|を)?\s*(?:トイレ|便器|restrooms?|toilets?)",
        true,
    ),
];
static STAFF_COUNT: [Pattern; 2] = [
    Pattern::new(
        r"(?:スタッフ|職員|staff|workers?)(?:を|が|は)?\s*(?P<value>(?<![A-Za-z0-9_])[+\-]\d{1,6}|\d{1,6}|[零〇一二三四五六七八九十]+)\s*(?:人|名|member(?:s)?|people|workers?)?",
        true,
    ),
    Pattern::new(
        r"(?P<value>(?<![A-Za-z0-9_])[+\-]\d{1,6}|\d{1,6}|[零〇一二三四五六七八九十]+)\s*(?:人|名|member(?:s)?|people|workers?)?(?:の|を)?\s*(?:スタッフ|職員|staff|workers?)",
        true,
    ),
];
static EVACUEE_COUNT: [Pattern; 2] = [
    Pattern::new(
        r"(?:避難者|evacuees?|residents?)(?:を|が|は)?\s*(?P<value>(?<![A-Za-z0-9_])[+\-]\d{1,6}|\d{1,6}|[零〇一二三四五六七八九十]+)\s*(?:人|名|people|persons?|residents?)?",
        true,
    ),
    Pattern::new(
        r"(?P<value>(?<![A-Za-z0-9_])[+\-]\d{1,6}|\d{1,6}|[零〇一二三四五六七八九十]+)\s*(?:人|名|people|persons?|residents?)?(?:の|を)?\s*(?:避難者|evacuees?|residents?)",
        true,
    ),
];

static FACILITY_ID: Pattern =
    Pattern::new(r"\b(?:toilet|restroom|facility|water_station)[_:#-]([a-z0-9][a-z0-9._:-]*)", true);
static FACILITY_NAMED: Pattern =
    Pattern::new(r"\b(?:toilet|restroom|facility|water station)\s+([a-z][a-z0-9._:-]*)", true);
static ZONE_ID: Pattern = Pattern::new(r"\b(?:zone|zone_id)\s*[:=#]?\s*([a-z][a-z0-9._:-]*)", true);

static DAY: Pattern = Pattern::new(r"\bday\s*(\d{1,3})\b|(\d{1,3})\s*日目", true);
static FULL_DATE: Pattern = Pattern::new(r"\b(\d{4})[-/](\d{1,2})[-/](\d{1,2})\b", false);
static SHORT_DATE: Pattern = Pattern::new(r"\b(\d{1,2})/(\d{1,2})\b", false);
static NEXT_MORNING: Pattern = Pattern::new(
    r"翌朝|翌日(?:の)?(?:朝|午前|昼)?|次(?:の|の日の)?朝|\b(?:next|following)\s+(?:morning|day)\b",
    true,
);
static DAY_REFERENCES: Pattern =
    Pattern::new(r"\bday\s*\d{1,3}\b|\d{1,3}\s*日目|\b\d{4}[-/]\d{1,2}[-/]\d{1,2}\b", true);
static VAGUE_TIME: Pattern =
    Pattern::new(r"午後|午前|朝|昼|正午|夕方|夜|\b(?:afternoon|morning|evening|night|noon|midday)\b", true);
static RANGE: Pattern = Pattern::new(r"から.{0,24}まで|\bto\b|[-–—]", true);
static TIMES: [Pattern; 4] = [
    Pattern::new(r"(?<!\d)(\d{1,2}):(\d{2})\s*(am|pm)?(?!\d)", true),
    Pattern::new(r"(?<!\d)(\d{1,2})\s*(am|pm)(?![a-z])", true),
    Pattern::new(r"(午前|午後)\s*(\d{1,2})(?:\s*時)?(?:\s*(\d{1,2})\s*分)?", false),
    Pattern::new(r"(?<!\d)(\d{1,2})\s*時(?:\s*(\d{1,2})\s*分)?", false),
];

pub(super) static COVERAGE_CLAUSES: Pattern = Pattern::new(
    r"[,、。;+]|\b(?:and|then|while)\b|さらに|そして|および|また|加えて|同時に|一方で|そのうえ",
    true,
);
static TEMPORAL: [Pattern; 9] = [
    Pattern::new(r"\bday\s*\d{1,3}\b", true),
    Pattern::new(r"\d{1,3}\s*日目", true),
    Pattern::new(r"\b\d{4}[-/]\d{1,2}[-/]\d{1,2}\b", true),
    Pattern::new(r"\b\d{1,2}/\d{1,2}\b", true),
    Pattern::new(r"\b\d{1,2}:\d{2}\s*(?:am|pm)?\b", true),
    Pattern::new(r"\b\d{1,2}\s*(?:am|pm)\b", true),
    Pattern::new(r"(?:午前|午後)?\s*\d{1,2}\s*時(?:\s*\d{1,2}\s*分)?", true),
    Pattern::new(r"\b(?:at|on|in|by|from|until|to|day|night|midnight|noon|morning|afternoon|evening)\b", true),
    Pattern::new(r"深夜|夜間|夜|朝|昼|夕方|午前|午後|ごろ|頃|から|まで|\d+", true),
];
static TEMPORAL_PUNCTUATION: Pattern = Pattern::new(r"[\s:：,、。;；/()（）-]+", false);


/// A `ParseIssue`: its code and message (the compiler's answer holds no issue's choices).
#[derive(Clone, Debug)]
pub(super) struct Issue {
    pub code: &'static str,
    pub message: String,
}

/// An `Ambiguity`.
#[derive(Clone, Debug)]
pub(super) struct Ambiguity {
    pub message: &'static str,
    pub choices: Vec<String>,
}

/// A `TargetSelector`.
#[derive(Clone, Debug)]
pub(super) struct Selector {
    pub kind: &'static str,
    pub target_id: Option<String>,
    pub candidate_ids: Vec<String>,
}

/// A `TimedAction` of the parser: no guard, no causation, reversible.
#[derive(Clone, Debug)]
pub(super) struct Effect {
    pub effect_id: String,
    pub op: &'static str,
    pub selector: Selector,
    pub start: i64,
    pub end: Option<i64>,
    pub parameters: (&'static str, V),
    pub units: &'static str,
    pub assumption_ids: Vec<String>,
    pub dependencies: Vec<&'static str>,
}

/// A `PlanAssumption` of the parser: no units or value.
#[derive(Clone, Debug)]
pub(super) struct Assumption {
    pub id: String,
    pub description: String,
}

/// A `DraftPlan` of the parser.
#[derive(Clone, Debug)]
pub(super) struct Plan {
    pub program_id: String,
    pub source_text: String,
    pub fork_at_minute: i64,
    pub effects: Vec<Effect>,
    pub assumptions: Vec<Assumption>,
    pub support_mode: &'static str,
    pub branch_id: String,
    pub ambiguities: Vec<Ambiguity>,
}

/// A `ParseResult`.
pub(super) enum Parsed {
    Parsed(Plan),
    Clarification(Plan),
    Rejected(Vec<Issue>),
}

/// Python raised a ValueError (a pydantic ValidationError): the route answers 400.
fn invalid(what: &str) -> Error {
    Error::Value(format!("invalid offline parse: {what}"))
}

/// `ParseIssue(...)`, refused as pydantic refuses it.
fn issue(code: &'static str, message: String, choices: Vec<String>) -> Result<Issue> {
    if text::chars(&message) > 500 || choices.len() > 100 {
        return Err(invalid("ParseIssue"));
    }
    Ok(Issue { code, message })
}

fn fixed(code: &'static str, message: &str) -> Issue {
    Issue { code, message: message.to_owned() }
}

fn hh_mm(code: &'static str, message: &str) -> Issue {
    Issue { code, message: message.to_owned() }
}

/// `TargetSelector(kind=..., candidate_ids=...)`, refused as pydantic refuses it.
fn candidates_selector(kind: &'static str, candidate_ids: Vec<String>) -> Result<Selector> {
    let mut seen = std::collections::BTreeSet::new();
    if candidate_ids.len() > 100 || !candidate_ids.iter().all(|id| seen.insert(id.as_str())) {
        return Err(invalid("TargetSelector"));
    }
    Ok(Selector { kind, target_id: None, candidate_ids })
}

struct Window {
    start: i64,
    end: Option<i64>,
}

enum Target {
    Selected { selector: Selector, ambiguity: Option<Ambiguity>, assumption: Option<String> },
    Issue(Issue),
}

/// `parse_natural_language(request)`.
pub(super) fn parse(raw_text: &str, branch_id: &str, selected_minute: i64, summary: &Summary) -> Result<Parsed> {
    let text = text::normalize(raw_text);
    let window = match time_window(&text, summary)? {
        Ok(Some(window)) => window,
        Ok(None) => Window { start: selected_minute, end: None },
        Err(issue) => return Ok(Parsed::Rejected(vec![issue])),
    };
    let operations = detect_operations(&text)?;
    if operations.is_empty() {
        return Ok(Parsed::Rejected(vec![Issue {
            code: "unsupported_event",
            message: "この入力に対応するオフライン作用を特定できません。対象・出来事・時刻を指定してください。".to_owned(),
        }]));
    }

    let mut effects = Vec::new();
    let mut assumptions = Vec::new();
    let mut ambiguities = Vec::new();
    let mut issues = Vec::new();
    let mut seen_ops = std::collections::BTreeSet::new();
    for (index, op) in operations.iter().copied().enumerate() {
        let index = index + 1;
        let population = op == "add_evacuees" || op == "depart_evacuees";
        if population && !summary.available_policies.iter().any(|policy| policy == "population_dynamics_v1") {
            issues.push(fixed("unsupported_policy", "このシナリオでは人口動態イベントを実行できません。"));
            continue;
        }
        let count = if op == "water_outage" { None } else { extract_count(&text, op)? };
        let limit = match op {
            "add_staff" => MAX_STAFF,
            "add_toilet" => MAX_TOILETS,
            _ => 1000,
        };
        if let Some(count) = count {
            if count <= 0 || count > limit {
                let noun = match op {
                    "add_staff" => "スタッフ",
                    "add_toilet" => "トイレ",
                    "facility_outage" => "停止するユニット",
                    _ => "避難者",
                };
                issues.push(Issue {
                    code: "invalid_count",
                    message: format!("{noun}の数は1以上{limit}以下で指定してください。"),
                });
                continue;
            }
        }
        let count = count.unwrap_or(1);

        let (selector, assumption) = match resolve_target(&text, op, summary)? {
            Target::Issue(issue) => {
                issues.push(issue);
                continue;
            }
            Target::Selected { selector, ambiguity, assumption } => {
                if let Some(ambiguity) = ambiguity {
                    ambiguities.push(ambiguity);
                }
                (selector, assumption)
            }
        };
        if population && window.end.is_some() {
            issues.push(fixed(
                "unsupported_time_window",
                "到着・帰宅は一時点のイベントです。開始時刻だけを指定してください。",
            ));
            continue;
        }
        let (units, parameters, dependencies): (&'static str, (&'static str, V), Vec<&'static str>) = match op {
            "facility_outage" => {
                if let Some(facility) = selected_facility(&selector, summary) {
                    if count > facility.working_units {
                        issues.push(Issue {
                            code: "invalid_count",
                            message: format!("{} の稼働中ユニット数は {} です。", facility.name, facility.working_units),
                        });
                        continue;
                    }
                }
                ("facility_unit", ("units", V::Int(count)), vec!["facility_capacity"])
            }
            "add_staff" => ("staff_member", ("count", V::Int(count)), vec!["staff_operations"]),
            "add_toilet" => ("toilet_stall", ("count", V::Int(count)), vec!["facility_capacity"]),
            "add_evacuees" | "depart_evacuees" => ("evacuee", ("count", V::Int(count)), vec!["population_dynamics_v1"]),
            _ => ("water_supply", ("outage", V::Bool(true)), vec!["operational_events_v1", "resource_service_v1"]),
        };

        let mut assumption_ids = Vec::new();
        if let Some(description) = assumption {
            let id = format!("assumption-{index}-target");
            assumption_ids.push(id.clone());
            assumptions.push(Assumption { id, description });
        }
        effects.push(Effect {
            effect_id: format!("effect-{index}-{op}"),
            op,
            selector,
            start: window.start,
            end: if population { None } else { window.end },
            parameters,
            units,
            assumption_ids,
            dependencies,
        });
        seen_ops.insert(op);
    }

    if !issues.is_empty() {
        return Ok(Parsed::Rejected(issues));
    }
    if effects.is_empty() {
        return Ok(Parsed::Rejected(vec![fixed("unknown_target", "対象を確定できません。")]));
    }
    if let Some(duration) = summary.duration {
        if effects.iter().any(|effect| effect.start >= duration || effect.end.is_some_and(|end| end > duration)) {
            return Ok(Parsed::Rejected(vec![Issue {
                code: "time_out_of_range",
                message: format!("指定時刻はシナリオの終了時刻（{duration}分）を超えています。"),
            }]));
        }
    }

    let earliest = effects.iter().map(|effect| effect.start).min().unwrap_or(selected_minute);
    let fork_at_minute = selected_minute.min(earliest);
    let mut hasher = Sha256::new();
    hasher.update(format!("{branch_id}\0{raw_text}\0{fork_at_minute}").as_bytes());
    let plan_id = hex(&hasher.finalize());
    let support_mode = if !ambiguities.is_empty() {
        "clarification_required"
    } else if seen_ops.len() > 1 {
        "composed"
    } else {
        "native"
    };
    let plan = Plan {
        program_id: format!("nl-h06-{}", &plan_id[..16]),
        source_text: raw_text.to_owned(),
        fork_at_minute,
        effects,
        assumptions,
        support_mode,
        branch_id: branch_id.to_owned(),
        ambiguities,
    };
    Ok(if plan.ambiguities.is_empty() { Parsed::Parsed(plan) } else { Parsed::Clarification(plan) })
}

/// `_detect_operations(text)`.
pub(super) fn detect_operations(text: &str) -> Result<Vec<&'static str>> {
    let mut ops: Vec<&'static str> = Vec::new();
    for clause in text::split(CLAUSES.get()?, text)? {
        let is_add = text::found(ADD.get()?, clause)?;
        if text::found(EVACUEE_ARRIVAL.get()?, clause)? {
            ops.push("add_evacuees");
        }
        if text::found(EVACUEE_DEPARTURE.get()?, clause)? {
            ops.push("depart_evacuees");
        }
        let has_water_failure = text::found(WATER.get()?, clause)? && text::found(WATER_FAILURE.get()?, clause)?;
        if has_water_failure {
            ops.push("water_outage");
        }
        let toilet = text::found(TOILET.get()?, clause)?;
        if toilet && is_add {
            ops.push("add_toilet");
        }
        if text::found(STAFF.get()?, clause)? && is_add {
            ops.push("add_staff");
        }
        if text::found(FACILITY_FAILURE.get()?, clause)?
            && !has_water_failure
            && (toilet || text::found(FACILITY_WORD.get()?, clause)?)
        {
            ops.push("facility_outage");
        }
    }
    let mut unique = Vec::new();
    for op in ops {
        if !unique.contains(&op) {
            unique.push(op);
        }
    }
    Ok(unique)
}

/// `_extract_count(text, op)`.
fn extract_count(text: &str, op: &str) -> Result<Option<i64>> {
    let quantity = text::blank(DAY_WORDS.get()?, text, " ")?;
    let quantity = text::blank(DATE_WORDS.get()?, &quantity, " ")?;
    let quantity = text::blank(CLOCK_WORDS.get()?, &quantity, " ")?;
    let patterns = match op {
        "facility_outage" | "add_toilet" => &FACILITY_COUNT,
        "add_staff" => &STAFF_COUNT,
        "add_evacuees" | "depart_evacuees" => &EVACUEE_COUNT,
        _ => return Ok(None),
    };
    for pattern in patterns {
        if let Some(found) = text::search(pattern.get()?, &quantity)? {
            let Some(raw) = found.name("value") else { continue };
            let raw = raw.as_str();
            let first = raw.chars().next();
            if matches!(first, Some('+' | '-')) || first.is_some_and(|char| !"零〇一二三四五六七八九十".contains(char)) {
                return text::int(raw).map(Some).ok_or_else(|| Error::unsupported("a count past i64"));
            }
            return Ok(kanji_number(raw));
        }
    }
    Ok(None)
}

fn kanji_digit(char: char) -> Option<i64> {
    Some(match char {
        '零' | '〇' => 0,
        '一' => 1,
        '二' => 2,
        '三' => 3,
        '四' => 4,
        '五' => 5,
        '六' => 6,
        '七' => 7,
        '八' => 8,
        '九' => 9,
        '十' => 10,
        _ => return None,
    })
}

/// `_parse_kanji_number(value)`.
fn kanji_number(value: &str) -> Option<i64> {
    let chars: Vec<char> = value.chars().collect();
    match chars.as_slice() {
        [one] => kanji_digit(*one),
        ['十', second] => kanji_digit(*second).map(|digit| 10 + digit),
        [first, '十'] => kanji_digit(*first).map(|digit| digit * 10),
        _ => None,
    }
}

fn is_toilet(kind: &str) -> bool {
    matches!(&*crate::text::casefold(kind), "toilet" | "restroom" | "toilet_facility" | "sanitation")
}

/// `_contains_label(text, label)`.
fn contains_label(text: &str, label: &str) -> Result<bool> {
    let normalized = text::normalize(label);
    if matches!(normalized.as_str(), "トイレ" | "便器" | "toilet" | "restroom" | "staff" | "water" | "水")
        || normalized.is_empty()
    {
        return Ok(false);
    }
    let first = normalized.chars().next().unwrap_or(' ');
    if normalized.is_ascii() && (first.is_ascii_alphanumeric() || first == '_') {
        let pattern = format!("(?<![a-z0-9_]){}(?![a-z0-9_])", fancy_regex::escape(&normalized));
        return text::found(&text::compile(&pattern, true)?, text);
    }
    Ok(text.contains(normalized.as_str()))
}

/// `_names_zone_area(text, name)`.
fn names_zone_area(text: &str, name: &str) -> bool {
    let normalized = text::normalize(name);
    !normalized.is_empty() && ["ゾーン", "エリア"].iter().any(|word| text.contains(&format!("{normalized}{word}")))
}

/// `_explicit_facility_id(text)`.
fn explicit_facility_id(text: &str) -> Result<Option<String>> {
    if let Some(found) = text::search(FACILITY_ID.get()?, text)? {
        return Ok(found.get(0).map(|whole| whole.as_str().to_owned()));
    }
    if let Some(found) = text::search(FACILITY_NAMED.get()?, text)? {
        let word = found.get(1).map(|word| crate::text::casefold(word.as_str()).into_owned()).unwrap_or_default();
        if !matches!(word.as_str(), "fail" | "fails" | "failed" | "failure" | "broken" | "outage" | "is" | "was") {
            return Ok(found.get(0).map(|whole| whole.as_str().to_owned()));
        }
    }
    Ok(None)
}

/// `_explicit_zone_id(text)`.
fn explicit_zone_id(text: &str) -> Result<Option<String>> {
    Ok(text::search(ZONE_ID.get()?, text)?.and_then(|found| found.get(1).map(|id| id.as_str().to_owned())))
}

fn selected_facility<'s>(selector: &Selector, summary: &'s Summary) -> Option<&'s Facility> {
    let id = selector.target_id.as_deref()?;
    summary.facilities.iter().find(|facility| facility.id == id)
}

/// `_zones_for_operation(op, summary)`.
fn zones_for_operation<'s>(op: &str, summary: &'s Summary) -> (Vec<&'s Zone>, &'static str) {
    let facility_zones = |matching: &dyn Fn(&str) -> bool| -> Vec<&str> {
        summary
            .facilities
            .iter()
            .filter(|facility| matching(&facility.kind))
            .filter_map(|facility| facility.zone_id.as_deref())
            .collect()
    };
    match op {
        "add_toilet" => {
            let ids = facility_zones(&is_toilet);
            (summary.zones.iter().filter(|zone| ids.contains(&zone.id.as_str())).collect(), "既存トイレのあるZone")
        }
        "add_evacuees" => {
            let ids = facility_zones(&|kind: &str| matches!(&*crate::text::casefold(kind), "intake" | "reception"));
            (summary.zones.iter().filter(|zone| ids.contains(&zone.id.as_str())).collect(), "受付のあるZone")
        }
        "add_staff" => (summary.zones.iter().filter(|zone| zone.staff_count != 0).collect(), "スタッフの配置されているZone"),
        "depart_evacuees" => (summary.zones.iter().filter(|zone| zone.evacuee_count != 0).collect(), "避難者のいるZone"),
        _ => (Vec::new(), ""),
    }
}

/// `_resolve_target(text, op, summary)`.
fn resolve_target(text: &str, op: &str, summary: &Summary) -> Result<Target> {
    if op == "facility_outage" || op == "water_outage" {
        let water = op == "water_outage";
        let facilities: Vec<&Facility> = if water {
            summary.facilities.iter().filter(|item| crate::text::casefold(&item.kind) == "water_station").collect()
        } else {
            summary.facilities.iter().filter(|item| is_toilet(&item.kind)).collect()
        };
        let mut candidates: Vec<(&str, &str, &'static str)> = Vec::new();
        if water {
            let zones: Vec<&Zone> = summary
                .zones
                .iter()
                .filter(|zone| {
                    zone.has_water_station || facilities.iter().any(|item| item.zone_id.as_deref() == Some(zone.id.as_str()))
                })
                .collect();
            candidates.extend(zones.iter().map(|zone| (zone.id.as_str(), zone.name.as_str(), "zone")));
            if zones.is_empty() {
                candidates.extend(
                    facilities
                        .iter()
                        .filter(|item| item.zone_id.is_none())
                        .map(|item| (item.id.as_str(), item.name.as_str(), "facility")),
                );
            }
        } else {
            candidates.extend(facilities.iter().map(|item| (item.id.as_str(), item.name.as_str(), "facility")));
        }
        let mut named = Vec::new();
        if water {
            for item in &facilities {
                if contains_label(text, &item.id)? || contains_label(text, &item.name)? {
                    named.push((item.id.as_str(), item.name.as_str(), "facility"));
                }
            }
        }
        if named.is_empty() {
            for candidate in &candidates {
                if contains_label(text, candidate.0)? || contains_label(text, candidate.1)? {
                    named.push(*candidate);
                }
            }
        }
        let matches = if named.is_empty() { &candidates } else { &named };
        if let Some(explicit) = explicit_facility_id(text)? {
            if named.is_empty() {
                return Ok(Target::Issue(issue(
                    "unknown_target",
                    format!("指定した対象ID {explicit} はシナリオにありません。"),
                    candidates.iter().map(|item| item.0.to_owned()).collect(),
                )?));
            }
        }
        if matches.is_empty() {
            return Ok(Target::Issue(fixed(
                "unknown_target",
                "シナリオ内に対応する対象がありません。存在するIDを指定してください。",
            )));
        }
        if let [(id, _, kind)] = matches.as_slice() {
            return Ok(Target::Selected {
                selector: Selector { kind, target_id: Some((*id).to_owned()), candidate_ids: Vec::new() },
                ambiguity: None,
                assumption: None,
            });
        }
        let candidate_ids: Vec<String> = matches.iter().map(|item| item.0.to_owned()).collect();
        if candidate_ids.len() > 100 {
            return Err(invalid("Ambiguity"));
        }
        let ambiguity = Ambiguity { message: "対象を一つ選択してください。", choices: candidate_ids.clone() };
        return Ok(Target::Selected {
            selector: candidates_selector(matches[0].2, candidate_ids)?,
            ambiguity: Some(ambiguity),
            assumption: None,
        });
    }

    let mut named = Vec::new();
    for zone in &summary.zones {
        if contains_label(text, &zone.id)? || contains_label(text, &zone.name)? || names_zone_area(text, &zone.name) {
            named.push(zone);
        }
    }
    let (eligible, basis) = zones_for_operation(op, summary);
    let mut from_eligible = false;
    let matches: Vec<&Zone> = if let Some(explicit) = explicit_zone_id(text)? {
        let matches: Vec<&Zone> = summary.zones.iter().filter(|zone| zone.id == explicit).collect();
        if matches.is_empty() {
            return Ok(Target::Issue(issue(
                "unknown_target",
                format!("指定したZone ID {explicit} はシナリオにありません。"),
                summary.zones.iter().map(|zone| zone.id.clone()).collect(),
            )?));
        }
        matches
    } else if !named.is_empty() {
        named
    } else if let Some(default) = &summary.default_zone_id {
        summary.zones.iter().filter(|zone| &zone.id == default).collect()
    } else if !eligible.is_empty() {
        from_eligible = true;
        eligible
    } else {
        summary.zones.iter().collect()
    };
    if matches.is_empty() {
        return Ok(Target::Issue(fixed(
            "unknown_target",
            "対象Zoneがシナリオにありません。存在するZone IDを指定してください。",
        )));
    }
    if let [chosen] = matches.as_slice() {
        return Ok(Target::Selected {
            selector: Selector { kind: "zone", target_id: Some(chosen.id.clone()), candidate_ids: Vec::new() },
            ambiguity: None,
            assumption: (from_eligible && !basis.is_empty())
                .then(|| format!("追加先が書かれていないため、{basis}「{}」（{}）を対象にしました。", chosen.name, chosen.id)),
        });
    }
    let candidate_ids: Vec<String> = matches.iter().map(|zone| zone.id.clone()).collect();
    if candidate_ids.len() > 100 {
        return Err(invalid("Ambiguity"));
    }
    Ok(Target::Selected {
        selector: candidates_selector("zone", candidate_ids.clone())?,
        ambiguity: Some(Ambiguity { message: "追加先Zoneを選択してください。", choices: candidate_ids }),
        assumption: None,
    })
}

/// The days from 0001-01-01 of a valid date, or None for one `datetime.date` refuses.
pub(super) fn ordinal(year: i64, month: i64, day: i64) -> Option<i64> {
    if !(1..=9999).contains(&year) || !(1..=12).contains(&month) {
        return None;
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let lengths = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if day < 1 || day > lengths[(month - 1) as usize] {
        return None;
    }
    let before = year - 1;
    let mut days = before * 365 + before / 4 - before / 100 + before / 400;
    days += lengths[..(month - 1) as usize].iter().sum::<i64>();
    Some(days + day)
}

fn group_int(found: &fancy_regex::Captures<'_, str>, index: usize) -> Option<i64> {
    found.get(index).and_then(|group| text::int(group.as_str()))
}

/// `_parse_time_window(text, summary)`: a window, None, or the issue that rejects the request.
fn time_window(text: &str, summary: &Summary) -> Result<std::result::Result<Option<Window>, Issue>> {
    let day_match = text::search(DAY.get()?, text)?;
    let full_date = text::search(FULL_DATE.get()?, text)?;
    let short_date = if full_date.is_some() { None } else { text::search(SHORT_DATE.get()?, text)? };
    let tokens = time_tokens(text)?;
    if text::found(NEXT_MORNING.get()?, text)? {
        return Ok(Err(hh_mm(
            "ambiguous_time",
            "『翌朝』『翌日』は対象イベントとの基点が曖昧です。各作用のDay番号と時刻を指定してください。",
        )));
    }
    if text::count(DAY_REFERENCES.get()?, text)? > 1 && tokens.len() > 1 {
        return Ok(Err(hh_mm(
            "ambiguous_time",
            "複数のイベント時刻を一つの作用時刻にまとめられません。各作用にDay番号と時刻を指定してください。",
        )));
    }
    if text::found(VAGUE_TIME.get()?, text)? && tokens.is_empty() {
        return Ok(Err(hh_mm(
            "ambiguous_time",
            "『午後』などの幅がある時間表現だけでは時刻を決められません。具体的な時刻を指定してください。",
        )));
    }
    if tokens.len() > 2 {
        return Ok(Err(fixed("ambiguous_time", "複数の時刻を読み取れません。開始・終了を一つずつ指定してください。")));
    }
    if tokens.len() == 2 && !text::found(RANGE.get()?, text)? {
        return Ok(Err(fixed("ambiguous_time", "複数の時刻があるため、開始と終了の範囲を明示してください。")));
    }

    let mut base_day: Option<i64> = None;
    if let Some(found) = day_match {
        let day = group_int(&found, 1).or_else(|| group_int(&found, 2)).unwrap_or(0);
        if !(1..=366).contains(&day) {
            return Ok(Err(fixed("invalid_time", "Dayは1から366で指定してください。")));
        }
        base_day = Some((day - 1) * 1440);
    } else if let Some(found) = full_date {
        let parts = [group_int(&found, 1), group_int(&found, 2), group_int(&found, 3)];
        let Some(requested) = ordinal(parts[0].unwrap_or(0), parts[1].unwrap_or(0), parts[2].unwrap_or(0)) else {
            return Ok(Err(fixed("invalid_time", "日付が正しくありません。")));
        };
        let Some(start) = summary.start_date else {
            // Python's issue offers the choice "Day N", which is not a StableId: pydantic refuses
            // the issue itself, and the request is a 400.
            return Err(invalid("ParseIssue choices"));
        };
        let days = (requested - start.0) * 1440;
        if days < 0 {
            return Ok(Err(fixed("invalid_time", "シナリオ開始日より前の日付です。")));
        }
        base_day = Some(days);
    } else if let Some(found) = short_date {
        let Some(start) = summary.start_date else {
            // Python's issue offers the choice "Day N", which is not a StableId: pydantic refuses
            // the issue itself, and the request is a 400.
            return Err(invalid("ParseIssue choices"));
        };
        let Some(requested) = ordinal(start.1, group_int(&found, 1).unwrap_or(0), group_int(&found, 2).unwrap_or(0)) else {
            return Ok(Err(fixed("invalid_time", "日付が正しくありません。")));
        };
        let days = (requested - start.0) * 1440;
        if days < 0 {
            return Ok(Err(fixed("invalid_time", "シナリオ開始日より前の日付です。")));
        }
        base_day = Some(days);
    }

    if tokens.is_empty() && base_day.is_none() {
        return Ok(Ok(None));
    }
    let base = base_day.unwrap_or(0);
    let (hour, minute) = tokens.first().copied().unwrap_or((0, 0));
    let start = base + hour * 60 + minute;
    let mut end = None;
    if let [_, (end_hour, end_minute)] = tokens.as_slice() {
        let value = base + end_hour * 60 + end_minute;
        if value <= start {
            return Ok(Err(fixed(
                "invalid_time_range",
                "終了時刻は開始時刻より後にしてください。日をまたぐ指定はDay番号も付けてください。",
            )));
        }
        end = Some(value);
    }
    Ok(Ok(Some(Window { start, end })))
}

/// `_find_time_tokens(text)`: (hour, minute) of each time, in the order of the text.
fn time_tokens(text: &str) -> Result<Vec<(i64, i64)>> {
    let mut matches: Vec<(usize, usize, i64, i64)> = Vec::new();
    let mut occupied: Vec<(usize, usize)> = Vec::new();
    for (priority, pattern) in TIMES.iter().enumerate() {
        for found in text::all_captures(pattern.get()?, text)? {
            let Some(whole) = found.get(0) else { continue };
            let (start, end) = (whole.start(), whole.end());
            if occupied.iter().any(|(first, last)| start < *last && *first < end) {
                continue;
            }
            let group = |index: usize| found.get(index).map(|group| group.as_str());
            let number = |index: usize| group(index).and_then(text::int);
            let (mut hour, minute, period) = match priority {
                0 => (number(1).unwrap_or(0), number(2).unwrap_or(0), group(3)),
                1 => (number(1).unwrap_or(0), 0, group(2)),
                2 => (number(2).unwrap_or(0), number(3).unwrap_or(0), group(1)),
                _ => (number(1).unwrap_or(0), number(2).unwrap_or(0), None),
            };
            if minute > 59 || hour > 24 {
                continue;
            }
            if let Some(period) = period {
                if !(1..=12).contains(&hour) {
                    continue;
                }
                if crate::text::casefold(period) == "pm" || period == "午後" {
                    hour = hour % 12 + 12;
                } else {
                    hour %= 12;
                }
            } else if hour == 24 && minute != 0 {
                continue;
            }
            if hour == 24 {
                continue;
            }
            matches.push((start, end, hour, minute));
            occupied.push((start, end));
        }
    }
    matches.sort();
    Ok(matches.into_iter().map(|(_, _, hour, minute)| (hour, minute)).collect())
}

/// `_offline_fallback_covers_all_clauses(text)`.
pub(super) fn covers_all_clauses(raw_text: &str) -> Result<bool> {
    let normalized = text::normalize(raw_text);
    for clause in text::split(COVERAGE_CLAUSES.get()?, &normalized)? {
        if text::strip(clause).is_empty() || !detect_operations(clause)?.is_empty() {
            continue;
        }
        if !is_temporal_clause(clause)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// `_is_temporal_clause(clause)`.
fn is_temporal_clause(clause: &str) -> Result<bool> {
    let mut residual = clause.to_owned();
    for pattern in &TEMPORAL {
        residual = text::blank(pattern.get()?, &residual, " ")?;
    }
    Ok(text::blank(TEMPORAL_PUNCTUATION.get()?, &residual, "")?.is_empty())
}

/// The parser's parameters as a map: `{"units": n}`, `{"count": n}` or `{"outage": true}`.
pub(super) fn parameters_value(parameters: &(&'static str, V)) -> V {
    let mut map = IndexMap::new();
    map.insert(Arc::from(parameters.0), parameters.1.clone());
    V::Map(map)
}
