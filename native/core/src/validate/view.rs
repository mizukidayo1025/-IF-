//! How a validator reads the world (`View`), and the Python texts of what it reads (`repr`).
//!
//! A validator holds the world shared for the whole check: it never writes, and it calls no host.
//! What Python's rule reads, a validator reads here:
//! - the scalars (`schema_version`, `minute_index`, `duration_minutes`) and the entity rows
//!   (`rows`), as the tick's rules read them;
//! - `model_configs` as the JSON value it is (`configs`, `config`): a validator parses the part it
//!   needs itself, with the Python rule's texts, and does not use the sections a host filled;
//! - a key of `model_state` (`contains`, `state`): the value as a `V`, whatever row holds it. A
//!   typed row is written as JSON and read back, which is slow but always what Python reads; a
//!   validator may read a typed row directly (`row`) where that is the row, as a faster path, and
//!   falls back to `state` where it is not (a test loads a broken namespace as a `ValueRow`,
//!   `load::Namespaces::Lenient`);
//! - the event queue and the log (`queue`, `log`).
//!
//! What is checked depends on the call (`View::history`, `View::in_flight`), as Python's keyword
//! arguments say.

use std::ops::Deref;

use crate::cell::ShardRef;
use crate::entities::Entities;
use crate::error::{Error, Result};
use crate::events::log::EventLog;
use crate::events::queue::Queue;
use crate::events::EventRow;
use crate::json::{python_float_repr, Encoder};
use crate::row::{Row, ValueRow};
use crate::text::py_repr;
use crate::value::V;
use crate::world::{Place, World, MODEL_CONFIGS};

/// The world as a validator reads it, and what the call asks.
pub struct View<'w> {
    pub world: &'w World,
    /// `validate_event_history`: the whole append-only histories are read (a save, a restore,
    /// `world.validate()`), not only the records of the last tick.
    pub history: bool,
    /// `_in_flight_event`: a pending event a handler is applying now.
    pub in_flight: Option<&'w EventRow>,
}

/// A value of the world: borrowed where a `ValueRow` holds it, made where a typed row does.
pub enum StateRef<'w> {
    Shared(ShardRef<'w, V>),
    Owned(V),
}

impl Deref for StateRef<'_> {
    type Target = V;

    fn deref(&self) -> &V {
        match self {
            StateRef::Shared(value) => value,
            StateRef::Owned(value) => value,
        }
    }
}

/// A row as the `V` its canonical JSON reads back as.
fn value_of_row(row: &dyn Row) -> Result<V> {
    let mut encoder = Encoder::default();
    row.write_json(&mut encoder)?;
    let json: serde_json::Value = serde_json::from_slice(&encoder.out).map_err(|_| Error::unsupported("a row's JSON does not read back"))?;
    V::from_json(&json).ok_or_else(|| Error::unsupported("a row's JSON does not read back"))
}

impl<'w> View<'w> {
    /// A view for a call with these arguments.
    pub fn new(world: &'w World, history: bool, in_flight: Option<&'w EventRow>) -> Self {
        Self { world, history, in_flight }
    }

    pub fn schema_version(&self) -> Result<i64> {
        self.world.schema_version()
    }

    pub fn minute_index(&self) -> Result<i64> {
        self.world.minute_index()
    }

    pub fn duration_minutes(&self) -> Result<i64> {
        self.world.duration_minutes()
    }

    /// The entity rows (`world.agents`, ...).
    pub fn rows(&self) -> Result<ShardRef<'w, Entities>> {
        self.world.rows()
    }

    /// `world.model_configs`.
    pub fn configs(&self) -> Result<ShardRef<'w, V>> {
        ShardRef::try_map(self.world.slots.try_row::<ValueRow>(MODEL_CONFIGS)?, |row| Ok(&row.0))
    }

    /// `name in world.model_configs`; false for configs that are not a dict.
    pub fn has_config(&self, name: &str) -> Result<bool> {
        Ok(matches!(&*self.configs()?, V::Map(configs) if configs.contains_key(name)))
    }

    /// `world.model_configs.get(name)`, copied; None for configs that are not a dict.
    pub fn config(&self, name: &str) -> Result<Option<V>> {
        Ok(match &*self.configs()? {
            V::Map(configs) => configs.get(name).cloned(),
            _ => None,
        })
    }

    /// `name in world.model_state`. A name the world holds as a field (`resources`, ...) is not a
    /// key of it.
    pub fn contains(&self, name: &str) -> Result<bool> {
        let Some(slot) = self.world.slots.get(name) else {
            return Ok(false);
        };
        let slot = slot.try_borrow()?;
        Ok(slot.place != Place::Field && (slot.row.is_some() || slot.taint.is_some()))
    }

    /// `world.model_state.get(name)` (a key that holds None gives `V::Null`). Unsupported for a
    /// tainted key, whose value only Python holds.
    pub fn state(&self, name: &str) -> Result<Option<StateRef<'w>>> {
        let Some(slot) = self.world.slots.get(name) else {
            return Ok(None);
        };
        let held = slot.try_borrow()?;
        if held.place == Place::Field {
            return Ok(None);
        }
        let Some(row) = held.row.as_deref() else {
            return match &held.taint {
                None => Ok(None),
                Some(_) => Err(Error::unsupported("the namespace is tainted")),
            };
        };
        if !row.as_any().is::<ValueRow>() {
            return value_of_row(row).map(|value| Some(StateRef::Owned(value)));
        }
        ShardRef::try_map(held, |slot| slot.row::<ValueRow>().map(|row| &row.0)).map(|value| Some(StateRef::Shared(value)))
    }

    /// The typed row of a key of `model_state`, where the slot holds a row of this type; None
    /// otherwise (absent, tainted, or another row: read `state` then).
    pub fn row<T: Row>(&self, name: &str) -> Result<Option<ShardRef<'w, T>>> {
        let Some(slot) = self.world.slots.get(name) else {
            return Ok(None);
        };
        let held = slot.try_borrow()?;
        if held.place == Place::Field || !held.row.as_deref().is_some_and(|row| row.as_any().is::<T>()) {
            return Ok(None);
        }
        ShardRef::try_map(held, |slot| slot.row::<T>()).map(Some)
    }

    /// The pending events (`world.event_queue`).
    pub fn queue(&self) -> Result<ShardRef<'w, Queue>> {
        self.world.queue()
    }

    /// The realized records (`world.event_log`).
    pub fn log(&self) -> Result<ShardRef<'w, EventLog>> {
        self.world.log()
    }
}

/// `repr(value)` of the Python value a `V` reads as: `None`, `True`, `1`, `1.0`, `'text'`,
/// `[1, 'a']`, `{'key': 1}`.
pub fn repr(value: &V) -> String {
    let mut out = String::new();
    write_repr(&mut out, value);
    out
}

fn write_repr(out: &mut String, value: &V) {
    match value {
        V::Null => out.push_str("None"),
        V::Bool(true) => out.push_str("True"),
        V::Bool(false) => out.push_str("False"),
        V::Int(number) => out.push_str(&number.to_string()),
        V::Big(text) => out.push_str(text),
        V::Float(number) if number.is_nan() => out.push_str("nan"),
        V::Float(number) if number.is_infinite() => out.push_str(if *number > 0.0 { "inf" } else { "-inf" }),
        V::Float(number) => out.push_str(&python_float_repr(*number)),
        V::Str(text) => out.push_str(&py_repr(text)),
        V::List(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                write_repr(out, item);
            }
            out.push(']');
        }
        V::Map(map) => {
            out.push('{');
            for (index, (key, item)) in map.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                out.push_str(&py_repr(key));
                out.push_str(": ");
                write_repr(out, item);
            }
            out.push('}');
        }
    }
}

/// `repr` of a list of str, as `sorted(...)` of a set gives it to an f-string: `['a', 'b']`.
pub fn repr_texts<'a>(texts: impl IntoIterator<Item = &'a str>) -> String {
    let items: Vec<String> = texts.into_iter().map(py_repr).collect();
    format!("[{}]", items.join(", "))
}
