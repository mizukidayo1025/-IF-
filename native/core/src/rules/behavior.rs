//! The decisions of the propose phase with `UtilityBehaviorPolicy` and `PriorityRuleBehaviorPolicy`
//! (`policies/behavior.py`, which keeps only their settings), reading the core's view
//! (`rules/world_view.rs`), and the observed state of a decision (moved to the core in lane P8 a5).
//!
//! The policy is what the Run read of `controller.decisions` (`run::behavior`, once per controller):
//! `scoring_of` asks for the runner, policy and scorer of the classes the rule knows, and `due`
//! reads the scorer's coefficients in the order the kernel read them before, with its refusals. A
//! read that raised in Python is a `PolicyFault` with its path, which the PyO3 crate raises again
//! by reading that path (policy E14).
//!
//! These steps change nothing and the draws keep no state, so refusing at any point is safe; the
//! step then raises KernelRefusal. That covers a policy or runner of another class (subclasses
//! too), and any value on which the deleted Python rule raised, which `check_candidate` and
//! `check_proposal` repeat from `ActionCandidate` and `ActionProposal`. A `Proposal` is what an
//! `ActionProposal` holds, as values: the PyO3 crate makes the Python object only when something
//! reads it.
//!
//! The coefficients are the ones the deleted `UtilityBehaviorPolicy.propose` had: `score` and
//! `candidates_of` have them with the same order of operations. The trigger texts format the
//! policy's numbers as `format(x, "g")` does (`g`).

use std::borrow::Cow;
use std::sync::Arc;

use crate::config::Fault;
use crate::entities::{Activity, RequestStatus};
use crate::error::{Error, Result};
use crate::mathx::{exp, py_fsum, py_is_close, py_max, py_round_int, py_sum};
use crate::rng::{label, Key, KeyedDraws};
use crate::rules::psychology::{Bad, PolicyFault};
use crate::rules::world_view::{DemandModifier, ViewAgent, ViewData};
use crate::run::behavior::{Behavior, Rule, POLICY_PATH, SCORER_PATH};
use crate::run::held::{Held, Obj};
use crate::text::g_text;
use crate::value::V;

/// A result whose failure is a refusal or a read of the policy to raise again.
pub type Got<T> = std::result::Result<T, Bad>;

#[track_caller]
fn refused<T>(reason: &'static str) -> Got<T> {
    Err(Bad::Refused(Error::unsupported(reason)))
}

// ---- The actions -----------------------------------------------------------------------------------

/// `ActionKind`; the discriminant is the place in `ACTION_VALUES`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Wait,
    RequestToilet,
    Rest,
    Sleep,
    SeekHelp,
    FetchWater,
    CollectMeal,
    SeekInformation,
    Socialize,
    CareFamily,
    HelpOthers,
}

/// The values of `ActionKind`, in the order of `Action`.
pub const ACTION_VALUES: [&str; 11] = [
    "wait",
    "request_toilet",
    "rest",
    "sleep",
    "seek_help",
    "fetch_water",
    "collect_meal",
    "seek_information",
    "socialize",
    "care_family",
    "help_others",
];

impl Action {
    pub fn value(self) -> &'static str {
        ACTION_VALUES[self as usize]
    }
}

// ---- What the rule reads of the policy -------------------------------------------------------------

/// The runner, the policy and its scorer, each of a class the rule knows.
pub struct Scoring<'a> {
    pub rule: Rule,
    /// `decisions.policy`.
    pub policy: &'a Obj,
    /// The policy itself, or the priority rule's `scorer`.
    pub scorer: &'a Obj,
}

/// The policy as the step asks for it first: the runner's class, then the policy's and the
/// scorer's, with the refusal of the first that is not one the rule knows.
pub fn scoring_of(behavior: &Behavior) -> Got<Scoring<'_>> {
    let fault = |fault: &Fault| Bad::Policy(PolicyFault { path: behavior.fault_path.to_vec(), fault: fault.clone() });
    let rule = *behavior.rule.as_ref().map_err(fault)?;
    let policy = behavior.policy.as_ref().map_err(fault)?;
    let scorer = behavior.scorer.as_ref().map_err(fault)?;
    Ok(Scoring { rule, policy, scorer })
}

impl Scoring<'_> {
    /// The path of the scorer from the controller.
    fn scorer_path(&self) -> &'static [&'static str] {
        match self.rule {
            Rule::Utility => POLICY_PATH,
            Rule::Priority => SCORER_PATH,
        }
    }

    /// An attribute of the scorer.
    fn scorer(&self, name: &'static str) -> Got<&Held> {
        attribute(self.scorer, self.scorer_path(), name)
    }
}

/// An attribute of an object the Run read, at `path` from the controller.
fn attribute<'a>(object: &'a Obj, path: &'static [&'static str], name: &'static str) -> Got<&'a Held> {
    object.get(name).map_err(|fault| {
        let mut path = path.to_vec();
        path.push(name);
        Bad::Policy(PolicyFault { path, fault })
    })
}

/// An exact int.
fn exact_i64(value: &V) -> Result<i64> {
    match value {
        V::Int(number) => Ok(*number),
        V::Big(_) => Err(Error::unsupported("an int is too large")),
        _ => Err(Error::unsupported("a policy value is not an int")),
    }
}

/// A float, or an int that converts exactly. Only operations with a float read these, in which
/// Python converts the int the same way, and so does `format(x, "g")`.
fn number(value: &V) -> Result<f64> {
    if let V::Float(number) = value {
        return Ok(*number);
    }
    let integer = exact_i64(value)?;
    if integer.unsigned_abs() > (1_u64 << 53) {
        return Err(Error::unsupported("a policy value is too large"));
    }
    Ok(integer as f64)
}

/// The plain value of an attribute; any other object is not an int.
fn plain(value: &Held) -> Result<&V> {
    match value {
        Held::Value(value) => Ok(value),
        _ => Err(Error::unsupported("a policy value is not an int")),
    }
}

/// `bool(value)` of a plain value; an object the Run could not read as one is refused.
fn truthy(value: &Held) -> Result<bool> {
    Ok(match value {
        Held::Value(V::Null) => false,
        Held::Value(V::Bool(flag)) => *flag,
        Held::Value(V::Int(number)) => *number != 0,
        Held::Value(V::Big(_)) => true,
        Held::Value(V::Float(number)) => *number != 0.0,
        Held::Value(V::Str(text)) | Held::Text(text) => !text.is_empty(),
        Held::Value(V::List(items)) => !items.is_empty(),
        Held::Value(V::Map(items)) => !items.is_empty(),
        Held::Object(_) | Held::Opaque { .. } | Held::Method(_) => return Err(Error::unsupported("a flag is not a plain value")),
    })
}

/// The fields of `UtilityBehaviorPolicy` the scoring reads. An int of a field counts as its float, as
/// it does in each operation with a float here.
struct Settings<'a> {
    /// The scorer's `policy_version`, in the key of the draw.
    version: &'a str,
    sleep_aware: bool,
    sleep_debt_weight: f64,
    resource_service: bool,
    autonomy: bool,
    hydration_threshold: f64,
    hunger_threshold: f64,
    water_base: f64,
    water_weight: f64,
    meal_base: f64,
    meal_weight: f64,
    query_threshold: f64,
    query_weight: f64,
    priority_rule: bool,
}

impl Settings<'_> {
    fn query_enabled(&self) -> bool {
        self.query_threshold <= 1.0 || self.query_weight > 0.0
    }
}

/// What every person's scoring shares in one call.
struct Frozen<'a> {
    data: &'a ViewData,
    settings: &'a Settings<'a>,
    draws: &'a KeyedDraws,
    /// The people of each zone of `data.zones`, in ID order.
    by_zone: Vec<Vec<usize>>,
    /// `view.group_members`, sorted by group.
    groups: Vec<(&'a Arc<str>, Vec<usize>)>,
    /// `view.immediate_needs()`, by person.
    pressure: Vec<f64>,
}

impl<'a> Frozen<'a> {
    /// A NaN would make `max` over tuples depend on identity, so it is left to Python.
    fn new(data: &'a ViewData, settings: &'a Settings<'a>, draws: &'a KeyedDraws) -> std::result::Result<Self, &'static str> {
        let mut by_zone: Vec<Vec<usize>> = vec![Vec::new(); data.zones.len()];
        let mut pressure = Vec::with_capacity(data.agents.len());
        for (index, agent) in data.agents.iter().enumerate() {
            by_zone[agent.zone].push(index);
            let needs = &agent.needs;
            let mut value = agent.stress;
            for need in [needs.toilet_urgency, needs.hunger, needs.fatigue, needs.hydration_need] {
                value = py_max(value, need);
            }
            for number in [agent.stress, needs.toilet_urgency, needs.hunger, needs.fatigue, needs.hydration_need] {
                if number.is_nan() {
                    return Err("a person's pressure is NaN");
                }
            }
            pressure.push(value);
        }
        Ok(Self { data, settings, draws, by_zone, groups: data.group_members(), pressure })
    }

    /// `view.group_members.get(agent.group_id or "", ())`.
    fn group_of(&self, agent: &ViewAgent) -> &[usize] {
        let key = agent.group_id.as_deref().unwrap_or("");
        match self.groups.binary_search_by(|(group, _)| (***group).cmp(key)) {
            Ok(place) => &self.groups[place].1,
            Err(_) => &[],
        }
    }

    /// `max(peers, key=lambda peer: (pressure[peer], peer), default=None)` of peers in ID order: with
    /// no NaN, a later peer with an equal pressure has the larger tuple.
    fn most_pressed(&self, peers: impl Iterator<Item = usize>) -> Option<usize> {
        let mut best = None;
        for peer in peers {
            if best.is_none_or(|kept: usize| self.pressure[peer] >= self.pressure[kept]) {
                best = Some(peer);
            }
        }
        best
    }
}

/// The last modifier of an action wins, as in `{item.action: item for item in ...}`.
pub fn demand_of(modifiers: &[DemandModifier], action: Action) -> Option<&DemandModifier> {
    modifiers.iter().rev().find(|modifier| &*modifier.action == action.value())
}

/// One candidate of `candidates` in `propose`, before its probability.
struct Candidate {
    action: Action,
    eligible: bool,
    utility: f64,
    priority: i64,
}

/// A candidate with what `ActionCandidate` holds. `utility` includes the priority's half.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scored {
    pub action: Action,
    pub eligible: bool,
    pub utility: f64,
    pub priority: i64,
    pub probability: f64,
}

/// What a proposal says.
#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    pub candidates: Vec<Scored>,
    pub choice_value: f64,
    pub tie_broken: bool,
    pub selection_probability: f64,
    pub action: Action,
    pub next_decision_at: i64,
    /// The target's place in the view.
    pub target: Option<usize>,
}

/// `ActionCandidate.__post_init__`, on what the scorer can make.
fn check_candidate(candidate: &Scored) -> std::result::Result<(), &'static str> {
    if !candidate.utility.is_finite()
        || !candidate.probability.is_finite()
        || !(0.0..=1.0).contains(&candidate.probability)
        || (!candidate.eligible && candidate.probability != 0.0)
    {
        return Err("a candidate Python turns down");
    }
    Ok(())
}

/// `ActionProposal.__post_init__`, on what the scorer can make.
fn check_proposal(
    minute: i64,
    next_decision_at: i64,
    choice_value: f64,
    selection_probability: f64,
    candidates: &[Scored],
) -> std::result::Result<(), &'static str> {
    if minute < 0 || next_decision_at <= minute {
        return Err("a proposal has invalid decision timing");
    }
    if !choice_value.is_finite() || !(0.0..1.0).contains(&choice_value) {
        return Err("a choice value Python turns down");
    }
    if !selection_probability.is_finite() || selection_probability <= 0.0 || selection_probability > 1.0 {
        return Err("a selected probability Python turns down");
    }
    let eligible: Vec<f64> = candidates.iter().filter(|item| item.eligible).map(|item| item.probability).collect();
    if eligible.is_empty() || !py_is_close(py_sum(&eligible), 1.0) {
        return Err("the probabilities do not sum to one");
    }
    Ok(())
}

/// `_decision_interval_minutes`.
fn decision_interval(
    base_minutes: i64,
    action: Action,
    modifier: Option<&DemandModifier>,
    minimum: i64,
    maximum: i64,
) -> std::result::Result<i64, &'static str> {
    let Some(modifier) = modifier else {
        return Ok(base_minutes);
    };
    if !matches!(action, Action::Rest | Action::Sleep) || modifier.duration_multiplier == 1.0 {
        return Ok(base_minutes);
    }
    let Some(scaled) = py_round_int(base_minutes as f64 * modifier.duration_multiplier) else {
        return Err("a scaled interval does not round to an i64");
    };
    Ok(scaled.max(minimum).min(maximum))
}

/// `_sleep_debt_utility`.
fn sleep_debt_utility(agent: &ViewAgent, settings: &Settings<'_>) -> f64 {
    if !settings.sleep_aware {
        return agent.body.sleep_debt_minutes / 1440.0 * 0.45;
    }
    let required = match agent.person.required_sleep_minutes {
        None | Some(0) => 480,
        Some(minutes) => minutes,
    };
    settings.sleep_debt_weight * agent.body.sleep_debt_minutes / required as f64
}

/// `_target_for_action`.
fn target_for_action(frozen: &Frozen<'_>, index: usize, action: Action) -> Option<usize> {
    let data = frozen.data;
    let agent = &data.agents[index];
    let zone_peers = || frozen.by_zone[agent.zone].iter().copied().filter(|peer| *peer != index);
    match action {
        Action::Socialize => zone_peers().next(),
        Action::CareFamily => {
            let peers: Vec<usize> = frozen
                .group_of(agent)
                .iter()
                .copied()
                .filter(|peer| *peer != index && data.agents[*peer].zone == agent.zone)
                .collect();
            let explicit: Vec<usize> =
                peers.iter().copied().filter(|peer| is_care_linked(agent, &data.agents[*peer])).collect();
            if !explicit.is_empty() {
                frozen.most_pressed(explicit.into_iter())
            } else if agent.person.personal_context_present
                || peers.iter().any(|peer| data.agents[*peer].person.personal_context_present)
            {
                None
            } else {
                frozen.most_pressed(peers.into_iter())
            }
        }
        Action::HelpOthers => {
            let threshold = frozen.pressure[index] + 0.15;
            frozen.most_pressed(zone_peers().filter(|peer| frozen.pressure[*peer] > threshold))
        }
        _ => None,
    }
}

/// `peer in agent.care_for_ids or agent.id in peer.caregiver_ids or agent.id in peer.parent_ids`.
fn is_care_linked(agent: &ViewAgent, peer: &ViewAgent) -> bool {
    agent.person.care_for_ids.iter().any(|id| *id == peer.id)
        || peer.person.caregiver_ids.iter().any(|id| *id == agent.id)
        || peer.person.parent_ids.iter().any(|id| *id == agent.id)
}

/// The list `candidates` of `propose`, in its order.
fn candidates_of(frozen: &Frozen<'_>, index: usize) -> Vec<Candidate> {
    let data = frozen.data;
    let settings = frozen.settings;
    let agent = &data.agents[index];
    let zone = data.zone_of(agent);
    let needs = &agent.needs;
    let body = &agent.body;
    let person = &agent.person;
    let resource_service = settings.resource_service && data.resource_service_enabled;

    let mut distress = py_max(needs.toilet_urgency, needs.hunger);
    distress = py_max(distress, needs.fatigue);
    if resource_service {
        distress = py_max(distress, needs.hydration_need);
    }
    distress = py_max(distress, agent.stress);
    distress = py_max(distress, agent.chronic_distress);
    distress = py_max(distress, body.discontent * 0.75);
    distress = py_max(distress, body.negative_affect * 0.6);

    let has_open_request = agent.support_request_id.is_some()
        && matches!(agent.support_request_status, None | Some(RequestStatus::Pending) | Some(RequestStatus::Assigned));
    let query_enabled = settings.query_enabled();
    let query_trigger = query_enabled
        && (agent.information_query_pressure >= settings.query_threshold
            || body.uncertainty >= py_max(0.65, settings.query_threshold));
    let cooled_down = agent.support_cooldown_remaining_minutes == 0;
    let can_seek_help = (agent.support_need >= 0.25 && distress >= 0.50 && !has_open_request && cooled_down)
        || (query_trigger && !has_open_request && cooled_down);
    let can_seek_information = query_enabled && query_trigger && !has_open_request && cooled_down;

    let minute_of_day = data.minute_index.rem_euclid(1440);
    let mut sleep_window = false;
    if let (Some(sleep_time), Some(wake_time)) = (person.usual_sleep_minute, person.usual_wake_minute) {
        sleep_window = if sleep_time > wake_time {
            minute_of_day >= sleep_time || minute_of_day < wake_time
        } else {
            sleep_time <= minute_of_day && minute_of_day < wake_time
        };
    }
    let sleep_rhythm_due =
        data.autonomy_enabled && sleep_window && person.required_sleep_minutes.is_some_and(|minutes| minutes > 0);
    let mut meal_rhythm_due = false;
    if data.autonomy_enabled {
        if let Some(wake_time) = person.usual_wake_minute {
            // Wider than i64 for a wake time near its limits. The result fits: it is below 1440.
            let since_wake = (i128::from(minute_of_day) - i128::from(wake_time)).rem_euclid(1440);
            meal_rhythm_due = [120_i128, 420, 660].iter().map(|offset| (since_wake - offset).abs()).min().unwrap_or(0) <= 45;
        }
    }

    let mut candidates = Vec::with_capacity(11);
    candidates.push(Candidate {
        action: Action::Wait,
        eligible: true,
        utility: 0.10 + 0.15 * (1.0 - distress),
        priority: 0,
    });
    candidates.push(Candidate {
        action: Action::RequestToilet,
        eligible: needs.toilet_urgency >= 0.25 || body.hygiene_need >= 0.5,
        utility: 0.20 + 0.90 * needs.toilet_urgency + 0.35 * body.hygiene_need,
        priority: i64::from(needs.toilet_urgency >= 0.80 || body.hygiene_need >= 0.8),
    });
    candidates.push(Candidate {
        action: Action::Rest,
        eligible: needs.fatigue >= 0.25 || needs.hunger >= 0.70 || agent.stress >= 0.50,
        utility: 0.10 + 0.45 * needs.fatigue + 0.20 * needs.hunger + 0.30 * agent.stress,
        priority: 0,
    });
    candidates.push(Candidate {
        action: Action::Sleep,
        eligible: needs.fatigue >= 0.65 || body.sleep_debt_minutes >= 240.0 || sleep_rhythm_due,
        utility: 0.25 + 0.90 * needs.fatigue + sleep_debt_utility(agent, settings)
            + (if sleep_rhythm_due { 0.20 } else { 0.0 })
            - 0.20 * py_max(0.0, zone.lighting_level - 0.3),
        priority: 0,
    });
    candidates.push(Candidate {
        action: Action::SeekHelp,
        eligible: can_seek_help,
        utility: 0.15 + 0.65 * distress + 0.25 * agent.support_need + 0.10 * agent.trust + 0.20 * body.uncertainty
            + 0.15 * body.discontent
            - 0.10 * body.reassurance
            + settings.query_weight * agent.information_query_pressure,
        priority: i64::from(distress >= 0.80),
    });
    candidates.push(Candidate {
        action: Action::SeekInformation,
        eligible: can_seek_information,
        utility: 0.20 + 0.55 * body.uncertainty + settings.query_weight * agent.information_query_pressure,
        priority: i64::from(body.uncertainty >= 0.9),
    });
    if data.autonomy_enabled && settings.autonomy {
        let zone_people = &frozen.by_zone[agent.zone];
        let zone_peers = || zone_people.iter().copied().filter(|peer| *peer != index);
        let mut family: Vec<usize> = frozen
            .group_of(agent)
            .iter()
            .copied()
            .filter(|peer| *peer != index && data.agents[*peer].zone == agent.zone)
            .collect();
        let explicit: Vec<usize> = zone_peers().filter(|peer| is_care_linked(agent, &data.agents[*peer])).collect();
        if !explicit.is_empty() {
            family.retain(|peer| explicit.contains(peer));
        } else if person.personal_context_present {
            family.clear();
        }
        let family_target = frozen.most_pressed(family.into_iter());
        let help_threshold = distress + 0.15;
        let help_target = frozen.most_pressed(zone_peers().filter(|peer| frozen.pressure[*peer] > help_threshold));
        candidates.push(Candidate {
            action: Action::Socialize,
            eligible: zone_peers().next().is_some(),
            utility: 0.10 + 0.25 * body.uncertainty + 0.15 * body.negative_affect + 0.10 * (1.0 - body.reassurance),
            priority: 0,
        });
        candidates.push(Candidate {
            action: Action::CareFamily,
            eligible: family_target.is_some_and(|target| frozen.pressure[target] >= 0.25),
            utility: 0.12 + family_target.map_or(0.0, |target| 0.45 * frozen.pressure[target]),
            priority: i64::from(family_target.is_some_and(|target| data.agents[target].stress >= 0.8)),
        });
        candidates.push(Candidate {
            action: Action::HelpOthers,
            eligible: help_target.is_some(),
            utility: 0.15 + help_target.map_or(0.0, |target| 0.4 * frozen.pressure[target]),
            priority: i64::from(help_target.is_some_and(|target| data.agents[target].stress >= 0.8)),
        });
    }
    if resource_service {
        candidates.push(Candidate {
            action: Action::FetchWater,
            eligible: needs.hydration_need >= settings.hydration_threshold,
            utility: settings.water_base + settings.water_weight * needs.hydration_need,
            priority: i64::from(needs.hydration_need >= 0.8),
        });
        candidates.push(Candidate {
            action: Action::CollectMeal,
            eligible: needs.hunger >= settings.hunger_threshold || meal_rhythm_due,
            utility: settings.meal_base + settings.meal_weight * needs.hunger + (if meal_rhythm_due { 0.12 } else { 0.0 }),
            priority: i64::from(needs.hunger >= 0.85),
        });
    }
    candidates
}

/// The probabilities of `propose`: softmax over the eligible candidates, with the demand multipliers,
/// and the last eligible one takes what the others leave.
fn score(candidates: &[Candidate], modifiers: &[DemandModifier], temperature: f64) -> std::result::Result<Vec<Scored>, &'static str> {
    let totals: Vec<f64> = candidates.iter().map(|item| item.utility + item.priority as f64 * 0.5).collect();
    let mut maximum = None;
    for (candidate, total) in candidates.iter().zip(&totals) {
        if candidate.eligible {
            maximum = Some(maximum.map_or(*total, |kept| py_max(kept, *total)));
        }
    }
    let Some(maximum) = maximum else {
        return Err("no candidate is eligible");
    };
    if temperature == 0.0 {
        return Err("the temperature is zero");
    }
    let mut weights = Vec::with_capacity(candidates.len());
    for (candidate, total) in candidates.iter().zip(&totals) {
        if !candidate.eligible {
            weights.push(0.0);
            continue;
        }
        let Ok(weight) = exp((*total - maximum) / temperature) else {
            return Err("an exponent is out of range");
        };
        let multiplier = demand_of(modifiers, candidate.action).map_or(1.0, |modifier| modifier.probability_multiplier);
        weights.push(weight * multiplier);
    }
    let total_weight = py_sum(&weights);
    if total_weight == 0.0 {
        return Err("the weights sum to zero");
    }
    let mut probabilities: Vec<f64> = weights.iter().map(|weight| weight / total_weight).collect();
    let eligible: Vec<usize> = (0..candidates.len()).filter(|place| candidates[*place].eligible).collect();
    let Some((last, others)) = eligible.split_last() else {
        return Err("no candidate is eligible");
    };
    let rest: Vec<f64> = others.iter().map(|place| probabilities[*place]).collect();
    let Some(others_sum) = py_fsum(&rest) else {
        return Err("a sum of probabilities is out of range");
    };
    probabilities[*last] = py_max(0.0, 1.0 - others_sum);
    let mut scored = Vec::with_capacity(candidates.len());
    for ((candidate, total), probability) in candidates.iter().zip(totals).zip(probabilities) {
        let item = Scored {
            action: candidate.action,
            eligible: candidate.eligible,
            utility: total,
            priority: candidate.priority,
            probability,
        };
        check_candidate(&item)?;
        scored.push(item);
    }
    Ok(scored)
}

/// `UtilityBehaviorPolicy.propose`, then `PriorityRuleBehaviorPolicy.propose` if it is that policy.
fn compute(frozen: &Frozen<'_>, index: usize, temperature: f64, base_minutes: i64) -> std::result::Result<Outcome, &'static str> {
    let data = frozen.data;
    let agent = &data.agents[index];
    let zone = data.zone_of(agent);
    let modifiers = &zone.demand_modifiers[..];
    let minute = data.minute_index;
    let candidates = candidates_of(frozen, index);
    let mut evidence = score(&candidates, modifiers, temperature)?;

    let mut key = Key::labelled(label::BEHAVIOR);
    key.text(&agent.rng_key).text(&agent.id).text("minute").int(minute);
    key.text("policy").text(frozen.settings.version).text("choice");
    let mut choice_value = key.draw(frozen.draws);
    let mut cumulative = 0.0;
    let Some(last) = evidence.iter().rev().find(|item| item.eligible).copied() else {
        return Err("no candidate is eligible");
    };
    let mut selected = last;
    for candidate in &evidence {
        if !candidate.eligible {
            continue;
        }
        cumulative += candidate.probability;
        if choice_value < cumulative {
            selected = *candidate;
            break;
        }
    }
    let interval = |action: Action| -> std::result::Result<i64, &'static str> {
        let minutes = decision_interval(
            base_minutes,
            action,
            demand_of(modifiers, action),
            zone.min_rest_decision_minutes,
            zone.max_rest_decision_minutes,
        )?;
        minute.checked_add(minutes).ok_or("a minute is out of range")
    };
    let mut next_decision_at = interval(selected.action)?;
    check_proposal(minute, next_decision_at, choice_value, selected.probability, &evidence)?;
    let mut selection_probability = selected.probability;
    let mut action = selected.action;

    let priority_rule = frozen.settings.priority_rule;
    if priority_rule {
        // Finite utilities, from check_candidate, so a tuple compares as these two do.
        let mut best: Option<(i64, f64)> = None;
        for item in evidence.iter().filter(|item| item.eligible) {
            best = match best {
                Some((priority, utility))
                    if item.priority < priority || (item.priority == priority && item.utility <= utility) =>
                {
                    Some((priority, utility))
                }
                _ => Some((item.priority, item.utility)),
            };
        }
        let Some(best) = best else {
            return Err("no candidate is eligible");
        };
        let tied = |item: &Scored| item.eligible && (item.priority, item.utility) == best;
        let mut ties: Vec<Scored> = evidence.iter().filter(|item| tied(item)).copied().collect();
        ties.sort_unstable_by_key(|item| item.action.value());
        key.text("priority-tie-break");
        choice_value = key.draw(frozen.draws);
        let count = ties.len();
        let place = ((choice_value * count as f64) as usize).min(count - 1);
        let chosen = ties[place];
        let probability = 1.0 / count as f64;
        for item in evidence.iter_mut() {
            item.probability = if tied(&*item) { probability } else { 0.0 };
            check_candidate(item)?;
        }
        action = chosen.action;
        selection_probability = probability;
        next_decision_at = interval(action)?;
        check_proposal(minute, next_decision_at, choice_value, probability, &evidence)?;
    }
    Ok(Outcome {
        candidates: evidence,
        choice_value,
        tie_broken: priority_rule,
        selection_probability,
        action,
        next_decision_at,
        target: target_for_action(frozen, index, action),
    })
}

// ---- The trigger texts ---------------------------------------------------------------------------

/// `format(value, "g")` of a float, a NaN and an infinity too.
pub fn g(value: f64) -> String {
    if value.is_nan() {
        return "nan".to_owned();
    }
    if value.is_infinite() {
        return if value > 0.0 { "inf" } else { "-inf" }.to_owned();
    }
    // `g_text` refuses only what is not finite.
    g_text(value).unwrap_or_default()
}

/// The trigger text of each action, made once a call that has someone due.
#[derive(Clone, Debug, PartialEq)]
pub struct Triggers {
    texts: [Option<Cow<'static, str>>; 11],
}

impl Triggers {
    /// `query_threshold` when the information query is on; the hydration and hunger thresholds when
    /// the resource service is.
    fn new(query_threshold: Option<f64>, resource_thresholds: Option<(f64, f64)>) -> Self {
        let mut texts: [Option<Cow<'static, str>>; 11] = Default::default();
        let mut set = |action: Action, text: Cow<'static, str>| texts[action as usize] = Some(text);
        set(Action::Wait, Cow::Borrowed("always"));
        set(Action::RequestToilet, Cow::Borrowed("toilet_urgency>=0.25 or hygiene_need>=0.5"));
        set(Action::Rest, Cow::Borrowed("fatigue>=0.25 or hunger>=0.70 or stress>=0.50"));
        set(Action::Sleep, Cow::Borrowed("fatigue>=0.65 or sleep_debt_minutes>=240 or individual_sleep_window"));
        match query_threshold {
            Some(threshold) => set(
                Action::SeekHelp,
                Cow::Owned(format!(
                    "support_need>=0.25 and maximum need/stress>=0.50, or information query pressure \
                     >={} or uncertainty>=0.65; no_open_support_request and support_cooldown_elapsed",
                    g(threshold)
                )),
            ),
            None => set(
                Action::SeekHelp,
                Cow::Borrowed(
                    "support_need>=0.25 and maximum need/stress>=0.50 and no_open_support_request and \
                     support_cooldown_elapsed",
                ),
            ),
        }
        set(Action::SeekInformation, Cow::Borrowed("information_query_pressure>=threshold or uncertainty>=0.65"));
        set(Action::Socialize, Cow::Borrowed("at least one co-located person is available"));
        set(Action::CareFamily, Cow::Borrowed("co-located family or care-linked person has an unmet need"));
        set(Action::HelpOthers, Cow::Borrowed("co-located person has substantially higher immediate need or stress"));
        if let Some((hydration, hunger)) = resource_thresholds {
            set(Action::FetchWater, Cow::Owned(format!("hydration_need>={}", g(hydration))));
            set(Action::CollectMeal, Cow::Owned(format!("hunger>={} or individual_meal_window", g(hunger))));
        }
        Self { texts }
    }

    /// `_demand_trigger`.
    pub fn of(&self, action: Action, modifier: Option<&DemandModifier>) -> Result<Cow<'_, str>> {
        let Some(trigger) = &self.texts[action as usize] else {
            return Err(Error::unsupported("an action has no trigger"));
        };
        let Some(modifier) = modifier else {
            return Ok(Cow::Borrowed(trigger));
        };
        let mut text = format!(
            "{trigger}; experimental zone action multiplier x{}; events={}; assumptions={}",
            g(modifier.probability_multiplier),
            modifier.modifier_event_ids.join(","),
            modifier.assumption_ids.join(","),
        );
        if modifier.duration_multiplier != 1.0 {
            text.push_str(&format!("; decision interval x{}", g(modifier.duration_multiplier)));
        }
        Ok(Cow::Owned(text))
    }
}

// ---- The proposals -------------------------------------------------------------------------------

/// What all the proposals of one call share.
#[derive(Clone, Debug, PartialEq)]
pub struct Common {
    pub triggers: Triggers,
    /// The view's `state_hash`.
    pub state_hash: Arc<str>,
    /// The scorer's `policy_version`, in the key of the draw.
    pub scorer_version: Arc<str>,
    /// The policy's `policy_version`, the proposal's.
    pub policy_version: Arc<str>,
}

/// An `ActionProposal` of the view it was made from: the person's place in it, and what was decided.
#[derive(Clone, Debug, PartialEq)]
pub struct Proposal {
    pub index: usize,
    pub outcome: Outcome,
    pub common: Arc<Common>,
}

impl Proposal {
    /// `rng_key`: the parts of the draw's key, ints as `Err`.
    pub fn rng_key<'a>(&'a self, data: &'a ViewData) -> Vec<std::result::Result<&'a str, i64>> {
        let agent = &data.agents[self.index];
        let mut key = vec![
            Ok(label::BEHAVIOR),
            Ok(&*agent.rng_key),
            Ok(&*agent.id),
            Ok("minute"),
            Err(data.minute_index),
            Ok("policy"),
            Ok(&*self.common.scorer_version),
            Ok("choice"),
        ];
        if self.outcome.tie_broken {
            key.push(Ok("priority-tie-break"));
        }
        key
    }

    /// The trigger of a candidate, with the person's zone's modifier of its action.
    pub fn trigger_of(&self, data: &ViewData, candidate: &Scored) -> Result<String> {
        let modifiers = &data.zone_of(&data.agents[self.index]).demand_modifiers;
        Ok(self.common.triggers.of(candidate.action, demand_of(modifiers, candidate.action))?.into_owned())
    }
}

/// Whether the person is due: their `next_decision_at` has come and they are idle, resting or
/// sleeping.
fn is_due(data: &ViewData, agent: &ViewAgent) -> bool {
    data.minute_index >= agent.next_decision_at && matches!(agent.activity, Activity::Idle | Activity::Resting | Activity::Sleeping)
}

/// `self.decisions.propose_due(view, rng)` of the propose phase, after `scoring_of`: the proposals
/// of everyone due, in ID order, None when nobody is. Everyone is computed first, so a refusal
/// changes nothing. `seed` is `str(rng.seed)` and `state_hash` the view's.
pub fn due(data: &ViewData, scoring: &Scoring<'_>, seed: &str, state_hash: Arc<str>) -> Got<Option<Vec<Proposal>>> {
    if !data.agents.iter().any(|agent| is_due(data, agent)) {
        return Ok(None);
    }
    let Held::Value(V::Str(scorer_version)) = scoring.scorer("policy_version")? else {
        return refused("the policy version is not a str");
    };
    let policy_version = match scoring.rule {
        Rule::Utility => scorer_version,
        Rule::Priority => match attribute(scoring.policy, POLICY_PATH, "policy_version")? {
            Held::Value(V::Str(version)) => version,
            _ => return refused("the policy version is empty or not a str"),
        },
    };
    if policy_version.is_empty() {
        return refused("the policy version is empty or not a str");
    }
    let float = |name: &'static str| -> Got<f64> { Ok(number(plain(scoring.scorer(name)?)?)?) };
    let settings = Settings {
        version: scorer_version,
        sleep_aware: scorer_version.starts_with("sleep-aware-utility-softmax-v2"),
        sleep_debt_weight: float("sleep_debt_utility_weight")?,
        resource_service: truthy(scoring.scorer("resource_service_enabled")?)?,
        autonomy: truthy(scoring.scorer("autonomy_enabled")?)?,
        hydration_threshold: float("hydration_action_threshold")?,
        hunger_threshold: float("hunger_action_threshold")?,
        water_base: float("water_utility_base")?,
        water_weight: float("water_utility_need_weight")?,
        meal_base: float("meal_utility_base")?,
        meal_weight: float("meal_utility_need_weight")?,
        query_threshold: float("information_query_threshold")?,
        query_weight: float("information_query_utility_weight")?,
        priority_rule: scoring.rule == Rule::Priority,
    };
    let temperature = float("temperature")?;
    let interval = exact_i64(plain(scoring.scorer("decision_interval_minutes")?)?)?;
    let temperatures = scoring.scorer("temperature_by_agent")?;
    let intervals = scoring.scorer("decision_interval_minutes_by_agent")?;
    let (Held::Value(V::Map(temperatures)), Held::Value(V::Map(intervals))) = (temperatures, intervals) else {
        return refused("the per-person coefficients are not a plain dict");
    };
    let draws = KeyedDraws::new(seed);
    let frozen = Frozen::new(data, &settings, &draws).map_err(Error::unsupported)?;
    let triggers = Triggers::new(
        settings.query_enabled().then_some(settings.query_threshold),
        (settings.resource_service && data.resource_service_enabled).then_some((settings.hydration_threshold, settings.hunger_threshold)),
    );

    let mut outcomes = Vec::new();
    for (index, agent) in data.agents.iter().enumerate() {
        if !is_due(data, agent) {
            continue;
        }
        let temperature = match temperatures.get(&*agent.id) {
            Some(value) => number(value)?,
            None => temperature,
        };
        let interval = match intervals.get(&*agent.id) {
            Some(value) => exact_i64(value)?,
            None => interval,
        };
        let outcome = compute(&frozen, index, temperature, interval).map_err(Error::unsupported)?;
        outcomes.push((index, outcome));
    }
    let common = Arc::new(Common {
        triggers,
        state_hash,
        scorer_version: scorer_version.clone(),
        policy_version: policy_version.clone(),
    });
    Ok(Some(outcomes.into_iter().map(|(index, outcome)| Proposal { index, outcome, common: common.clone() }).collect()))
}

// ---- The observed state --------------------------------------------------------------------------

fn text_value(text: &Option<Arc<str>>) -> V {
    match text {
        Some(text) => V::Str(text.clone()),
        None => V::Null,
    }
}

fn text_list(items: &[Arc<str>]) -> V {
    V::List(items.iter().map(|item| V::Str(item.clone())).collect())
}

/// `RequestStatus.value`.
pub fn status_text(status: RequestStatus) -> &'static str {
    match status {
        RequestStatus::Pending => "pending",
        RequestStatus::Assigned => "assigned",
        RequestStatus::Resolved => "resolved",
        RequestStatus::Unresolved => "unresolved",
    }
}

/// `_agent_view_snapshot(view.agents[agent_id])`: what a decision records it observed of the
/// person, with its keys in that function's order. `rules/decisions.rs` writes the same document
/// as JSON bytes.
pub fn observed_value(data: &ViewData, index: usize) -> V {
    let agent = &data.agents[index];
    let zone = data.zone_of(agent);
    let map = |entries: Vec<(&str, V)>| V::Map(entries.into_iter().map(|(key, value)| (Arc::from(key), value)).collect());
    let mut needs = vec![
        ("toilet_urgency", V::Float(agent.needs.toilet_urgency)),
        ("hunger", V::Float(agent.needs.hunger)),
        ("fatigue", V::Float(agent.needs.fatigue)),
        ("hygiene_need", V::Float(agent.body.hygiene_need)),
    ];
    if data.resource_service_enabled {
        needs.push(("hydration_need", V::Float(agent.needs.hydration_need)));
    }
    let body = &agent.body;
    let psychological = vec![
        ("sleep_debt_minutes", V::Float(body.sleep_debt_minutes)),
        ("uncertainty", V::Float(body.uncertainty)),
        ("reassurance", V::Float(body.reassurance)),
        ("discontent", V::Float(body.discontent)),
        ("positive_affect", V::Float(body.positive_affect)),
        ("negative_affect", V::Float(body.negative_affect)),
    ];
    let queue = vec![
        ("task_id", text_value(&agent.queue_task_id)),
        ("kind", text_value(&agent.queue_kind)),
        ("target_id", text_value(&agent.queue_target_id)),
        ("wait_minutes", V::Int(agent.queue_wait_minutes)),
    ];
    let support = vec![
        ("request_id", text_value(&agent.support_request_id)),
        ("status", agent.support_request_status.map_or(V::Null, |status| V::Str(Arc::from(status_text(status))))),
        ("assigned_staff_id", text_value(&agent.support_assigned_staff_id)),
        ("wait_minutes", V::Int(agent.support_wait_minutes)),
        ("cooldown_remaining_minutes", V::Int(agent.support_cooldown_remaining_minutes)),
    ];
    let mut snapshot = vec![
        ("agent_id", V::Str(agent.id.clone())),
        ("zone_id", V::Str(zone.id.clone())),
        ("activity", V::Str(Arc::from(agent.activity.text()))),
        ("target_id", text_value(&agent.target_id)),
        ("needs", map(needs)),
        ("psychological", map(psychological)),
        ("sanitation_level", V::Float(zone.sanitation_level)),
        ("stress", V::Float(agent.stress)),
        ("trust", V::Float(agent.trust)),
        ("queue", map(queue)),
        ("support", map(support)),
    ];
    if !zone.zone_environment_modifier_event_ids.is_empty() || !zone.demand_modifiers.is_empty() {
        let modifiers = zone
            .demand_modifiers
            .iter()
            .map(|item| {
                map(vec![
                    ("action", V::Str(item.action.clone())),
                    ("probability_multiplier", V::Float(item.probability_multiplier)),
                    ("duration_multiplier", V::Float(item.duration_multiplier)),
                    ("event_ids", text_list(&item.modifier_event_ids)),
                    ("assumption_ids", text_list(&item.assumption_ids)),
                ])
            })
            .collect();
        snapshot.push((
            "zone_environment",
            map(vec![
                ("ambient_noise_level", V::Float(zone.ambient_noise_level)),
                ("rest_efficiency_multiplier", V::Float(zone.rest_efficiency_multiplier)),
                ("modifier_event_ids", text_list(&zone.zone_environment_modifier_event_ids)),
                ("assumption_ids", text_list(&zone.zone_environment_assumption_ids)),
                ("demand_modifiers", V::List(modifiers)),
            ]),
        ));
    }
    map(snapshot)
}
