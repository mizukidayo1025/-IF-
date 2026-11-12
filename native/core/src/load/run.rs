//! What the Run read of the controller, from the document the PyO3 crate wrote of it
//! (`RunCore.policies_document`) and the controller's route capacities and zone neighbors.

use std::collections::HashMap;
use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::Value;

use super::{bad, bool_of, object, str_of, text, value};
use crate::config::{Fault, FaultClass};
use crate::error::Result;
use crate::rules::tick::ZoneNeighbors;
use crate::run::dispatch::{Dispatch, Handler, HandlerKind, Native};
use crate::run::held::{Class, Held, Obj};
use crate::run::{Capacity, Policies, RouteCapacities};

/// A fault as the exporter writes it: `[class, text]`.
pub fn fault(json: &Value) -> Result<Fault> {
    let class = match str_of(&json[0], "a fault's class")? {
        "Value" => FaultClass::Value,
        "Type" => FaultClass::Type,
        "Key" => FaultClass::Key,
        "Validation" => FaultClass::Validation,
        "Refused" => FaultClass::Refused,
        other => FaultClass::Other(text(other)),
    };
    Ok(Fault::new(class, str_of(&json[1], "a fault's text")?))
}

fn held(json: &Value) -> Result<Held> {
    let entries = object(json, "a held value")?;
    let (kind, item) = match entries.iter().next() {
        Some(entry) if entries.len() == 1 => entry,
        _ => return Err(bad("a held value has not one entry")),
    };
    Ok(match kind.as_str() {
        "value" => Held::Value(value(item)?),
        "text" => Held::Text(text(str_of(item, "a held text")?)),
        "object" => Held::Object(Arc::new(obj(item)?)),
        "opaque" => Held::Opaque { callable: bool_of(item, "an opaque value's callable")? },
        "method" => Held::Method(text(str_of(item, "a held method")?)),
        _ => return Err(bad("not a held value")),
    })
}

fn read(json: &Value) -> Result<std::result::Result<Held, Fault>> {
    Ok(match json.get("fault") {
        Some(found) => Err(fault(found)?),
        None => Ok(held(json)?),
    })
}

/// An object as `RunCore.policies_document` wrote it.
pub fn obj(json: &Value) -> Result<Obj> {
    let class = &json["class"];
    let mut object = Obj::new(Class {
        module: class[0].as_str().map(text),
        qualname: class[1].as_str().map(text),
        resolves: bool_of(&class[2], "a class's resolves")?,
    });
    for (name, item) in super::object(&json["attrs"], "attrs")? {
        object.attrs.insert(text(name), read(item)?);
    }
    for (name, item) in super::object(&json["class_own"], "class_own")? {
        object.class_own.insert(text(name), held(item)?);
    }
    for (name, item) in super::object(&json["own"], "own")? {
        object.own.insert(text(name), held(item)?);
    }
    Ok(object)
}

fn handler(json: &Value) -> Result<Handler> {
    if json.as_str() == Some("foreign") {
        return Ok(Handler::Foreign);
    }
    let class = &json["class"];
    let (module, qualname) = (str_of(&class[0], "a handler's module")?, str_of(&class[1], "a handler's qualname")?);
    let kind = HandlerKind::of_class(module, qualname).ok_or_else(|| bad(format_args!("the core runs no handler {module}.{qualname}")))?;
    Ok(Handler::Native(Native {
        kind,
        handler: Arc::new(obj(&json["handler"])?),
        own_managers: bool_of(&json["own_managers"], "own_managers")?,
        after_arrival_as_defined: bool_of(&json["after_arrival_as_defined"], "after_arrival_as_defined")?,
    }))
}

fn dispatch(json: &Value) -> Result<Dispatch> {
    let known = match json["known"].get("fault") {
        Some(found) => Err(fault(found)?),
        None => Ok(bool_of(&json["known"]["value"], "known")?),
    };
    let handlers = match json["handlers"].get("fault") {
        Some(found) => Err(fault(found)?),
        None => {
            let mut table = IndexMap::new();
            for (event_type, item) in object(&json["handlers"]["value"], "handlers")? {
                table.insert(text(event_type), handler(item)?);
            }
            Ok(Arc::new(table))
        }
    };
    Ok(Dispatch { known, handlers })
}

/// The Run's policies as the PyO3 crate read them (`RunCore.policies_document`).
pub fn policies(json: &Value) -> Result<Policies> {
    let mut policies = Policies::of(Arc::new(obj(&json["controller"])?));
    policies.dispatch = dispatch(&json["dispatch"])?;
    Ok(policies)
}

/// `controller.route_capacities` as the Run reads it: ints and None, by edge.
pub fn route_capacities(json: &Value) -> Result<RouteCapacities> {
    let mut by_edge = HashMap::new();
    for (edge, capacity) in object(json, "route_capacities")? {
        let capacity = match capacity {
            Value::Null => Capacity::Unlimited,
            number => Capacity::Max(number.as_i64().ok_or_else(|| bad("a route capacity is not an int"))?),
        };
        by_edge.insert(text(edge), capacity);
    }
    Ok(RouteCapacities { any: !by_edge.is_empty(), by_edge, unmatched_keys: false })
}

/// `controller.zone_neighbors` as the tick reads it.
pub fn neighbors(json: &Value) -> Result<Arc<ZoneNeighbors>> {
    let mut table: HashMap<String, Vec<String>> = HashMap::new();
    for (zone, near) in object(json, "zone_neighbors")? {
        let near = super::array(near, "a zone's neighbors")?
            .iter()
            .map(|item| str_of(item, "a neighbor").map(str::to_owned))
            .collect::<Result<_>>()?;
        table.insert(zone.clone(), near);
    }
    let social = Ok(table.iter().map(|(zone, near)| (zone.clone(), Ok(near.clone()))).collect());
    Ok(Arc::new(ZoneNeighbors { social, information: Ok(table) }))
}
