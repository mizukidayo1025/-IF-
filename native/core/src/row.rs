//! Typed rows without Python: `Field`, what a value of a row can do, and `state_row!`, which writes
//! it for a record from its list of fields (docs/rust-port/lanes/p5-template-namespace.md).
//!
//! ```ignore
//! sheltersim_core::state_row! {
//!     #[derive(Clone, Copy)]
//!     pub struct AgentRecord {
//!         level: f64 => set_level,   // a scalar field with a mirrored setter
//!         visits: i64,               // a scalar field Rust does not write
//!     }
//! }
//! ```
//!
//! Any `Field` is a `Row`, so a namespace is a `state_row!` struct or a map of them. The macro
//! writes, for the struct:
//! - `NAMES`: the fields in the order they are listed, which is the tree's order.
//! - `write_json`: the bytes of `canonical_json` of the tree, keys sorted by bytes.
//! - `from_value`: the record read from the `V` of that JSON, with the checks of the PyO3 crate's
//!   reader (`Field::from_value`). `str_enum!` writes it for its set too, so no row lacks one.
//! - `set_level(&mut self, tree, value)`: the mirrored setter a rule uses: the row and the tree, or
//!   neither when the value has the same bits. Only a `Scalar` field can have one, because storing
//!   a scalar over a scalar leaves the tree's table of nodes as it is. The tree is a `TreeSink`,
//!   which the PyO3 crate implements for the Python tree.
//!
//! What needs Python is in the PyO3 crate (`native/src/core/row.rs`): reading a row from a Python
//! value, making one, and applying a forwarded write. Its `py_row!` writes that for a struct of
//! this macro, from the same field names.
//!
//! Also here, for what most namespaces need beside the fields: `Maybe<T>` for a key the tree may
//! not have (not the same as `Option<T>`, a key holding None), and `str_enum!` for a closed set of
//! strings.
//!
//! `canonicalize` puts a row in the order `WorldState.from_dict(world.to_dict())` gives its tree
//! (lane 6-2, design-p5-p6.md §1.3 F1): the keys of every map sorted by their bytes, as
//! `json.dumps(sort_keys=True)` sorts str by code point; lists and the fields of a record keep
//! their order. A transaction does it at its start (`World::canonicalize`), so the kernels that
//! iterate a map go in the order they went after a copy through canonical JSON.

use std::any::Any;
use std::sync::Arc;

use indexmap::IndexMap;
use rustc_hash::FxBuildHasher;

use crate::error::{Error, Misfit, Result};
use crate::json::{write_json_string, Encoder};
use crate::value::V;

/// A map keyed by id: insertion order like a dict's, so what iterates it is deterministic.
pub type IdMap<T> = IndexMap<Arc<str>, T, FxBuildHasher>;

/// One step of a path from the namespace's root.
#[derive(Clone, Copy, Debug)]
pub enum Part<'a> {
    Key(&'a str),
    Index(usize),
}

/// A value that can be part of a row. `Clone` is for a snapshot's copy on write (lane 6-2).
pub trait Field: Sized + Clone + Send + Sync + 'static {
    /// The bytes of `canonical_json`.
    fn write_json(&self, encoder: &mut Encoder) -> Result<()>;
    /// The value read from JSON, as a `V`: what `write_json` wrote, or a namespace of a world's
    /// canonical document, read with a float where the JSON has a point or an exponent (`1.0`) and
    /// an int where it has neither (`1`), as Python's `json` reads them. It takes what the PyO3
    /// crate's `PyField::from_py` takes of the same value in Python, and refuses with a `Misfit`
    /// what that refuses: an int for a float and the other way round, a missing key of a record
    /// unless `when_missing` allows it, a key the record does not have, a text outside a
    /// `str_enum!` set, and the ranges and the checks a hand-written reader there has. Nothing
    /// in a tick reads a row this way; it is for a world read from a file without Python.
    fn from_value(value: &V) -> std::result::Result<Self, Misfit>;
    /// Sort the keys of every map inside by their bytes, as a copy through canonical JSON does;
    /// lists keep their order. A value with no map inside does nothing.
    fn canonicalize(&mut self);
    /// The value of a field whose key the record does not have, if that is allowed: `Maybe` only.
    fn when_missing() -> Option<Self> {
        None
    }
    /// Whether this value is a key the record does not have.
    fn is_absent(&self) -> bool {
        false
    }
    /// The value as a list read item by item, if it is a list (`Row::part_items`).
    fn items(&self) -> Option<&dyn Items> {
        None
    }
    /// The items of the list under key of a record: None for a key it does not have, a value
    /// that is not a list, or a value that is not a record.
    fn part_items(&self, _key: &str) -> Option<&dyn Items> {
        None
    }
}

/// A list of a row, read an item at a time: for a reader of a part of a detached namespace, which
/// must not make all of a history of 20,000 items.
pub trait Items {
    fn count(&self) -> usize;
    /// The canonical JSON of an item: the bytes the row keeps, or `scratch` after writing them there.
    fn item_json<'a>(&'a self, index: usize, scratch: &'a mut Encoder) -> Result<&'a [u8]>;
}

/// A namespace's state as Rust holds it. A lane implements this in `state/<name>.rs`.
pub trait Row: Send + Sync + 'static {
    /// The canonical JSON of the state, the same bytes as `canonical_json` of the tree.
    fn write_json(&self, encoder: &mut Encoder) -> Result<()>;
    /// The items of the list under key, read one by one: None for a key the state does not have,
    /// or a value that is not a list.
    fn part_items(&self, key: &str) -> Option<&dyn Items>;
    /// The row as its own type, for a reader that knows it (`Slot::row`).
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
    /// A copy, for a snapshot's copy on write (`Slot`'s `Clone`).
    fn clone_row(&self) -> Box<dyn Row>;
    /// `Field::canonicalize`: the order a copy through canonical JSON gives.
    fn canonicalize(&mut self);
}

impl<T: Field> Row for T {
    fn write_json(&self, encoder: &mut Encoder) -> Result<()> {
        Field::write_json(self, encoder)
    }

    fn clone_row(&self) -> Box<dyn Row> {
        Box::new(self.clone())
    }

    fn canonicalize(&mut self) {
        Field::canonicalize(self);
    }

    fn part_items(&self, key: &str) -> Option<&dyn Items> {
        Field::part_items(self, key)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// A row that is a `V`, for a namespace without typed fields: any JSON fits.
#[derive(Clone)]
pub struct ValueRow(pub V);

impl Row for ValueRow {
    fn write_json(&self, encoder: &mut Encoder) -> Result<()> {
        self.0.write_json(encoder)
    }

    fn clone_row(&self) -> Box<dyn Row> {
        Box::new(self.clone())
    }

    fn canonicalize(&mut self) {
        self.0.canonicalize();
    }

    fn part_items(&self, key: &str) -> Option<&dyn Items> {
        match &self.0 {
            V::Map(map) => match map.get(key) {
                Some(V::List(items)) => Some(items as &dyn Items),
                _ => None,
            },
            _ => None,
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// A scalar as the tree stores it. `Name` is a text of a closed set, which the tree keeps as an
/// interned str; `Str` is any other text.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Atom<'a> {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(&'a str),
    Name(&'static str),
}

/// A field a mirrored setter can store in the tree as it is: never a container.
pub trait Scalar: Field {
    /// Whether writing other would change nothing in the canonical JSON.
    fn same(&self, other: &Self) -> bool;
    /// The value the tree stores.
    fn atom(&self) -> Atom<'_>;
}

/// The dict of a namespace's tree that a mirrored setter writes, if the namespace has a tree: the
/// PyO3 crate's `Tree`. A detached namespace has none, and its sink does nothing.
pub trait TreeSink {
    type Error;
    /// d[key] = value, where key is a field of a record.
    fn set_field(&self, key: &'static str, value: Atom<'_>) -> std::result::Result<(), Self::Error>;
    /// d[key] = value, where key is an entry of a map.
    fn set_entry(&self, key: &str, value: Atom<'_>) -> std::result::Result<(), Self::Error>;
}

impl<S: TreeSink + ?Sized> TreeSink for &S {
    type Error = S::Error;

    fn set_field(&self, key: &'static str, value: Atom<'_>) -> std::result::Result<(), Self::Error> {
        (**self).set_field(key, value)
    }

    fn set_entry(&self, key: &str, value: Atom<'_>) -> std::result::Result<(), Self::Error> {
        (**self).set_entry(key, value)
    }
}

impl Field for f64 {
    fn write_json(&self, encoder: &mut Encoder) -> Result<()> {
        encoder.try_float(*self)
    }

    fn from_value(value: &V) -> std::result::Result<Self, Misfit> {
        match value {
            V::Float(number) => Ok(*number),
            _ => Err(Misfit::new("not a float")),
        }
    }

    fn canonicalize(&mut self) {}
}

impl Scalar for f64 {
    fn same(&self, other: &Self) -> bool {
        self.to_bits() == other.to_bits()
    }

    fn atom(&self) -> Atom<'_> {
        Atom::Float(*self)
    }
}

impl Field for i64 {
    fn write_json(&self, encoder: &mut Encoder) -> Result<()> {
        encoder.int(*self);
        Ok(())
    }

    fn from_value(value: &V) -> std::result::Result<Self, Misfit> {
        match value {
            V::Int(number) => Ok(*number),
            V::Big(_) => Err(Misfit::new(BIG_INT)),
            _ => Err(Misfit::new("not an int")),
        }
    }

    fn canonicalize(&mut self) {}
}

impl Scalar for i64 {
    fn same(&self, other: &Self) -> bool {
        self == other
    }

    fn atom(&self) -> Atom<'_> {
        Atom::Int(*self)
    }
}

impl Field for bool {
    fn write_json(&self, encoder: &mut Encoder) -> Result<()> {
        encoder.bool(*self);
        Ok(())
    }

    fn from_value(value: &V) -> std::result::Result<Self, Misfit> {
        match value {
            V::Bool(flag) => Ok(*flag),
            _ => Err(Misfit::new("not a bool")),
        }
    }

    fn canonicalize(&mut self) {}
}

impl Scalar for bool {
    fn same(&self, other: &Self) -> bool {
        self == other
    }

    fn atom(&self) -> Atom<'_> {
        Atom::Bool(*self)
    }
}

impl Field for Arc<str> {
    fn write_json(&self, encoder: &mut Encoder) -> Result<()> {
        write_json_string(&mut encoder.out, self);
        Ok(())
    }

    fn from_value(value: &V) -> std::result::Result<Self, Misfit> {
        match value {
            V::Str(text) => Ok(text.clone()),
            _ => Err(Misfit::new("not a str")),
        }
    }

    fn canonicalize(&mut self) {}
}

impl Scalar for Arc<str> {
    fn same(&self, other: &Self) -> bool {
        self == other
    }

    fn atom(&self) -> Atom<'_> {
        Atom::Str(self)
    }
}

impl<T: Field> Field for Option<T> {
    fn write_json(&self, encoder: &mut Encoder) -> Result<()> {
        match self {
            Some(value) => value.write_json(encoder),
            None => {
                encoder.null();
                Ok(())
            }
        }
    }

    fn from_value(value: &V) -> std::result::Result<Self, Misfit> {
        match value {
            V::Null => Ok(None),
            _ => Ok(Some(T::from_value(value)?)),
        }
    }

    fn items(&self) -> Option<&dyn Items> {
        self.as_ref().and_then(Field::items)
    }

    fn canonicalize(&mut self) {
        if let Some(value) = self {
            value.canonicalize();
        }
    }
}

impl<T: Scalar> Scalar for Option<T> {
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Some(first), Some(second)) => first.same(second),
            (None, None) => true,
            _ => false,
        }
    }

    fn atom(&self) -> Atom<'_> {
        match self {
            Some(value) => value.atom(),
            None => Atom::Null,
        }
    }
}

/// The free-form part of a row: takes any JSON, and any write that fits it.
impl Field for V {
    fn write_json(&self, encoder: &mut Encoder) -> Result<()> {
        V::write_json(self, encoder)
    }

    fn from_value(value: &V) -> std::result::Result<Self, Misfit> {
        if !value.depth_fits() {
            return Err(Misfit::new(TOO_DEEP));
        }
        Ok(value.clone())
    }

    fn canonicalize(&mut self) {
        V::canonicalize(self);
    }

    fn items(&self) -> Option<&dyn Items> {
        match self {
            V::List(items) => Some(items as &dyn Items),
            _ => None,
        }
    }

    fn part_items(&self, key: &str) -> Option<&dyn Items> {
        match self {
            V::Map(map) => map.get(key).and_then(Field::items),
            _ => None,
        }
    }
}

/// The text of `Maybe`'s error for a key it does not have; a `ValueError` in Python.
pub const ABSENT_KEY: &str = "an absent key has no value";

/// Why `from_value` refuses an int outside i64 where a row holds an i64.
const BIG_INT: &str = "an int too large";
/// Why `from_value` refuses a value deeper than `MAX_DEPTH`.
const TOO_DEEP: &str = "value nests too deeply";

/// The canonical JSON of a value read whole: an item of a list a row keeps as its bytes (a
/// history), as the PyO3 crate's `canonical_json` writes the Python value.
pub fn value_bytes(value: &V) -> std::result::Result<Box<[u8]>, Misfit> {
    if !value.depth_fits() {
        return Err(Misfit::new(TOO_DEEP));
    }
    let mut encoder = Encoder { out: Vec::with_capacity(256) };
    value.write_json(&mut encoder).map_err(|error| Misfit::new(error.to_string()))?;
    Ok(encoder.out.into_boxed_slice())
}

/// A key the record may not have: `{"a": 1}` and `{}`, where `Option` is `{"a": 1}` and
/// `{"a": None}`. A record's key that holds None is a misfit here unless T takes None.
#[derive(Clone)]
pub struct Maybe<T>(pub Option<T>);

impl<T: Field> Field for Maybe<T> {
    fn write_json(&self, encoder: &mut Encoder) -> Result<()> {
        match &self.0 {
            Some(value) => value.write_json(encoder),
            None => Err(Error::Value(ABSENT_KEY.to_owned())),
        }
    }

    /// A key the record has: one it lacks is `when_missing`.
    fn from_value(value: &V) -> std::result::Result<Self, Misfit> {
        Ok(Self(Some(T::from_value(value)?)))
    }

    fn items(&self) -> Option<&dyn Items> {
        self.0.as_ref().and_then(Field::items)
    }

    fn when_missing() -> Option<Self> {
        Some(Self(None))
    }

    fn is_absent(&self) -> bool {
        self.0.is_none()
    }

    fn canonicalize(&mut self) {
        if let Some(value) = &mut self.0 {
            value.canonicalize();
        }
    }
}

impl<T: Field> Field for IdMap<T> {
    fn write_json(&self, encoder: &mut Encoder) -> Result<()> {
        let mut entries: Vec<(&Arc<str>, &T)> = self.iter().collect();
        // Python orders str by code point, the same as UTF-8 bytes.
        entries.sort_unstable_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        encoder.out.push(b'{');
        for (index, (key, value)) in entries.into_iter().enumerate() {
            if index > 0 {
                encoder.out.push(b',');
            }
            write_json_string(&mut encoder.out, key);
            encoder.out.push(b':');
            value.write_json(encoder)?;
        }
        encoder.out.push(b'}');
        Ok(())
    }

    fn from_value(value: &V) -> std::result::Result<Self, Misfit> {
        let V::Map(entries) = value else {
            return Err(Misfit::new("not a dict"));
        };
        let mut map = IdMap::with_capacity_and_hasher(entries.len(), FxBuildHasher);
        for (key, item) in entries {
            map.insert(key.clone(), T::from_value(item)?);
        }
        Ok(map)
    }

    fn canonicalize(&mut self) {
        sort_keys(self);
        for value in self.values_mut() {
            value.canonicalize();
        }
    }
}

/// Sort a map's keys by their bytes: the order of `json.dumps(sort_keys=True)`, which compares str
/// by code point, the same as UTF-8 bytes. The keys are distinct, so no two compare equal.
pub fn sort_keys<K: AsRef<str>, T, S>(map: &mut IndexMap<K, T, S>) {
    if !map.keys().is_sorted_by(|first, second| first.as_ref().as_bytes() <= second.as_ref().as_bytes()) {
        map.sort_unstable_by(|first, _, second, _| first.as_ref().as_bytes().cmp(second.as_ref().as_bytes()));
    }
}

impl<T: Field> Items for Vec<T> {
    fn count(&self) -> usize {
        self.len()
    }

    fn item_json<'a>(&'a self, index: usize, scratch: &'a mut Encoder) -> Result<&'a [u8]> {
        scratch.out.clear();
        self[index].write_json(scratch)?;
        Ok(&scratch.out)
    }
}

impl<T: Field> Field for Vec<T> {
    fn write_json(&self, encoder: &mut Encoder) -> Result<()> {
        encoder.out.push(b'[');
        for (index, item) in self.iter().enumerate() {
            if index > 0 {
                encoder.out.push(b',');
            }
            item.write_json(encoder)?;
        }
        encoder.out.push(b']');
        Ok(())
    }

    fn from_value(value: &V) -> std::result::Result<Self, Misfit> {
        match value {
            V::List(items) => items.iter().map(T::from_value).collect(),
            _ => Err(Misfit::new("not a list")),
        }
    }

    fn items(&self) -> Option<&dyn Items> {
        Some(self)
    }

    fn canonicalize(&mut self) {
        for item in self.iter_mut() {
            item.canonicalize();
        }
    }
}

/// A mirrored write into a map of scalars: the row and the tree, or neither for the same bits.
pub fn map_set<T: Scalar, S: TreeSink + ?Sized>(
    map: &mut IdMap<T>,
    tree: &S,
    key: &str,
    value: T,
) -> std::result::Result<(), S::Error> {
    match map.get_mut(key) {
        Some(slot) if slot.same(&value) => {}
        Some(slot) => {
            tree.set_entry(key, value.atom())?;
            *slot = value;
        }
        None => {
            tree.set_entry(key, value.atom())?;
            map.insert(Arc::from(key), value);
        }
    }
    Ok(())
}

/// A closed set of strings, as `str_enum!` writes it.
pub trait StrEnum: Scalar + Copy {
    /// `"not a {Name}"`: why another str does not fit.
    const MISFIT: &'static str;
    fn as_text(self) -> &'static str;
    fn from_text(text: &str) -> Option<Self>;
}

// ---- The macros --------------------------------------------------------------------------------

/// The order that sorts names by their bytes, computed at compile time for `write_json`.
pub const fn sorted_order<const N: usize>(names: &[&str]) -> [usize; N] {
    const fn before(first: &[u8], second: &[u8]) -> bool {
        let mut index = 0;
        while index < first.len() && index < second.len() {
            if first[index] != second[index] {
                return first[index] < second[index];
            }
            index += 1;
        }
        first.len() < second.len()
    }
    let mut order = [0; N];
    let mut index = 0;
    while index < N {
        order[index] = index;
        index += 1;
    }
    let mut done = 1;
    while done < N {
        let mut place = done;
        while place > 0 && before(names[order[place]].as_bytes(), names[order[place - 1]].as_bytes()) {
            let moved = order[place];
            order[place] = order[place - 1];
            order[place - 1] = moved;
            place -= 1;
        }
        done += 1;
    }
    order
}

/// Whether two lists of names are the same, in the same order: for the PyO3 crate's `py_row!`,
/// which must list a struct's fields as `state_row!` did.
pub const fn same_names(first: &[&str], second: &[&str]) -> bool {
    if first.len() != second.len() {
        return false;
    }
    let mut index = 0;
    while index < first.len() {
        let (a, b) = (first[index].as_bytes(), second[index].as_bytes());
        if a.len() != b.len() {
            return false;
        }
        let mut byte = 0;
        while byte < a.len() {
            if a[byte] != b[byte] {
                return false;
            }
            byte += 1;
        }
        index += 1;
    }
    true
}

/// A record of a row: the struct, its `NAMES`, its `Field` and its mirrored setters. The PyO3
/// crate's `py_row!` gives it the Python side.
#[macro_export]
macro_rules! state_row {
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident {
            $( $field:ident : $kind:ty $(=> $setter:ident)? ),* $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Clone)]
        $vis struct $name {
            $( pub $field: $kind, )*
        }

        impl $name {
            /// The fields in the order they are listed: the order of the tree.
            pub const NAMES: &'static [&'static str] = &[$(stringify!($field)),*];
            const JSON_ORDER: [usize; $name::NAMES.len()] = $crate::row::sorted_order($name::NAMES);
            const JSON_WRITERS: &'static [fn(&Self, &mut $crate::json::Encoder) -> $crate::error::Result<()>] =
                &[$(|row, encoder| $crate::row::Field::write_json(&row.$field, encoder)),*];
            const JSON_ABSENT: &'static [fn(&Self) -> bool] =
                &[$(|row| $crate::row::Field::is_absent(&row.$field)),*];

            /// Every field with a setter takes next's value, through its setter: how a rule writes a
            /// record it planned.
            #[allow(dead_code)]
            pub fn set_all<S: $crate::row::TreeSink + ?Sized>(
                &mut self,
                tree: &S,
                next: &Self,
            ) -> ::core::result::Result<(), S::Error> {
                let _ = (&tree, &next);
                $($(
                    self.$setter(tree, next.$field.clone())?;
                )?)*
                Ok(())
            }

            $($(
                /// The row and the tree, or neither when the value has the same bits.
                pub fn $setter<S: $crate::row::TreeSink + ?Sized>(
                    &mut self,
                    tree: &S,
                    value: $kind,
                ) -> ::core::result::Result<(), S::Error> {
                    if !$crate::row::Scalar::same(&self.$field, &value) {
                        tree.set_field(stringify!($field), $crate::row::Scalar::atom(&value))?;
                        self.$field = value;
                    }
                    Ok(())
                }
            )?)*
        }

        impl $crate::row::Field for $name {
            fn write_json(&self, encoder: &mut $crate::json::Encoder) -> $crate::error::Result<()> {
                encoder.out.push(b'{');
                let mut first = true;
                for index in $name::JSON_ORDER.iter() {
                    if $name::JSON_ABSENT[*index](self) {
                        continue;
                    }
                    if !first {
                        encoder.out.push(b',');
                    }
                    first = false;
                    $crate::json::write_json_string(&mut encoder.out, $name::NAMES[*index]);
                    encoder.out.push(b':');
                    $name::JSON_WRITERS[*index](self, encoder)?;
                }
                encoder.out.push(b'}');
                Ok(())
            }

            fn from_value(
                value: &$crate::value::V,
            ) -> ::core::result::Result<Self, $crate::error::Misfit> {
                let $crate::value::V::Map(record) = value else {
                    return Err($crate::error::Misfit::new("not a dict"));
                };
                if record.len() > $name::NAMES.len() {
                    return Err($crate::error::Misfit::new("other fields than the row's"));
                }
                let mut present = 0;
                let row = Self {
                    $(
                        $field: match record.get(stringify!($field)) {
                            Some(item) => {
                                present += 1;
                                $crate::row::Field::from_value(item)?
                            }
                            None => $crate::row::Field::when_missing().ok_or_else(|| {
                                $crate::error::Misfit::new(
                                    concat!("other fields than the row's: no ", stringify!($field)),
                                )
                            })?,
                        },
                    )*
                };
                if present != record.len() {
                    return Err($crate::error::Misfit::new("other fields than the row's"));
                }
                Ok(row)
            }

            fn part_items(&self, key: &str) -> Option<&dyn $crate::row::Items> {
                $(
                    if key == stringify!($field) {
                        if $crate::row::Field::is_absent(&self.$field) {
                            return None;
                        }
                        return $crate::row::Field::items(&self.$field);
                    }
                )*
                None
            }

            fn canonicalize(&mut self) {
                $( $crate::row::Field::canonicalize(&mut self.$field); )*
            }
        }
    };
}

/// A closed set of strings in a row: one byte, no allocation, and any other string is a misfit.
///
/// ```ignore
/// sheltersim_core::str_enum! {
///     pub enum Phase { Idle = "idle", Busy = "busy" }
/// }
/// ```
#[macro_export]
macro_rules! str_enum {
    ($vis:vis enum $name:ident { $($member:ident = $text:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        $vis enum $name { $($member),+ }

        impl $name {
            #[allow(dead_code)]
            pub const fn text(self) -> &'static str {
                match self { $(Self::$member => $text),+ }
            }
        }

        impl $crate::row::StrEnum for $name {
            const MISFIT: &'static str = concat!("not a ", stringify!($name));

            fn as_text(self) -> &'static str {
                self.text()
            }

            fn from_text(text: &str) -> Option<Self> {
                match text {
                    $($text => Some(Self::$member),)+
                    _ => None,
                }
            }
        }

        impl $crate::row::Field for $name {
            fn write_json(&self, encoder: &mut $crate::json::Encoder) -> $crate::error::Result<()> {
                $crate::json::write_json_string(&mut encoder.out, self.text());
                Ok(())
            }

            fn from_value(
                value: &$crate::value::V,
            ) -> ::core::result::Result<Self, $crate::error::Misfit> {
                match value {
                    $crate::value::V::Str(text) => <Self as $crate::row::StrEnum>::from_text(text)
                        .ok_or_else(|| $crate::error::Misfit::new(<Self as $crate::row::StrEnum>::MISFIT)),
                    _ => Err($crate::error::Misfit::new("not a str")),
                }
            }

            fn canonicalize(&mut self) {}
        }

        impl $crate::row::Scalar for $name {
            fn same(&self, other: &Self) -> bool {
                self == other
            }

            fn atom(&self) -> $crate::row::Atom<'_> {
                $crate::row::Atom::Name(self.text())
            }
        }
    };
}
