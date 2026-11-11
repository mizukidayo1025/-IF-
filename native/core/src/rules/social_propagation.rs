//! The social model's step of the update phase: `_social_propagation` in
//! `experiments/phases/update.py`, with the rules of `StressPropagationPolicy` and
//! `ExpressivenessChannelOpennessPolicy` (`policies/social.py`) and `DeGrootContactPolicy`
//! (`policies/alternatives.py`), whose Python bodies lane L10 deleted. Moved to the core in lane
//! P8 a4.
//!
//! The step takes the psychology deltas of the minute (`Delta`, lane a3) and gives the deltas after
//! the social model, in ID order. It changes nothing until it knows its checks pass: `update` plans
//! everything from the rows, and any value the deleted rules turned down, or that this step does
//! not handle, is Unsupported. Then the plan goes to the row of `social_propagation_v1` (its
//! attached tree follows the row when the world is given back, lane P8-0c) and to
//! `social_network_v1` (`record_observed_contacts`, whose refusals are checked before the first
//! write). Every float operation keeps the order of the Python expression.
//!
//! The policy and its parameters are what the Run read of `controller.stress_policy`
//! (`run::stress`); a read that raised in Python is a `PolicyFault`, which the PyO3 crate raises
//! again (policy E14). Stress, zones and sensitivities come from the entity rows. The group comes
//! from the core's view (`rules/world_view.rs`): a person's household in `population_presence_v1`
//! overrides `profile.group_id` there. The trace's document goes to the host (`Host::trace`).
//!
//! The first tick of a Run makes the namespace, which the core cannot add to `model_state`: `update`
//! then plans from the empty state and writes nothing, and the PyO3 crate stores the empty
//! namespace before it calls `write`.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use indexmap::IndexMap;
use rustc_hash::FxBuildHasher;

use crate::config::Fault;
use crate::contacts::{key_head, nearby_buckets, pair_key, quoted_ids, select_contacts, zone_members, Basis, Bucket};
use crate::error::{Error, Result};
use crate::host::{Host, Trace};
use crate::mathx::{max_num, min_num, py_fsum, py_max, py_min, sum_nums, Num};
use crate::parallel::in_order;
use crate::rng::{label, KeyedDraws};
use crate::row::IdMap;
use crate::rules::kit;
use crate::rules::psychology::{Bad, Delta, PolicyFault};
use crate::rules::social_network::{self, ObservedContacts};
use crate::rules::trace::{social_document, SocialContact};
use crate::rules::world_view::ViewData;
use crate::run::held::{Held, Obj, NOT_AN_OBJECT};
use crate::run::stress::{Model, Stress};
use crate::state::social_network::NAME as SOCIAL_NETWORK;
use crate::state::social_propagation::{ContactRecord, SocialRow, NAME, SCHEMA_VERSION};
use crate::value::V;
use crate::world::World;

#[track_caller]
fn unsupported<T>(reason: &'static str) -> Result<T> {
    Err(Error::unsupported(reason))
}

// ---- What the Run read of the policy -------------------------------------------------------------

type Got<T> = std::result::Result<T, Bad>;

fn fault_at(path: &[&'static str], fault: Fault) -> Bad {
    Bad::Policy(PolicyFault { path: path.to_vec(), fault })
}

/// `controller.stress_policy`.
fn policy_of(stress: &Stress) -> Got<&Arc<Obj>> {
    stress.policy.as_ref().map_err(|fault| fault_at(&["stress_policy"], fault.clone()))
}

/// `controller.stress_policy.parameters`.
fn parameters_of(stress: &Stress) -> Got<&Arc<Obj>> {
    const PATH: [&str; 2] = ["stress_policy", "parameters"];
    let held = policy_of(stress)?.get("parameters").map_err(|fault| fault_at(&PATH, fault))?;
    let found = held.object().map_err(|fault| fault_at(&PATH, fault))?;
    found.ok_or_else(|| fault_at(&PATH, Fault::refused(NOT_AN_OBJECT)))
}

/// A parameter as the Run read it.
fn parameter<'a>(parameters: &'a Obj, name: &'static str) -> Got<&'a Held> {
    parameters.get(name).map_err(|fault| fault_at(&["stress_policy", "parameters", name], fault))
}

/// Python's truth of a value the Run read.
fn truth(held: &Held) -> Result<bool> {
    Ok(match held {
        Held::Value(value) => match value {
            V::Null => false,
            V::Bool(flag) => *flag,
            V::Int(number) => *number != 0,
            V::Big(_) => true,
            V::Float(number) => *number != 0.0,
            V::Str(text) => !text.is_empty(),
            V::List(items) => !items.is_empty(),
            V::Map(map) => !map.is_empty(),
        },
        Held::Text(text) => !text.is_empty(),
        _ => return unsupported("the flag is not a plain value"),
    })
}

/// `stress_policy.parameters.enabled`, by its truth: the step does nothing without it.
pub fn enabled(stress: &Stress) -> Got<bool> {
    Ok(truth(parameter(parameters_of(stress)?, "enabled")?)?)
}

/// `exact_int` of a parameter.
fn int_of(held: &Held) -> Result<i64> {
    match held {
        Held::Value(V::Int(number)) => Ok(*number),
        Held::Value(V::Big(_)) => unsupported("int out of range"),
        _ => unsupported("not an int"),
    }
}

/// A float, or an int that `SocialPropagationParameters` allows (within 0..1, so `0` or `1`).
fn number_of(value: &V) -> Result<Num> {
    match value {
        V::Float(number) => Ok(Num::Float(*number)),
        V::Int(whole @ 0..=1) => Ok(Num::Int(*whole)),
        V::Int(_) => unsupported("an int coefficient is outside 0..1"),
        V::Big(_) => unsupported("int out of range"),
        _ => unsupported("not an int"),
    }
}

fn number_param(held: &Held) -> Result<Num> {
    match held {
        Held::Value(value) => number_of(value),
        _ => unsupported("not an int"),
    }
}

/// `SocialPropagationParameters`. A rate or a strength may be an int (the config's `0` or `1`):
/// where it meets a float it is the same f64, and the two caps keep their type, because
/// `min(cap, x)` returns the int itself, which the trace writes as `0` and not `0.0`.
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub policy: Model,
    pub maximum: i64,
    pub minimum_contact_minutes: i64,
    pub cooldown_minutes: i64,
    pub spread: f64,
    pub recovery: f64,
    pub pair_cap: Num,
    pub agent_cap: Num,
    pub family_strength: f64,
    pub zone_strength: f64,
    pub nearby_strength: f64,
}

/// The policy by its class, then its parameters, in the order the kernel read them.
pub fn settings(stress: &Stress) -> Got<Settings> {
    let policy = stress.model.clone().map_err(|fault| fault_at(&["stress_policy"], fault))?;
    let parameters = parameters_of(stress)?;
    let int = |name| -> Got<i64> { Ok(int_of(parameter(parameters, name)?)?) };
    let cap = |name| -> Got<Num> { Ok(number_param(parameter(parameters, name)?)?) };
    let float = |name| -> Got<f64> { cap(name).map(Num::value) };
    Ok(Settings {
        policy,
        maximum: int("max_contacts_per_agent")?,
        minimum_contact_minutes: int("minimum_contact_minutes")?,
        cooldown_minutes: int("cooldown_minutes")?,
        spread: float("stress_spread_per_minute")?,
        recovery: float("natural_recovery_per_minute")?,
        pair_cap: cap("max_pair_stress_increase")?,
        agent_cap: cap("max_agent_stress_increase_per_minute")?,
        family_strength: float("family_channel_strength")?,
        zone_strength: float("same_zone_channel_strength")?,
        nearby_strength: float("nearby_zone_channel_strength")?,
    })
}

/// A plain dict the Run read.
pub type Table = IndexMap<Arc<str>, V>;

/// `parameters.source_expressiveness_by_agent`, a plain dict.
pub fn expressiveness(stress: &Stress) -> Got<&Table> {
    match parameter(parameters_of(stress)?, "source_expressiveness_by_agent")? {
        Held::Value(V::Map(table)) => Ok(table),
        _ => Err(Bad::Refused(Error::unsupported("the expressiveness is not a dict"))),
    }
}

/// `stress_policy.policy_version`, as the trace's document holds it.
pub fn policy_version(stress: &Stress) -> Got<V> {
    const PATH: [&str; 2] = ["stress_policy", "policy_version"];
    match policy_of(stress)?.get("policy_version").map_err(|fault| fault_at(&PATH, fault))? {
        Held::Value(value) => Ok(value.clone()),
        Held::Text(text) => Ok(V::Str(text.clone())),
        _ => Err(Bad::Refused(Error::unsupported("the policy version is not a plain value"))),
    }
}

// ---- The plan ------------------------------------------------------------------------------------

/// One person's values of this tick. The people of a plan are sorted by id, and differ in id.
pub struct Person<'a> {
    pub id: &'a str,
    pub group: Option<&'a str>,
    pub zone: &'a str,
    pub stress: f64,
    pub sensitivity: f64,
    pub expressiveness: f64,
    /// The stress of the psychology delta.
    pub base: f64,
}

pub struct Plan {
    /// The new stress of each person.
    pub stresses: Vec<f64>,
    /// The new contacts, in the order of Python's new dict.
    pub contacts: IdMap<ContactRecord>,
    /// The new cooldowns, sorted, or None if the row has these already.
    pub cooldowns: Option<IdMap<i64>>,
    /// (recipient, source) of what the step records in `social_network_v1`, in its order.
    pub observed: Vec<(usize, usize)>,
    /// What `SocialPropagationResult` holds for its trace, if the Run keeps one.
    pub trace: Option<PlanTrace>,
}

#[derive(Default)]
pub struct PlanTrace {
    /// By recipient, source and basis, as `update` sorts them.
    pub contacts: Vec<SocialContact>,
    pub contributing: i64,
    pub total_added: f64,
}

/// What `ExpressivenessChannelOpennessPolicy.update` finds of each eligible contact.
#[derive(Default)]
struct ChannelDetails {
    weights: Vec<f64>,
    rate: f64,
    raw: Vec<f64>,
    capped: Vec<Num>,
    scale: f64,
}

/// What `DeGrootContactPolicy.update` finds of each neighbor.
#[derive(Default)]
struct DeGrootDetails {
    contributions: Vec<Num>,
    scale: f64,
    applied: f64,
}

#[track_caller]
fn checked<T>(value: Option<T>) -> Result<T> {
    value.ok_or_else(|| Error::unsupported("a minute does not fit"))
}

/// What `plan` reads for every person, shared by the threads of its compute.
struct Shared<'a> {
    minute: i64,
    last_minute: i64,
    cooldown_after: i64,
    settings: &'a Settings,
    people: &'a [Person<'a>],
    quoted: &'a [Vec<u8>],
    members: &'a HashMap<&'a str, Vec<usize>>,
    group_members: &'a HashMap<&'a str, Vec<usize>>,
    nearby_of_zone: &'a HashMap<&'a str, Vec<&'a [usize]>>,
    previous: &'a IdMap<ContactRecord>,
    cooldowns_before: &'a IdMap<i64>,
    /// The cooldowns as the step starts from them: each person reads and writes only their own.
    cooldowns: &'a BTreeMap<&'a str, i64>,
    draws: &'a KeyedDraws,
    traced: bool,
}

/// One person's part of the plan, which `plan` applies in the people's order.
struct PersonPlan {
    stress: f64,
    /// The new contacts of this recipient, in the order of Python's new dict.
    contacts: Vec<(String, ContactRecord)>,
    /// (recipient, source) of what the step records in `social_network_v1`.
    observed: Vec<(usize, usize)>,
    /// The person's cooldown, if the step sets it.
    cooldown: Option<i64>,
    /// The trace's contacts of this recipient, sorted by source.
    trace_contacts: Vec<SocialContact>,
    /// What goes to the trace's total, in the order the Python rule adds it.
    added: Vec<f64>,
    contributing: i64,
}

/// The compute of one recipient: their contacts and new stress. It reads only `shared`.
fn plan_person(shared: &Shared<'_>, me: usize) -> Result<PersonPlan> {
    let Shared { minute, last_minute, cooldown_after, settings, people, quoted, .. } = *shared;
    let person = &people[me];
    let mut buckets = Vec::new();
    if let Some(group) = person.group {
        let family = &shared.group_members[group];
        if family.len() > 1 {
            buckets.push(Bucket { basis: Basis::Family, members: family, includes_self: true });
        }
    }
    let same_zone = &shared.members[person.zone];
    if same_zone.len() > 1 {
        buckets.push(Bucket { basis: Basis::SameZone, members: same_zone, includes_self: true });
    }
    for &nearby in &shared.nearby_of_zone[person.zone] {
        buckets.push(Bucket { basis: Basis::Nearby, members: nearby, includes_self: false });
    }
    let mut selected = Vec::new();
    let mut head = key_head(label::SOCIAL_CONTACT_BUCKET, minute, &quoted[me]);
    select_contacts(shared.draws, &mut head, me, &buckets, settings.maximum, true, &mut selected);

    let traced = shared.traced;
    let mut cooldown = shared.cooldowns.get(person.id).copied();
    let mut set_cooldown = None;
    let mut out = PersonPlan {
        stress: 0.0,
        contacts: Vec::with_capacity(selected.len()),
        observed: Vec::new(),
        cooldown: None,
        trace_contacts: Vec::new(),
        added: Vec::new(),
        contributing: 0,
    };
    let mut lasting: Vec<(usize, Basis)> = Vec::new();
    let mut minutes_of: Vec<(usize, i64)> = Vec::new();
    let mut additions = 0.0;
    for &(source, basis) in &selected {
        let key = pair_key(&quoted[me], &quoted[source])?;
        let duration = match shared.previous.get(key.as_str()) {
            Some(record) if record.last_seen_minute == last_minute => {
                checked(record.consecutive_minutes.checked_add(1))?
            }
            _ => 1,
        };
        out.contacts.push((key, ContactRecord { last_seen_minute: minute, consecutive_minutes: duration }));
        let mut sparse_contribution = Num::Float(0.0);
        if duration >= settings.minimum_contact_minutes {
            lasting.push((source, basis));
            if traced {
                minutes_of.push((source, duration));
            }
            if settings.policy == Model::Sparse && minute >= cooldown.unwrap_or(0) {
                let difference = py_max(0.0, people[source].stress - person.stress);
                let contribution = min_num(
                    settings.pair_cap,
                    Num::Float(difference * settings.spread * duration as f64),
                );
                let remaining = py_max(
                    0.0,
                    py_min(
                        settings.agent_cap.value() - additions,
                        1.0 - py_max(0.0, person.base - settings.recovery) - additions,
                    ),
                );
                let kept = min_num(contribution, Num::Float(remaining));
                sparse_contribution = kept;
                let contribution = kept.value();
                if contribution > 0.0 {
                    cooldown = Some(cooldown_after);
                    set_cooldown = Some(cooldown_after);
                    additions += contribution;
                    if traced {
                        out.added.push(contribution);
                        out.contributing += 1;
                    }
                }
            }
        }
        if traced && settings.policy == Model::Sparse {
            out.trace_contacts.push(SocialContact {
                recipient: me,
                source,
                basis,
                minutes: duration,
                cooldown_remaining: (cooldown.unwrap_or(0) - minute).max(0),
                difference: py_max(0.0, people[source].stress - person.stress),
                contribution: sparse_contribution,
                weight: 0.0,
                aggregate_rate: 0.0,
                pre_pair_cap: 0.0,
                pair_cap: Num::Float(0.0),
                aggregate_scale: 1.0,
            });
        }
    }
    // Selected sources differ, and people are indexed in ID order: `contacts.sort`.
    if traced && settings.policy == Model::Sparse {
        out.trace_contacts.sort_by_key(|contact| contact.source);
    }

    let stress = match settings.policy {
        Model::Sparse => {
            // Every selected contact, by source (people are indexed in ID order).
            out.observed.extend(selected.iter().map(|&(source, _)| (me, source)));
            out.observed.sort_unstable();
            py_min(1.0, py_max(0.0, person.base - settings.recovery + additions))
        }
        Model::Channel => {
            lasting.sort_unstable_by_key(|contact| contact.0);
            out.observed.extend(lasting.iter().map(|&(source, _)| (me, source)));
            let can_contribute = minute >= shared.cooldowns_before.get(person.id).copied().unwrap_or(0);
            let mut details = traced.then(ChannelDetails::default);
            let (stress, shift) = channel_stress(
                settings,
                people,
                person,
                if can_contribute { &lasting } else { &[] },
                details.as_mut(),
            )?;
            if shift.value() != 0.0 {
                cooldown = Some(cooldown_after);
                set_cooldown = Some(cooldown_after);
            }
            if let Some(details) = &details {
                out.added.push(shift.value());
                out.contributing += i64::from(shift.value() != 0.0);
                let cooldown_remaining = (cooldown.unwrap_or(0) - minute).max(0);
                for (place, &(source, basis)) in lasting.iter().enumerate() {
                    let eligible = can_contribute && !details.weights.is_empty();
                    let raw = if eligible { details.raw[place] } else { 0.0 };
                    let capped = if eligible { details.capped[place] } else { Num::Float(0.0) };
                    out.trace_contacts.push(SocialContact {
                        recipient: me,
                        source,
                        basis,
                        minutes: minutes_of.iter().find(|item| item.0 == source).map_or(0, |item| item.1),
                        cooldown_remaining,
                        difference: people[source].stress - person.stress,
                        contribution: Num::Float(if eligible { capped.value() * details.scale } else { 0.0 }),
                        weight: if eligible { details.weights[place] } else { 0.0 },
                        aggregate_rate: if eligible { details.rate } else { 0.0 },
                        pre_pair_cap: raw,
                        pair_cap: capped,
                        aggregate_scale: if eligible { details.scale } else { 1.0 },
                    });
                }
            }
            stress
        }
        Model::DeGroot => {
            lasting.sort_unstable_by_key(|contact| contact.0);
            out.observed.extend(lasting.iter().map(|&(source, _)| (me, source)));
            let mut details = traced.then(DeGrootDetails::default);
            let stress = degroot_stress(settings, people, person, &lasting, details.as_mut());
            if let Some(details) = &details {
                out.added.push(details.applied);
                for (place, &(source, basis)) in lasting.iter().enumerate() {
                    let contribution = details.contributions[place].value() * details.scale;
                    out.contributing += i64::from(contribution != 0.0);
                    out.trace_contacts.push(SocialContact {
                        recipient: me,
                        source,
                        basis,
                        minutes: minutes_of.iter().find(|item| item.0 == source).map_or(0, |item| item.1),
                        cooldown_remaining: 0,
                        difference: people[source].stress - person.stress,
                        contribution: Num::Float(contribution),
                        weight: 0.0,
                        aggregate_rate: 0.0,
                        pre_pair_cap: 0.0,
                        pair_cap: Num::Float(0.0),
                        aggregate_scale: 1.0,
                    });
                }
            }
            stress
        }
    };
    // `replace` of the Python rule checks the value it stores.
    if !(0.0..=1.0).contains(&stress) {
        return unsupported("a new stress is outside 0..1");
    }
    out.stress = stress;
    out.cooldown = set_cooldown;
    Ok(out)
}

/// The plan of a minute from plain values: the people sorted by ID, the neighbors of their zones,
/// and the state before (empty on the first tick). Changes nothing.
#[allow(clippy::too_many_arguments)]
pub fn plan(
    seed: &str,
    minute: i64,
    settings: &Settings,
    people: &[Person<'_>],
    neighbors: &HashMap<String, Vec<String>>,
    previous: &IdMap<ContactRecord>,
    cooldowns_before: &IdMap<i64>,
    traced: bool,
) -> Result<Plan> {
    if previous.values().any(|record| record.last_seen_minute < 0 || record.consecutive_minutes < 0) {
        return unsupported("a contact field is negative");
    }
    if cooldowns_before.values().any(|&due| due < 0) {
        return unsupported("a cooldown is negative");
    }
    let last_minute = checked(minute.checked_sub(1))?;
    let cooldown_after = checked(minute.checked_add(settings.cooldown_minutes))?;

    let ids: Vec<&str> = people.iter().map(|person| person.id).collect();
    let zones: Vec<&str> = people.iter().map(|person| person.zone).collect();
    let quoted = quoted_ids(&ids);
    let members = zone_members(&zones);
    let mut group_members: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, person) in people.iter().enumerate() {
        if let Some(group) = person.group {
            group_members.entry(group).or_default().push(index);
        }
    }
    let nearby_of_zone: HashMap<&str, Vec<&[usize]>> = members
        .keys()
        .map(|&zone| (zone, nearby_buckets(zone, neighbors, &members)))
        .collect();

    let keep_all = settings.policy == Model::Sparse;
    let mut cooldowns: BTreeMap<&str, i64> = cooldowns_before
        .iter()
        .filter(|(_, &due)| keep_all || due > minute)
        .map(|(id, &due)| (&**id, due))
        .collect();
    if settings.policy == Model::DeGroot {
        cooldowns.clear();
    }

    // The compute, each person on any thread; then the plan, in the people's order.
    let draws = KeyedDraws::new(seed);
    let shared = Shared {
        minute,
        last_minute,
        cooldown_after,
        settings,
        people,
        quoted: &quoted,
        members: &members,
        group_members: &group_members,
        nearby_of_zone: &nearby_of_zone,
        previous,
        cooldowns_before,
        cooldowns: &cooldowns,
        draws: &draws,
        traced,
    };
    let planned = in_order(people.len(), |me| plan_person(&shared, me));
    let mut stresses = Vec::with_capacity(people.len());
    let mut contacts = IdMap::with_capacity_and_hasher(people.len() * 3, FxBuildHasher);
    let mut observed = Vec::with_capacity(people.len() * 3);
    let mut trace = traced.then(PlanTrace::default);
    let mut new_cooldowns = Vec::new();
    for (person, planned) in people.iter().zip(planned) {
        let planned = planned?;
        if let Some(due) = planned.cooldown {
            new_cooldowns.push((person.id, due));
        }
        for (key, record) in planned.contacts {
            contacts.insert(Arc::from(key), record);
        }
        observed.extend(planned.observed);
        if let Some(trace) = &mut trace {
            trace.contacts.extend(planned.trace_contacts);
            for added in planned.added {
                trace.total_added += added;
            }
            trace.contributing += planned.contributing;
        }
        stresses.push(planned.stress);
    }
    drop(shared);
    cooldowns.extend(new_cooldowns);

    let new_cooldowns: Vec<(&str, i64)> =
        cooldowns.into_iter().filter(|&(_, due)| due > minute).collect();
    let unchanged = cooldowns_before.len() == new_cooldowns.len()
        && cooldowns_before
            .iter()
            .zip(&new_cooldowns)
            .all(|((id, due), (new_id, new_due))| &**id == *new_id && due == new_due);
    let cooldowns = (!unchanged).then(|| {
        new_cooldowns.into_iter().map(|(id, due)| (Arc::from(id), due)).collect::<IdMap<i64>>()
    });
    Ok(Plan { stresses, contacts, cooldowns, observed, trace })
}

/// The new stress and the shift of `ExpressivenessChannelOpennessPolicy.update`. `eligible` are by
/// source.
fn channel_stress(
    settings: &Settings,
    people: &[Person<'_>],
    person: &Person<'_>,
    eligible: &[(usize, Basis)],
    details: Option<&mut ChannelDetails>,
) -> Result<(f64, Num)> {
    let weights: Vec<f64> = eligible
        .iter()
        .map(|&(source, basis)| {
            let channel = match basis {
                Basis::Family => settings.family_strength,
                Basis::SameZone => settings.zone_strength,
                Basis::Nearby => settings.nearby_strength,
            };
            people[source].expressiveness * channel * person.sensitivity
        })
        .collect();
    let weight_total = py_fsum(&weights).ok_or_else(|| Error::unsupported("weights are not finite"))?;
    // Weights are products of factors in 0..1, so a total of 0 means every contribution is 0:
    // no shift, and `details` keep no weights, so the trace has each contact as one without a
    // share, as the Python rule has it (#87, 元 #301).
    let mut shift = Num::Float(0.0);
    if weight_total > 0.0 {
        let gamma = py_min(1.0, settings.spread * weight_total);
        let raw: Vec<f64> = eligible
            .iter()
            .zip(&weights)
            .map(|(&(source, _), &weight)| gamma * weight / weight_total * (people[source].stress - person.stress))
            .collect();
        let pair_shifts: Vec<Num> = raw
            .iter()
            .map(|&value| max_num(settings.pair_cap.neg(), min_num(settings.pair_cap, Num::Float(value))))
            .collect();
        // math.fsum reads an int as its float.
        let pair_values: Vec<f64> = pair_shifts.iter().map(|value| value.value()).collect();
        let raw_shift = py_fsum(&pair_values).ok_or_else(|| Error::unsupported("shifts are not finite"))?;
        shift = max_num(settings.agent_cap.neg(), min_num(settings.agent_cap, Num::Float(raw_shift)));
        shift = max_num(Num::Float(-person.base), min_num(Num::Float(1.0 - person.base), shift));
        if let Some(details) = details {
            details.scale = if raw_shift != 0.0 { shift.value() / raw_shift } else { 0.0 };
            details.rate = gamma;
            details.weights = weights;
            details.raw = raw;
            details.capped = pair_shifts;
        }
    }
    Ok((py_min(1.0, py_max(0.0, person.base + shift.value())), shift))
}

/// The new stress of `DeGrootContactPolicy.update`. `neighbors` are by source.
fn degroot_stress(
    settings: &Settings,
    people: &[Person<'_>],
    person: &Person<'_>,
    neighbors: &[(usize, Basis)],
    details: Option<&mut DeGrootDetails>,
) -> f64 {
    let alpha = py_min(1.0, settings.spread);
    let count = neighbors.len().max(1) as f64;
    let contributions: Vec<Num> = neighbors
        .iter()
        .map(|&(source, _)| {
            let value = alpha * (people[source].stress - person.stress) / count;
            max_num(settings.pair_cap.neg(), min_num(settings.pair_cap, Num::Float(value)))
        })
        .collect();
    // `sum` from the int 0: an empty sum, or one of int caps, stays the int 0. The caps are at
    // most 0.25, so no int leaves i64.
    let raw_shift = sum_nums(&contributions).unwrap_or(Num::Float(f64::NAN));
    let shift = max_num(settings.agent_cap.neg(), min_num(settings.agent_cap, raw_shift));
    let recovered = py_max(0.0, person.base - settings.recovery);
    let applied = py_min(1.0, py_max(0.0, recovered + shift.value())) - recovered;
    if let Some(details) = details {
        details.scale = if raw_shift.value() != 0.0 { applied / raw_shift.value() } else { 1.0 };
        details.applied = applied;
        details.contributions = contributions;
    }
    recovered + applied
}

// ---- The step ------------------------------------------------------------------------------------

/// `controller.zone_neighbors` as the PyO3 crate read it: by zone, the neighbors as strs or the
/// refusal of that zone's value, which the step raises only for a zone somebody is in; or the
/// refusal of a table that is not a dict.
pub type Neighbors = std::result::Result<HashMap<String, std::result::Result<Vec<String>, &'static str>>, &'static str>;

/// What the step reads besides the rows.
pub struct Inputs<'a> {
    pub settings: &'a Settings,
    /// `source_expressiveness_by_agent`, which the channel model reads.
    pub expressiveness: &'a Table,
    pub view: &'a ViewData,
    /// The psychology deltas of the minute, each checked to be of it.
    pub deltas: &'a [Delta],
    pub minute: i64,
    /// `str(rng.seed)` of the step's `CounterRNG`.
    pub seed: &'a str,
    pub neighbors: &'a Neighbors,
    /// The Run keeps a trace.
    pub traced: bool,
}

/// A minute of the social model, planned: the new deltas and what is to be written.
pub struct Planned {
    /// The place in the input of each delta, in ID order.
    pub order: Vec<usize>,
    /// The people's IDs, in ID order.
    pub ids: Vec<Arc<str>>,
    /// The plan; its contacts and cooldowns are taken out when it is written.
    pub plan: Plan,
    observed: Option<ObservedContacts>,
    /// The namespace is not in `model_state`: the PyO3 crate stores the empty one, then calls
    /// `write`. False once written.
    pub first_use: bool,
    written: bool,
}

impl Planned {
    /// The deltas after the social model, in ID order: each base delta with its new stress.
    pub fn deltas(&self, base: &[Delta]) -> Vec<Delta> {
        self.order
            .iter()
            .zip(&self.plan.stresses)
            .map(|(&place, &stress)| Delta { stress, ..base[place].clone() })
            .collect()
    }

    /// Whether the plan is written.
    pub fn is_written(&self) -> bool {
        self.written
    }
}

/// The neighbors of the people's zones, or the refusal of the first zone whose value the step
/// cannot read (people in ID order).
fn neighbors_of(neighbors: &Neighbors, zones: &[&str]) -> Result<HashMap<String, Vec<String>>> {
    let table = neighbors.as_ref().map_err(|reason| Error::unsupported(*reason))?;
    let mut found = HashMap::with_capacity(zones.len());
    for &zone in zones {
        if found.contains_key(zone) {
            continue;
        }
        match table.get(zone) {
            None => {}
            Some(Ok(names)) => {
                found.insert(zone.to_owned(), names.clone());
            }
            Some(Err(reason)) => return Err(Error::unsupported(*reason)),
        }
    }
    Ok(found)
}

/// `_social_propagation` of the update phase, with the model on: the plan of the minute and, when
/// the namespace is in `model_state`, its writes. Unsupported before anything changes.
pub fn update(world: &World, inputs: &Inputs<'_>) -> Result<Planned> {
    let settings = inputs.settings;
    let view = inputs.view;
    let deltas = inputs.deltas;
    if view.agents.len() != deltas.len() {
        return unsupported("the view and the deltas have different people");
    }
    // Each person's group and expressiveness, from the view and the policy.
    let mut found = Vec::with_capacity(deltas.len());
    for (place, delta) in deltas.iter().enumerate() {
        let Some(agent) = view.index_of(&delta.agent_id) else {
            return unsupported("the view and the deltas have different people");
        };
        let expressiveness = if settings.policy == Model::Channel && !inputs.expressiveness.is_empty() {
            match inputs.expressiveness.get(&*delta.agent_id) {
                Some(value) => number_of(value)?.value(),
                None => 1.0,
            }
        } else {
            1.0
        };
        if !(0.0..=1.0).contains(&delta.stress) {
            return unsupported("a stress is outside 0..1");
        }
        found.push((place, view.agents[agent].group_id.as_deref(), expressiveness));
    }
    let id_of = |item: &(usize, Option<&str>, f64)| deltas[item.0].agent_id.as_bytes();
    if !found.windows(2).all(|pair| id_of(&pair[0]) < id_of(&pair[1])) {
        // Deltas come sorted. The Python rules sort them, except DeGroot's, which keeps their order.
        if settings.policy == Model::DeGroot {
            return unsupported("the deltas are not in id order");
        }
        found.sort_unstable_by(|first, second| id_of(first).cmp(id_of(second)));
        if found.windows(2).any(|pair| id_of(&pair[0]) == id_of(&pair[1])) {
            return unsupported("two deltas for one person");
        }
    }

    // The entity rows.
    let rows = {
        let rows = world.rows()?;
        if rows.agents.len() != found.len() {
            return unsupported("the view and the world have different people");
        }
        let mut people_rows = Vec::with_capacity(found.len());
        for item in &found {
            let id = &*deltas[item.0].agent_id;
            let Some(row) = rows.agents.get(id) else {
                return unsupported("the view and the world have different people");
            };
            if &*row.profile.id != id {
                return unsupported("a person's key is not their id");
            }
            if !(0.0..=1.0).contains(&row.stress) || !row.profile.stress_sensitivity.is_finite() {
                return unsupported("a stress is outside 0..1");
            }
            people_rows.push((row.zone_id.clone(), row.stress, row.profile.stress_sensitivity));
        }
        people_rows
    };
    let zones: Vec<&str> = rows.iter().map(|(zone, _, _)| &**zone).collect();
    let neighbors = neighbors_of(inputs.neighbors, &zones)?;
    let people: Vec<Person<'_>> = found
        .iter()
        .zip(&rows)
        .map(|(&(place, group, expressiveness), (zone, stress, sensitivity))| Person {
            id: &deltas[place].agent_id,
            group,
            zone,
            stress: *stress,
            sensitivity: *sensitivity,
            expressiveness,
            base: deltas[place].stress,
        })
        .collect();

    // The first tick of a Run: `_read_runtime_state(None)`, and this step makes the namespace.
    let first_use = kit::namespace_absent(world, NAME)?;
    let plan = if first_use {
        plan(inputs.seed, inputs.minute, settings, &people, &neighbors, &IdMap::default(), &IdMap::default(), inputs.traced)?
    } else {
        let row = world.row::<SocialRow>(NAME)?;
        if row.schema_version != SCHEMA_VERSION {
            return unsupported("another schema version");
        }
        plan(inputs.seed, inputs.minute, settings, &people, &neighbors, &row.contacts, &row.agent_cooldown_until, inputs.traced)?
    };
    // What it records in `social_network_v1`, when that namespace is there, checked now so that its
    // refusals come before any write.
    let observed = if kit::namespace_absent(world, SOCIAL_NETWORK)? {
        None
    } else {
        let pairs: Vec<(&str, &str)> =
            plan.observed.iter().map(|&(recipient, source)| (people[recipient].id, people[source].id)).collect();
        Some(social_network::observed_contacts(world, &pairs, inputs.minute)?)
    };
    let ids = people.iter().map(|person| Arc::from(person.id)).collect();
    let mut planned =
        Planned { order: found.iter().map(|item| item.0).collect(), ids, plan, observed, first_use, written: false };
    if !first_use {
        write(world, &mut planned)?;
    }
    Ok(planned)
}

/// The writes of a plan: the row of `social_propagation_v1`, which must be in `model_state` now,
/// then what the step records in `social_network_v1`, in the order the Python step wrote them.
pub fn write(world: &World, planned: &mut Planned) -> Result<()> {
    if planned.written {
        return Err(Error::Runtime("the plan is written already".to_owned()));
    }
    {
        let mut row = world.row_mut::<SocialRow>(NAME)?;
        if let Some(cooldowns) = planned.plan.cooldowns.take() {
            row.agent_cooldown_until = cooldowns;
        }
        row.contacts = std::mem::take(&mut planned.plan.contacts);
    }
    planned.written = true;
    planned.first_use = false;
    match &planned.observed {
        Some(observed) => observed.apply(world),
        None => Ok(()),
    }
}

/// The trace's document of a minute (`SocialPropagationResult.trace_document`) to the host's sink,
/// when the plan keeps a trace. `minute` and `policy_version` are the values the document holds.
pub fn record_social<H: Host>(host: &mut H, planned: &Planned, minute: &V, policy_version: &V) -> std::result::Result<(), H::Error> {
    let Some(trace) = &planned.plan.trace else {
        return Ok(());
    };
    let ids: Vec<&str> = planned.ids.iter().map(|id| &**id).collect();
    let document = social_document(minute, policy_version, &ids, &trace.contacts, trace.contributing, trace.total_added);
    host.trace(Trace::SocialContact(document))
}
