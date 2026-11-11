//! The sampling of `DistanceSampledProximityPolicy` (`policies/proximity.py`) and the step that
//! schedules what it finds, `_sample_proximity_contacts` (`experiments/phases/sample.py`) (lane c6 of
//! Issue #152).
//!
//! The controller's config and policy are the Run's (`run::proximity::Proximity`, read when the
//! controller starts; a test that puts another policy on the controller reads them again with
//! `refresh_policies`). The positions come from the row of `movement_v1`: a namespace the world does
//! not hold has nobody placed, and a row a value did not fit is refused (there is no tree to read
//! instead). A coordinate whose cell does not fit an i64 says Unsupported, as `math.floor` gives an
//! exact int of any size; validation does not bound coordinates, but no floor plan reaches them.
//!
//! The step is two parts: `when` reads only the Run's config and the minute, so that a minute off
//! the interval borrows no world; `sample_contacts` schedules the contacts of a minute that samples.

use std::sync::Arc;

use indexmap::IndexMap;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::error::{Error, Result};
use crate::events::writer::WriteResult;
use crate::events::{EventKind, EventRow};
use crate::mathx::hypot;
use crate::rules::kit::movement::KEY as MOVEMENT;
use crate::rules::kit::{self, held_row, Events, Held};
use crate::run::proximity::Proximity;
use crate::state::movement::MovementRow;
use crate::value::V;
use crate::world::World;

const SOURCE_ID: &str = "ASM-PROXIMITY-CONTACT-V1";

#[track_caller]
fn refuse<T>(reason: &'static str) -> Result<T> {
    Err(Error::unsupported(reason))
}

/// `config[key]` of a dict config: its KeyError for a key it does not have.
fn item<'a>(config: &'a V, key: &str) -> Result<&'a V> {
    let V::Map(config) = config else {
        return refuse("the proximity config is not a dict");
    };
    config.get(key).ok_or_else(|| Error::Key(key.to_owned()))
}

/// A person the sampler sees, as `points` holds them.
#[derive(Clone, Debug, PartialEq)]
pub struct Placed {
    pub id: Arc<str>,
    pub zone: Arc<str>,
    pub x: f64,
    pub y: f64,
}

/// The positions the head of `sample` keeps from the row: `(agent, zone, x, y)`.
fn placed(world: &World, movement: &Held) -> Result<Vec<Placed>> {
    let Some(row) = held_row::<MovementRow>(world, MOVEMENT, movement)? else {
        // `world.model_state.get(MOVEMENT_STATE_KEY)` is no dict: nobody is placed.
        return Ok(Vec::new());
    };
    let mut found = Vec::with_capacity(row.positions.len());
    for (id, position) in row.positions.iter() {
        if &*position.position_kind != "physical" {
            continue;
        }
        if let Some(point) = &position.point {
            found.push(Placed { id: id.clone(), zone: position.zone_id.clone(), x: point.x, y: point.y });
        }
    }
    Ok(found)
}

/// The cell of a coordinate: `math.floor(value / radius)`.
fn cell(value: f64, radius: f64) -> Result<i64> {
    const LIMIT: f64 = (1_u64 << 62) as f64;
    let floor = (value / radius).floor();
    if floor.abs() < LIMIT {
        Ok(floor as i64)
    } else {
        refuse("a coordinate's cell does not fit an i64")
    }
}

/// `(distance, first, second)`, the indexes in `points`, sorted as `sample` sorts them and cut to
/// the limit. `points` is in ID order, so comparing indexes is comparing IDs.
fn pairs_within(points: &[Placed], radius: f64, limit: usize) -> Result<Vec<(f64, usize, usize)>> {
    let mut cells: FxHashMap<(&str, i64, i64), Vec<usize>> = FxHashMap::default();
    let mut places = Vec::with_capacity(points.len());
    for (index, point) in points.iter().enumerate() {
        let place = (cell(point.x, radius)?, cell(point.y, radius)?);
        places.push(place);
        cells.entry((&point.zone, place.0, place.1)).or_default().push(index);
    }
    let mut candidates = Vec::new();
    let mut visited: FxHashSet<(usize, usize)> = FxHashSet::default();
    for (index, point) in points.iter().enumerate() {
        let (cell_x, cell_y) = places[index];
        for offset_x in [-1, 0, 1] {
            for offset_y in [-1, 0, 1] {
                let Some(others) = cells.get(&(&*point.zone, cell_x + offset_x, cell_y + offset_y)) else {
                    continue;
                };
                for &other in others {
                    if other == index {
                        continue;
                    }
                    let pair = (index.min(other), index.max(other));
                    if !visited.insert(pair) {
                        continue;
                    }
                    let (first, second) = (&points[pair.0], &points[pair.1]);
                    let distance = hypot(&[second.x - first.x, second.y - first.y]);
                    if distance <= radius {
                        candidates.push((distance, pair.0, pair.1));
                    }
                }
            }
        }
    }
    candidates.sort_unstable_by(|first, second| first.0.total_cmp(&second.0).then((first.1, first.2).cmp(&(second.1, second.2))));
    candidates.truncate(limit);
    Ok(candidates)
}

/// What `policy.sample(world)` finds, given the policy's config: `points`, and the pairs. `movement`
/// says whether the world holds `movement_v1`.
pub fn sample(world: &World, policy_config: &V, movement: &Held) -> Result<(Vec<Placed>, Vec<(f64, usize, usize)>)> {
    let radius = match item(policy_config, "max_distance_m")? {
        V::Float(number) => *number,
        V::Int(number) => *number as f64,
        V::Big(_) => return refuse("the proximity radius is too large"),
        _ => return refuse("the proximity radius is not a number"),
    };
    if !(radius.is_finite() && radius > 0.0) {
        return refuse("the proximity radius is not positive");
    }
    let limit = match item(policy_config, "max_pairs_per_sample")? {
        V::Int(limit) if *limit >= 0 => *limit as usize,
        V::Int(_) | V::Big(_) => return refuse("the proximity limit is out of range"),
        _ => return refuse("the proximity limit is not an int"),
    };
    let placed = placed(world, movement)?;
    let rows = world.rows()?;
    if rows.zones_tainted().is_some() {
        return refuse(crate::rules::travel::ZONES_TAINTED);
    }
    let mut points = Vec::with_capacity(placed.len());
    for point in placed {
        // The checks in the order of the Python rule's `or`; none has an effect.
        if !point.x.is_finite() || !point.y.is_finite() {
            continue;
        }
        if !rows.agents.contains_key(&point.id) || !rows.zones.contains_key(&point.zone) {
            continue;
        }
        points.push(point);
    }
    drop(rows);
    points.sort_unstable_by(|first, second| first.id.cmp(&second.id));
    let pairs = pairs_within(&points, radius, limit)?;
    Ok((points, pairs))
}

fn point_value(point: &Placed) -> V {
    let mut map = IndexMap::with_capacity(2);
    map.insert(Arc::from("x"), V::Float(point.x));
    map.insert(Arc::from("y"), V::Float(point.y));
    V::Map(map)
}

/// The head of `_sample_proximity_contacts`, which reads no world: the minute to sample, or None for
/// no config or a minute off its interval. `minute` is None for a minute that is not an int.
pub fn when(proximity: &Proximity, minute: Option<i64>) -> Result<Option<i64>> {
    let config = kit::policy(proximity.config.as_ref())?;
    if matches!(config, V::Null) {
        return Ok(None);
    }
    let Some(minute) = minute else {
        return refuse("the minute is not an int");
    };
    let V::Int(interval) = item(config, "sample_interval_minutes")? else {
        return refuse("the sampling interval is not an int");
    };
    if *interval <= 0 {
        return refuse("the sampling interval is not positive");
    }
    if minute.rem_euclid(*interval) != 0 {
        return Ok(None);
    }
    Ok(Some(minute))
}

/// `_sample_proximity_contacts` at a minute `when` gave: a `proximity_contact_update` for each pair,
/// due at the next minute. `movement` says whether the world holds `movement_v1`.
pub fn sample_contacts(world: &World, proximity: &Proximity, minute: i64, movement: &Held) -> WriteResult<()> {
    let config = kit::policy(proximity.config.as_ref())?;
    let events = Events::of(world)?;
    let Some(due_minute) = minute.checked_add(1) else {
        return Err(Error::unsupported("a minute or a sequence is negative").into());
    };
    if due_minute >= events.duration() {
        return Ok(());
    }
    if !*kit::policy(proximity.known.as_ref())? {
        return Err(Error::unsupported("another proximity policy").into());
    }
    let V::Str(version) = item(config, "policy_version")? else {
        return Err(Error::unsupported("the proximity policy's version is not a str").into());
    };
    if due_minute < 0 || events.next_sequence()? < 0 {
        return Err(Error::unsupported("a minute or a sequence is negative").into());
    }
    let Some(policy) = kit::policy(proximity.policy.as_ref())? else {
        return Err(Error::unsupported("another proximity policy").into());
    };
    let policy_config = kit::policy(policy.value("config"))?;
    let (points, pairs) = sample(world, policy_config, movement)?;
    let source: Arc<str> = Arc::from(SOURCE_ID);
    for (distance, first, second) in pairs {
        let (first, second) = (&points[first], &points[second]);
        let sequence = events.next_sequence()?;
        let mut payload = IndexMap::with_capacity(8);
        payload.insert(Arc::from("first_agent_id"), V::Str(first.id.clone()));
        payload.insert(Arc::from("second_agent_id"), V::Str(second.id.clone()));
        payload.insert(Arc::from("zone_id"), V::Str(first.zone.clone()));
        payload.insert(Arc::from("observed_minute"), V::Int(due_minute));
        payload.insert(Arc::from("contact_minutes"), V::Int(0));
        payload.insert(Arc::from("distance_m"), V::Float(distance));
        payload.insert(Arc::from("first_point"), point_value(first));
        payload.insert(Arc::from("second_point"), point_value(second));
        let mut provenance = IndexMap::with_capacity(4);
        provenance.insert(Arc::from("source_type"), V::Str(Arc::from("assumption")));
        provenance.insert(Arc::from("source_id"), V::Str(source.clone()));
        provenance.insert(Arc::from("policy_version"), V::Str(version.clone()));
        provenance.insert(Arc::from("observed_minute"), V::Int(due_minute));
        let row = EventRow::new(
            Arc::from(format!("proximity-contact-{sequence}")),
            EventKind::Endogenous,
            due_minute,
            40,
            sequence,
            Arc::from("proximity_contact_update"),
            payload,
            Some(source.clone()),
            None,
            provenance,
        );
        // Finite values, stable IDs and non-negative numbers, all checked above.
        let Some(row) = row else {
            return Err(Error::Runtime("a proximity contact is not an event".to_owned()).into());
        };
        events.allocate_sequence()?;
        events.schedule(row)?;
    }
    Ok(())
}
