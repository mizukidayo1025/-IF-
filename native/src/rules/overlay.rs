//! `domain/temporary_spaces.py` and `domain/geometry_overlay.py`: the glue of the core's kernels
//! (`sheltersim_core::rules::overlay`, lane c4 of Issue #152). Each entry reads its arguments and
//! what Python holds (whether `model_state` holds each namespace, the sections of the configs, the
//! floor plan), runs the core on the world borrowed whole, and makes the Python values of what it
//! returns. The core writes the rows of `temporary_spaces_v1` and `geometry_overlay_v1`, a
//! facility's `zone_id` and an edge's `closed`; the attached trees and the objects follow them when
//! the world is given back. The pydantic contracts of a geometry change's parameters go through the
//! native host (`Host::contract`, policy E15).
//!
//! A floor plan that `Geo::of` could not read is passed as none, and where the core reads it, it
//! says `PLAN_UNREAD`: the glue raises what `Geo::of` raised instead, as the rule raised it there.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyString, PyTuple};

use sheltersim_core::config::movement::Movement;
use sheltersim_core::config::overlay::overlay_schema as parse_overlay_schema;
use sheltersim_core::config::service::ResourceService;
use sheltersim_core::config::SectionId;
use sheltersim_core::host::Holding;
use sheltersim_core::rules::kit::{Held, Kept};
use sheltersim_core::rules::overlay::{self as core, Beds, Change, Intake, Places, OVERLAY_KEY, PLAN_UNREAD, SPACES_KEY};

pub(crate) use sheltersim_core::rules::overlay::{Bed, Capacity};

use crate::core::configs;
use crate::core::errors::OrPy;
use crate::core::row::{attribute, exact_dict, exact_int, exact_str, str_value, unsupported};
use crate::core::value::{V, VPy};
use crate::core::world_core::{core_of, raw_dict_get, state_value, with_world, PyWorldCell};
use crate::host::PyHost;
use crate::rules::movement::point_object;
use crate::state::entities::rows_follow_maps;
use crate::rules::movement_geometry::{Geo, GeoPy, Pt};
use crate::state::events::{EventRow, EventRowPy};
use crate::Unsupported;

// ---- What ran and what was refused --------------------------------------------------------------

static RAN: Mutex<BTreeMap<&'static str, u64>> = Mutex::new(BTreeMap::new());
static REFUSALS: Mutex<BTreeMap<String, u64>> = Mutex::new(BTreeMap::new());

fn counted<T>(py: Python<'_>, label: &'static str, result: PyResult<T>) -> PyResult<T> {
    match &result {
        Ok(_) => {
            if let Ok(mut ran) = RAN.lock() {
                *ran.entry(label).or_insert(0) += 1;
            }
        }
        Err(error) if error.is_instance_of::<Unsupported>(py) => {
            if let Ok(mut refusals) = REFUSALS.lock() {
                *refusals.entry(format!("{label}: {}", error.value(py))).or_insert(0) += 1;
            }
        }
        Err(_) => {}
    }
    result
}

#[pyfunction]
fn stats<'py>(py: Python<'py>) -> PyResult<(Bound<'py, PyDict>, Bound<'py, PyDict>)> {
    let counts = PyDict::new(py);
    if let Ok(ran) = RAN.lock() {
        for (label, times) in ran.iter() {
            counts.set_item(*label, *times)?;
        }
    }
    let reasons = PyDict::new(py);
    if let Ok(refusals) = REFUSALS.lock() {
        for (reason, times) in refusals.iter() {
            reasons.set_item(reason, *times)?;
        }
    }
    Ok((counts, reasons))
}

// ---- What Python holds ----------------------------------------------------------------------------

pub(crate) fn spaces_held(holding: &Holding) -> Held {
    holding.held(SPACES_KEY, "the world holds another temporary spaces state")
}

pub(crate) fn overlay_held(holding: &Holding) -> Held {
    holding.held(OVERLAY_KEY, "the world holds another overlay state")
}

fn movement_held(holding: &Holding) -> Held {
    holding.held(crate::rules::movement::KEY, "the world holds another movement state")
}

/// The holding read now, through the native host of an entry.
fn holding_now(world: &Bound<'_, PyAny>) -> PyResult<Arc<Holding>> {
    PyHost::of_entry(world).holding_now()
}

/// `config.get("schema_version")` of the overlay's config, for a dict config: the section
/// `overlay_schema` (`config::overlay`), kept for the stamp of `model_configs` and read through the
/// core's reader (`kit::section_value`); its refusal is said where the rule reads it.
pub(crate) fn schema_of(host: &mut PyHost<'_>, holding: &Holding) -> PyResult<Kept<Option<i64>>> {
    host.value_section(holding, SectionId::OverlaySchema, |configs| &configs.overlay_schema, parse_overlay_schema)
}

/// `schema_of` through the native host of an entry, with the holding read now.
pub(crate) fn schema_now(world: &Bound<'_, PyAny>) -> PyResult<Kept<Option<i64>>> {
    let mut host = PyHost::of_entry(world);
    let holding = host.holding_now()?;
    schema_of(&mut host, &holding)
}

/// `schema_of`, refused here, for the sections' tests.
#[track_caller]
pub(crate) fn overlay_schema(world: &Bound<'_, PyAny>) -> PyResult<Option<i64>> {
    Ok(*configs::read(schema_now(world)?)?)
}

/// The floor plan as `Geo::of` read it, or not read: a kernel whose overlay is absent never reads it.
enum Plan {
    Unread,
    Read(PyResult<Arc<Geo>>),
}

impl Plan {
    fn of(world: &Bound<'_, PyAny>) -> Self {
        Plan::Read(Geo::of(world))
    }

    /// Read only where the rule can reach it: with the overlay.
    fn with(world: &Bound<'_, PyAny>, overlay: &Held) -> Self {
        if matches!(overlay, Held::Absent) {
            Plan::Unread
        } else {
            Self::of(world)
        }
    }

    fn get(&self) -> Option<&Geo> {
        match self {
            Plan::Read(Ok(geo)) => Some(geo),
            _ => None,
        }
    }

    /// The kernel's result, with what `Geo::of` raised where the kernel read the plan.
    fn finish<T>(&self, py: Python<'_>, result: PyResult<T>) -> PyResult<T> {
        match (result, self) {
            (Err(error), Plan::Read(Err(read)))
                if error.is_instance_of::<Unsupported>(py) && error.value(py).to_string() == PLAN_UNREAD =>
            {
                Err(read.clone_ref(py))
            }
            (result, _) => result,
        }
    }
}

// ---- domain/temporary_spaces.py -----------------------------------------------------------------

#[pyfunction]
#[pyo3(signature = (world, record, *, event_id))]
fn register_temporary_space(world: &Bound<'_, PyAny>, record: &Bound<'_, PyAny>, event_id: &Bound<'_, PyAny>) -> PyResult<()> {
    let result = (|| {
        let event_id = exact_str(event_id)?;
        let held = spaces_held(&*holding_now(world)?);
        let record = <V as VPy>::from_py(record)?;
        with_world(world, |world| core::register_space(world, &held, record, event_id))?.or_py()
    })();
    counted(world.py(), "register_temporary_space", result)
}

#[pyfunction]
#[pyo3(signature = (world, space_id, *, event_id, causation_id))]
fn expire_temporary_space(
    world: &Bound<'_, PyAny>,
    space_id: &Bound<'_, PyAny>,
    event_id: &Bound<'_, PyAny>,
    causation_id: &Bound<'_, PyAny>,
) -> PyResult<bool> {
    let result = (|| {
        let cause = if causation_id.is_none() { None } else { Some(exact_str(causation_id)?) };
        let (space_id, event_id) = (exact_str(space_id)?, exact_str(event_id)?);
        let held = spaces_held(&*holding_now(world)?);
        with_world(world, |world| core::expire_space(world, &held, space_id, event_id, cause))?.or_py()
    })();
    counted(world.py(), "expire_temporary_space", result)
}

/// `validate_temporary_spaces_state`: true only for what the Python rule passes.
#[pyfunction]
fn validate_spaces(world: &Bound<'_, PyAny>) -> PyResult<bool> {
    let result = (|| {
        let held = spaces_held(&*holding_now(world)?);
        with_world(world, |world| core::validate_spaces(world, &held))?.or_py()
    })();
    counted(world.py(), "validate_spaces", result)
}

/// What `capacity_metrics` reads of the Python world.
struct PlacesIn {
    spaces: Held,
    overlay: Held,
    schema: Kept<Option<i64>>,
}

impl PlacesIn {
    fn of(world: &Bound<'_, PyAny>) -> PyResult<Self> {
        let mut host = PyHost::of_entry(world);
        let holding = host.holding_now()?;
        Ok(Self { spaces: spaces_held(&holding), overlay: overlay_held(&holding), schema: schema_of(&mut host, &holding)? })
    }

    fn places(&self) -> Places<'_> {
        Places { spaces: &self.spaces, overlay: &self.overlay, schema: &self.schema }
    }
}

/// `capacity_metrics` of a zone with a capacity, for a rule in another file. A world the core does
/// not hold as it is (a save's world being validated before its core, a `model_state` that is
/// another mapping or holds another value) is counted from the values Python holds.
pub(crate) fn capacity_of(world: &Bound<'_, PyAny>, zone_id: &str, nominal: i64, occupancy: Option<i64>) -> PyResult<Capacity> {
    let inputs = PlacesIn::of(world)?;
    let other = |held: &Held| matches!(held, Held::Other(_));
    if core_of(world).is_ok() && !other(&inputs.spaces) && !other(&inputs.overlay) {
        return with_world(world, |world| core::capacity(world, &inputs.places(), zone_id, nominal, occupancy))?.or_py();
    }
    let current = match occupancy {
        Some(occupancy) => occupancy,
        None => {
            let py = world.py();
            let agents = attribute(world, intern!(py, "agents"))?;
            let mut count = 0;
            for (_, agent) in exact_dict(&agents)?.iter() {
                count += i64::from(exact_str(&attribute(&agent, intern!(py, "zone_id"))?)? == zone_id);
            }
            count
        }
    };
    let (spaces, overlay) = (state_value(world, SPACES_KEY)?, state_value(world, OVERLAY_KEY)?);
    core::capacity_of_values(spaces.as_ref(), overlay.as_ref(), &inputs.schema, zone_id, nominal, current).or_py()
}

/// `zone.capacity` of a zone of the world: None for a zone without one. Read from the zone's row
/// where the core holds the world's maps and zones (lane P8 d4; policy E12: a write past the
/// object's `__setattr__` reaches no rule), and else from the Python object, which is the truth then:
/// a world without a core (a save's world being validated before its core), maps stored past
/// `IdMap`, tainted zones.
fn nominal_of(world: &Bound<'_, PyAny>, zone_id: &Bound<'_, PyAny>) -> PyResult<Option<i64>> {
    let py = world.py();
    if let Ok(zone_id) = zone_id.downcast_exact::<PyString>() {
        if rows_follow_maps(world)? {
            let zone_id = zone_id.to_str()?;
            if let Some(nominal) = with_world(world, |cell| core::nominal_of_rows(cell, zone_id))?.or_py()? {
                return Ok(nominal);
            }
        }
    }
    let zones = attribute(world, intern!(py, "zones"))?;
    let Some(zone) = raw_dict_get(&zones, zone_id)? else {
        return unsupported("no such zone");
    };
    let capacity = attribute(&zone, intern!(py, "capacity"))?;
    if capacity.is_none() {
        return Ok(None);
    }
    Ok(Some(exact_int(&capacity)?))
}

/// `capacity_metrics`' reads of a zone given as a Python value: its nominal capacity and what the
/// spaces and rooms add, or None for a zone without a capacity.
fn zone_capacity(world: &Bound<'_, PyAny>, zone_id: &Bound<'_, PyAny>, occupancy: Option<&Bound<'_, PyAny>>) -> PyResult<Option<(i64, Capacity)>> {
    let Some(nominal) = nominal_of(world, zone_id)? else {
        return Ok(None);
    };
    let zone_id = exact_str(zone_id)?;
    Ok(Some((nominal, capacity_of(world, zone_id, nominal, occupancy_of(occupancy)?)?)))
}

fn occupancy_of(occupancy: Option<&Bound<'_, PyAny>>) -> PyResult<Option<i64>> {
    match occupancy {
        Some(value) if !value.is_none() => Ok(Some(exact_int(value)?)),
        _ => Ok(None),
    }
}

#[pyfunction]
#[pyo3(name = "capacity_metrics", signature = (world, zone_id, *, occupancy=None))]
fn capacity_metrics_kernel<'py>(world: &Bound<'py, PyAny>, zone_id: &Bound<'py, PyAny>, occupancy: Option<&Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyDict>> {
    let py = world.py();
    let result = (|| {
        let out = PyDict::new(py);
        let Some((nominal, found)) = zone_capacity(world, zone_id, occupancy)? else {
            let none = py.None();
            out.set_item(intern!(py, "nominal_capacity"), &none)?;
            out.set_item(intern!(py, "effective_capacity"), &none)?;
            out.set_item(intern!(py, "temporary_people_capacity"), 0)?;
            out.set_item(intern!(py, "retained_expired_capacity"), 0)?;
            out.set_item(intern!(py, "temporary_sleeping_capacity"), 0)?;
            out.set_item(intern!(py, "physical_room_capacity"), 0)?;
            out.set_item(intern!(py, "active_room_admissions"), 0)?;
            out.set_item(intern!(py, "retained_expired_room_admissions"), 0)?;
            out.set_item(intern!(py, "effective_total_capacity"), &none)?;
            return Ok(out);
        };
        let total = found.total().or_py()?;
        out.set_item(intern!(py, "nominal_capacity"), nominal)?;
        out.set_item(intern!(py, "effective_capacity"), found.effective)?;
        out.set_item(intern!(py, "temporary_people_capacity"), found.active_people)?;
        out.set_item(intern!(py, "retained_expired_capacity"), found.retained)?;
        out.set_item(intern!(py, "temporary_sleeping_capacity"), found.sleeping)?;
        out.set_item(intern!(py, "physical_room_capacity"), found.room_capacity)?;
        out.set_item(intern!(py, "active_room_admissions"), found.active_admissions)?;
        out.set_item(intern!(py, "retained_expired_room_admissions"), found.retained_rooms)?;
        out.set_item(intern!(py, "effective_total_capacity"), total)?;
        Ok(out)
    })();
    counted(py, "capacity_metrics", result)
}

#[pyfunction]
#[pyo3(signature = (world, zone_id, *, occupancy=None))]
fn effective_zone_capacity(world: &Bound<'_, PyAny>, zone_id: &Bound<'_, PyAny>, occupancy: Option<&Bound<'_, PyAny>>) -> PyResult<Option<i64>> {
    let result = (|| Ok(zone_capacity(world, zone_id, occupancy)?.map(|(_, found)| found.effective)))();
    counted(world.py(), "effective_zone_capacity", result)
}

#[pyfunction]
#[pyo3(signature = (world, zone_id, *, occupancy=None))]
fn effective_total_zone_capacity(world: &Bound<'_, PyAny>, zone_id: &Bound<'_, PyAny>, occupancy: Option<&Bound<'_, PyAny>>) -> PyResult<Option<i64>> {
    let result = (|| zone_capacity(world, zone_id, occupancy)?.map(|(_, found)| found.total().or_py()).transpose())();
    counted(world.py(), "effective_total_zone_capacity", result)
}

/// `incoming_travelers`.
#[pyfunction]
#[pyo3(signature = (world, zone_id, *, excluding=None))]
fn incoming_travelers(world: &Bound<'_, PyAny>, zone_id: &Bound<'_, PyAny>, excluding: Option<&Bound<'_, PyAny>>) -> PyResult<i64> {
    let result = (|| {
        let zone_id = exact_str(zone_id)?;
        let excluding = match excluding {
            Some(value) if !value.is_none() => Some(exact_str(value)?),
            _ => None,
        };
        with_world(world, |world| core::incoming_travelers(world, zone_id, excluding))?.or_py()
    })();
    counted(world.py(), "incoming_travelers", result)
}

// ---- Beds and intake ----------------------------------------------------------------------------

/// `release_sleep_bed` (`part` "bed_reservations") or `release_temporary_room_admission`.
fn release_from(world: &Bound<'_, PyAny>, part: &str, agent_id: &Bound<'_, PyAny>) -> PyResult<bool> {
    let held = overlay_held(&*holding_now(world)?);
    with_world(world, |world| core::release(world, &held, part, str_value(agent_id)))?.or_py()
}

#[pyfunction]
fn release_sleep_bed(world: &Bound<'_, PyAny>, agent_id: &Bound<'_, PyAny>) -> PyResult<bool> {
    counted(world.py(), "release_sleep_bed", release_from(world, "bed_reservations", agent_id))
}

#[pyfunction]
fn release_temporary_room_admission(world: &Bound<'_, PyAny>, agent_id: &Bound<'_, PyAny>) -> PyResult<bool> {
    counted(world.py(), "release_temporary_room_admission", release_from(world, "room_admissions", agent_id))
}

/// What the intake of a room reads of the Python world.
struct IntakeIn {
    overlay: Held,
    movement: Held,
    schema: Kept<Option<i64>>,
    plan: Plan,
}

impl IntakeIn {
    fn of(world: &Bound<'_, PyAny>) -> PyResult<Self> {
        let mut host = PyHost::of_entry(world);
        let holding = host.holding_now()?;
        let overlay = overlay_held(&holding);
        let movement = movement_held(&holding);
        let schema = schema_of(&mut host, &holding)?;
        let plan = Plan::with(world, &overlay);
        Ok(Self { overlay, movement, schema, plan })
    }

    fn intake(&self) -> Intake<'_> {
        Intake { overlay: &self.overlay, movement: &self.movement, schema: &self.schema, geo: self.plan.get() }
    }
}

#[pyfunction]
fn room_admission_point<'py>(world: &Bound<'py, PyAny>, room_id: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = world.py();
    let result = (|| {
        let room_id = exact_str(room_id)?;
        let inputs = IntakeIn::of(world)?;
        let found = inputs.plan.finish(py, with_world(world, |world| core::room_admission_point(world, &inputs.intake(), room_id))?.or_py())?;
        match found {
            Some(point) => point_object(py, point),
            None => Ok(py.None().into_bound(py)),
        }
    })();
    counted(py, "room_admission_point", result)
}

/// `available_temporary_room`, after the admissions a plan has given this far, with the point
/// `admit_to_temporary_room` then gives.
pub(crate) fn available_room_after(world: &Bound<'_, PyAny>, zone_id: &str, planned: &[(Arc<str>, Option<Pt>)]) -> PyResult<Option<(Arc<str>, Pt)>> {
    let inputs = IntakeIn::of(world)?;
    inputs.plan.finish(world.py(), with_world(world, |world| core::available_room_after(world, &inputs.intake(), zone_id, planned))?.or_py())
}

/// `available_temporary_room`.
#[pyfunction]
fn available_temporary_room(world: &Bound<'_, PyAny>, zone_id: &Bound<'_, PyAny>) -> PyResult<Option<String>> {
    let result = (|| Ok(available_room_after(world, exact_str(zone_id)?, &[])?.map(|(room_id, _)| room_id.to_string())))();
    counted(world.py(), "available_temporary_room", result)
}

/// `admit_to_temporary_room`.
#[pyfunction]
#[pyo3(signature = (world, *, agent_id, room_id, minute, event_id))]
fn admit_to_temporary_room<'py>(
    world: &Bound<'py, PyAny>,
    agent_id: &Bound<'py, PyAny>,
    room_id: &Bound<'py, PyAny>,
    minute: &Bound<'py, PyAny>,
    event_id: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = world.py();
    let result = (|| {
        let (agent_id, room_id) = (exact_str(agent_id)?, exact_str(room_id)?);
        let minute = exact_int(minute)?;
        let event_id = exact_str(event_id)?;
        let inputs = IntakeIn::of(world)?;
        let point = inputs.plan.finish(py, with_world(world, |world| core::admit(world, &inputs.intake(), agent_id, room_id, minute, event_id))?.or_py())?;
        point_object(py, point)
    })();
    counted(py, "admit_to_temporary_room", result)
}

/// What `reserve_sleep_bed` reads of the Python world.
struct BedsIn {
    overlay: Held,
    movement: Held,
    schema: Kept<Option<i64>>,
    walking: Kept<Movement>,
    plan: Plan,
}

impl BedsIn {
    fn of(world: &Bound<'_, PyAny>, host: &mut PyHost<'_>, holding: &Holding, overlay: Held) -> PyResult<Self> {
        let movement = movement_held(holding);
        let schema = schema_of(host, holding)?;
        let walking = host.value_section(holding, SectionId::Movement, |configs| &configs.movement, Movement::parse)?;
        let plan = Plan::with(world, &overlay);
        Ok(Self { overlay, movement, schema, walking, plan })
    }

    fn beds(&self) -> Beds<'_> {
        Beds { overlay: &self.overlay, movement: &self.movement, schema: &self.schema, walking: &self.walking, geo: self.plan.get() }
    }
}

/// `reserve_sleep_bed`, writing only the empty reservations of an older state: None for no bed.
pub(crate) fn plan_bed(world: &Bound<'_, PyAny>, agent_id: &str) -> PyResult<Option<Bed>> {
    let mut host = PyHost::of_entry(world);
    let holding = host.holding_now()?;
    let overlay = overlay_held(&holding);
    if matches!(overlay, Held::Absent) {
        return Ok(None);
    }
    let inputs = BedsIn::of(world, &mut host, &holding, overlay)?;
    inputs.plan.finish(world.py(), with_world(world, |world| core::plan_bed(world, &inputs.beds(), agent_id))?.or_py())
}

/// The reservation of `plan_bed`, when it is new.
pub(crate) fn write_bed(world: &Bound<'_, PyAny>, agent_id: &str, bed: &Bed) -> PyResult<()> {
    if !bed.is_new {
        return Ok(());
    }
    let held = overlay_held(&*holding_now(world)?);
    with_world(world, |world| core::write_bed(world, &held, agent_id, bed))?.or_py()
}

#[pyfunction]
fn reserve_sleep_bed<'py>(world: &Bound<'py, PyAny>, agent_id: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = world.py();
    let result = (|| {
        let agent_text = exact_str(agent_id)?;
        let Some(bed) = plan_bed(world, agent_text)? else {
            return Ok(py.None().into_bound(py));
        };
        let route = bed.path.iter().map(|point| point_object(py, *point)).collect::<PyResult<Vec<_>>>()?;
        let answer = PyDict::new(py);
        answer.set_item(intern!(py, "agent_id"), agent_id)?;
        answer.set_item(intern!(py, "room_id"), &*bed.room_id)?;
        answer.set_item(intern!(py, "bed_index"), bed.bed_index)?;
        answer.set_item(intern!(py, "target_id"), &bed.target_id)?;
        answer.set_item(intern!(py, "reserved_minute"), bed.reserved_minute)?;
        answer.set_item(intern!(py, "point"), point_object(py, bed.point)?)?;
        answer.set_item(intern!(py, "route_points"), PyTuple::new(py, route)?)?;
        answer.set_item(intern!(py, "travel_minutes"), bed.travel_minutes)?;
        write_bed(world, agent_text, &bed)?;
        Ok(answer.into_any())
    })();
    counted(py, "reserve_sleep_bed", result)
}

// ---- Facilities installed during a Run ----------------------------------------------------------

/// `installed_facility_placement_enabled`.
#[pyfunction]
fn installed_facility_placement_enabled(world: &Bound<'_, PyAny>) -> PyResult<bool> {
    let result = (|| {
        let mut host = PyHost::of_entry(world);
        let holding = host.holding_now()?;
        let (schema, overlay) = (schema_of(&mut host, &holding)?, overlay_held(&holding));
        core::placement_enabled(&schema, &overlay).or_py()
    })();
    counted(world.py(), "installed_facility_placement_enabled", result)
}

#[pyfunction]
#[pyo3(signature = (world, facility_id))]
fn installed_facility_point<'py>(world: &Bound<'py, PyAny>, facility_id: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = world.py();
    let result = (|| {
        let facility_id = exact_str(facility_id)?;
        let held = overlay_held(&*holding_now(world)?);
        let plan = Plan::of(world);
        match plan.finish(py, with_world(world, |world| core::installed_point(world, &held, plan.get(), facility_id))?.or_py())? {
            Some(point) => point_object(py, point),
            None => Ok(py.None().into_bound(py)),
        }
    })();
    counted(py, "installed_facility_point", result)
}

#[pyfunction]
#[pyo3(signature = (world, facility_id, *, event_id, source_ids))]
fn place_installed_facility<'py>(
    world: &Bound<'py, PyAny>,
    facility_id: &Bound<'py, PyAny>,
    event_id: &Bound<'py, PyAny>,
    source_ids: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = world.py();
    let result = (|| {
        let tuple = source_ids.downcast_exact::<PyTuple>().map_err(|_| Unsupported::new_err("the sources are not a tuple"))?;
        let held: Vec<Bound<'_, PyAny>> = tuple.iter().collect();
        let names = held.iter().map(|item| exact_str(item)).collect::<PyResult<Vec<&str>>>()?;
        let (facility_id, event_id) = (exact_str(facility_id)?, exact_str(event_id)?);
        let overlay = overlay_held(&*holding_now(world)?);
        let plan = Plan::of(world);
        let point = plan.finish(py, with_world(world, |world| core::place_facility(world, &overlay, plan.get(), facility_id, event_id, &names))?.or_py())?;
        point_object(py, point)
    })();
    counted(py, "place_installed_facility", result)
}

// ---- Approved changes of the floor plan -----------------------------------------------------------

/// What a geometry change reads of the Python world.
pub(crate) struct ChangeIn {
    overlay: Held,
    movement: Held,
    staff_operations: Held,
    resource_service: Held,
    service: Kept<ResourceService>,
    plan: Plan,
}

impl ChangeIn {
    pub(crate) fn of(world: &Bound<'_, PyAny>, host: &mut PyHost<'_>) -> PyResult<Self> {
        let holding = host.holding_now()?;
        let overlay = overlay_held(&holding);
        let plan = Plan::with(world, &overlay);
        Ok(Self {
            movement: movement_held(&holding),
            staff_operations: holding.held("staff_operations_v1", "the world holds another staff operations state"),
            resource_service: holding.held("resource_service_v1", "the world holds another resource service state"),
            service: host.value_section(&holding, SectionId::ResourceService, |configs| &configs.resource_service, ResourceService::parse)?,
            overlay,
            plan,
        })
    }

    pub(crate) fn change(&self) -> Change<'_> {
        Change {
            overlay: &self.overlay,
            movement: &self.movement,
            staff_operations: &self.staff_operations,
            resource_service: &self.resource_service,
            service: &self.service,
            geo: self.plan.get(),
        }
    }
}

fn end_of(end_minute: &Bound<'_, PyAny>) -> PyResult<Option<i64>> {
    if end_minute.is_none() {
        Ok(None)
    } else {
        Ok(Some(exact_int(end_minute)?))
    }
}

/// `validate_geometry_change`, for the approval of an EffectProgram and the Branch commands.
#[pyfunction]
fn validate_geometry_change(
    world: &Bound<'_, PyAny>,
    op: &Bound<'_, PyAny>,
    target: &Bound<'_, PyAny>,
    parameters: &Bound<'_, PyAny>,
    end_minute: &Bound<'_, PyAny>,
) -> PyResult<()> {
    let py = world.py();
    let result = (|| {
        let (op, target, end_minute) = (exact_str(op)?, exact_str(target)?, end_of(end_minute)?);
        let parameters = <V as VPy>::from_py(parameters)?;
        let mut host = PyHost::of_entry(world);
        let inputs = ChangeIn::of(world, &mut host)?;
        let checked = core::check_change(&PyWorldCell(world), &mut host, &inputs.change(), op, target, &parameters, end_minute);
        inputs.plan.finish(py, checked.map(drop).map_err(|error| error.0))
    })();
    counted(py, "validate_geometry_change", result)
}

#[pyfunction]
#[pyo3(signature = (world, event, *, expire=false))]
fn apply_geometry_change(world: &Bound<'_, PyAny>, event: &Bound<'_, PyAny>, expire: bool) -> PyResult<()> {
    let py = world.py();
    let result = (|| {
        let Some(row) = EventRow::from_event(event)? else {
            return unsupported("not a ScheduledEvent");
        };
        let mut host = PyHost::of_entry(world);
        let inputs = ChangeIn::of(world, &mut host)?;
        let applied = core::apply_change(&PyWorldCell(world), &mut host, &inputs.change(), &row, expire);
        inputs.plan.finish(py, applied.map_err(|error| error.0))
    })();
    counted(py, "apply_geometry_change", result)
}

/// `validate_geometry_overlay_state`: true only for what the Python rule passes.
#[pyfunction]
fn validate_overlay(world: &Bound<'_, PyAny>) -> PyResult<bool> {
    let py = world.py();
    let result = (|| {
        let held = overlay_held(&*holding_now(world)?);
        let plan = Plan::with(world, &held);
        plan.finish(py, with_world(world, |world| core::check_overlay(world, &held, plan.get()))?.or_py())
    })();
    counted(py, "validate_overlay", result.map(|()| true))
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(register_temporary_space, m)?)?;
    m.add_function(wrap_pyfunction!(expire_temporary_space, m)?)?;
    m.add_function(wrap_pyfunction!(validate_spaces, m)?)?;
    m.add_function(wrap_pyfunction!(capacity_metrics_kernel, m)?)?;
    m.add_function(wrap_pyfunction!(effective_zone_capacity, m)?)?;
    m.add_function(wrap_pyfunction!(effective_total_zone_capacity, m)?)?;
    m.add_function(wrap_pyfunction!(incoming_travelers, m)?)?;
    m.add_function(wrap_pyfunction!(release_sleep_bed, m)?)?;
    m.add_function(wrap_pyfunction!(release_temporary_room_admission, m)?)?;
    m.add_function(wrap_pyfunction!(room_admission_point, m)?)?;
    m.add_function(wrap_pyfunction!(available_temporary_room, m)?)?;
    m.add_function(wrap_pyfunction!(admit_to_temporary_room, m)?)?;
    m.add_function(wrap_pyfunction!(reserve_sleep_bed, m)?)?;
    m.add_function(wrap_pyfunction!(installed_facility_placement_enabled, m)?)?;
    m.add_function(wrap_pyfunction!(installed_facility_point, m)?)?;
    m.add_function(wrap_pyfunction!(place_installed_facility, m)?)?;
    m.add_function(wrap_pyfunction!(validate_geometry_change, m)?)?;
    m.add_function(wrap_pyfunction!(apply_geometry_change, m)?)?;
    m.add_function(wrap_pyfunction!(validate_overlay, m)?)?;
    m.add_function(wrap_pyfunction!(stats, m)?)?;
    Ok(())
}
