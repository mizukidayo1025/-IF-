//! What the PyO3 crate read of a controller's objects for its `Policies`, without Python (Issue
//! #152, lane P8-0b; policies E7 and E14 of docs/rust-port/step5-plan.md).
//!
//! The kernels read the controller's policies and managers with `getattr` at every tick:
//! `psychology.parameters`, `decisions.policy.scorer`, `social_policy.limits` and so on. A `Spec`
//! names what a rule reads of an object; the PyO3 crate walks it once per controller and keeps each
//! attribute as a `Held`: a JSON value (`V`), a str of a str subclass, an object it descends into,
//! or an opaque value. Reading never raises: what an attribute read raised is kept as a `Fault`,
//! and the read site raises it as it does today, a refusal by its text and any other error by a
//! second read (policy E14).
//!
//! A class is kept by `__module__` and `__qualname__` and whether that name finds this very class
//! (`resolves`): a kernel that asks `type(x) is module.Name` asks `Class::is`, one that compares the
//! names asks `Class::has_name`.

use std::sync::Arc;

use indexmap::IndexMap;

use crate::config::read::{self, NOT_AN_INT, NOT_A_DICT};
use crate::config::read::Read;
use crate::config::Fault;
use crate::value::V;

/// The refusal of an attribute the controller's `Spec` does not read.
pub const NOT_READ: &str = "the run did not read this attribute";
/// The refusal of a `Run` made without a controller.
pub const NO_CONTROLLER: &str = "the run has no controller";
/// The refusal of a value that is not a plain JSON value.
pub const NOT_A_VALUE: &str = "a value is not a plain JSON value";
/// The refusal of a value that is not an object the run read.
pub const NOT_AN_OBJECT: &str = "a value is not an object the run read";

/// What a rule reads of an object.
#[derive(Debug)]
pub struct Spec {
    /// Attributes read with `getattr`, in this order.
    pub attrs: &'static [Attr],
    /// Also every entry of the object's own `__dict__` the attributes don't name, in its order.
    pub fields: bool,
    /// Names read from the class's own `__dict__` (not one it inherits), kept only when present.
    pub class_own: &'static [&'static str],
}

impl Spec {
    pub const fn of(attrs: &'static [Attr]) -> Self {
        Self { attrs, fields: false, class_own: &[] }
    }

    /// The attributes and every field of the object's `__dict__`.
    pub const fn with_fields(attrs: &'static [Attr]) -> Self {
        Self { attrs, fields: true, class_own: &[] }
    }
}

/// An attribute a `Spec` reads: a value, or an object read by its own `Spec`.
#[derive(Debug)]
pub struct Attr {
    pub name: &'static str,
    pub spec: Option<&'static Spec>,
}

impl Attr {
    pub const fn value(name: &'static str) -> Self {
        Self { name, spec: None }
    }

    pub const fn object(name: &'static str, spec: &'static Spec) -> Self {
        Self { name, spec: Some(spec) }
    }
}

/// A class by its names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Class {
    /// `__module__`, None where it is not a str.
    pub module: Option<Arc<str>>,
    /// `__qualname__`, None where it is not a str.
    pub qualname: Option<Arc<str>>,
    /// Whether `sys.modules[module]` reached by `qualname` is this class.
    pub resolves: bool,
}

impl Class {
    /// The class the name finds.
    pub fn named(module: &str, qualname: &str) -> Self {
        Self { module: Some(Arc::from(module)), qualname: Some(Arc::from(qualname)), resolves: true }
    }

    /// `type(x) is module.qualname`.
    pub fn is(&self, module: &str, qualname: &str) -> bool {
        self.resolves && self.has_name(module, qualname)
    }

    /// The class's names are these, whichever class they find.
    pub fn has_name(&self, module: &str, qualname: &str) -> bool {
        self.module.as_deref() == Some(module) && self.qualname.as_deref() == Some(qualname)
    }

    /// Both names are str.
    pub fn has_plain_names(&self) -> bool {
        self.module.is_some() && self.qualname.is_some()
    }
}

/// An attribute's value as the run read it.
#[derive(Clone, Debug, PartialEq)]
pub enum Held {
    /// A plain JSON value (`VPy::from_py`): None, bool, int, float, str, dict and list.
    Value(V),
    /// An instance of a str subclass (a `StrEnum`), by its text.
    Text(Arc<str>),
    /// An object its `Spec` reads.
    Object(Arc<Obj>),
    /// Anything else, which a rule may only call or compare.
    Opaque { callable: bool },
    /// A method bound to the controller the Run read, whose function is the controller's step of
    /// this name as the driver installed it (`segments.py`): `staff_service.route_resolver` is
    /// `controller._route` (lane b3).
    Method(Arc<str>),
}

impl Held {
    /// `value is None`.
    pub fn is_none(&self) -> bool {
        matches!(self, Held::Value(V::Null))
    }

    /// The JSON value, or a refusal.
    pub fn value(&self) -> Read<&V> {
        match self {
            Held::Value(value) => Ok(value),
            _ => Err(Fault::refused(NOT_A_VALUE)),
        }
    }

    /// The text of a str or of a str subclass (`isinstance(value, str)`).
    pub fn text(&self) -> Option<&str> {
        match self {
            Held::Value(V::Str(text)) | Held::Text(text) => Some(text),
            _ => None,
        }
    }

    /// The object, None for None, or a refusal.
    pub fn object(&self) -> Read<Option<&Arc<Obj>>> {
        match self {
            Held::Object(object) => Ok(Some(object)),
            Held::Value(V::Null) => Ok(None),
            _ => Err(Fault::refused(NOT_AN_OBJECT)),
        }
    }
}

/// An object as its `Spec` read it.
#[derive(Clone, Debug, PartialEq)]
pub struct Obj {
    pub class: Class,
    /// By name: the attributes of the `Spec` in its order, then the other fields of `__dict__`.
    pub attrs: IndexMap<Arc<str>, Read<Held>>,
    /// The names of `Spec::class_own` the class's own `__dict__` has.
    pub class_own: IndexMap<Arc<str>, Held>,
}

impl Obj {
    pub fn new(class: Class) -> Self {
        Self { class, attrs: IndexMap::new(), class_own: IndexMap::new() }
    }

    /// The attribute as read, or what its read raised. One no `Spec` reads is a refusal.
    pub fn get(&self, name: &str) -> Read<&Held> {
        match self.attrs.get(name) {
            Some(Ok(held)) => Ok(held),
            Some(Err(fault)) => Err(fault.clone()),
            None => Err(Fault::refused(NOT_READ)),
        }
    }

    /// The attribute's JSON value.
    pub fn value(&self, name: &str) -> Read<&V> {
        self.get(name)?.value()
    }

    /// The attribute's object, None for None.
    pub fn object(&self, name: &str) -> Read<Option<&Arc<Obj>>> {
        self.get(name)?.object()
    }

    /// The attribute's object, a refusal for None.
    pub fn some_object(&self, name: &str) -> Read<&Arc<Obj>> {
        self.object(name)?.ok_or_else(|| Fault::refused(NOT_AN_OBJECT))
    }

    /// `getattr(x, name) is None`.
    pub fn is_none(&self, name: &str) -> Read<bool> {
        Ok(self.get(name)?.is_none())
    }

    /// `exact_int` of the attribute.
    pub fn int(&self, name: &str) -> Read<i64> {
        match self.get(name)? {
            Held::Value(value) => read::int(value),
            _ => Err(Fault::refused(NOT_AN_INT)),
        }
    }

    /// `exact_dict` of the attribute.
    pub fn map(&self, name: &str) -> Read<&IndexMap<Arc<str>, V>> {
        match self.get(name)? {
            Held::Value(value) => read::map(value),
            _ => Err(Fault::refused(NOT_A_DICT)),
        }
    }

    /// An attribute of an attribute, as `getattr(getattr(x, a), b)` reads it.
    pub fn path(&self, names: &[&str]) -> Read<&Held> {
        let Some((last, inner)) = names.split_last() else {
            return Err(Fault::refused(NOT_READ));
        };
        let mut object = self;
        for name in inner {
            object = object.some_object(name)?;
        }
        object.get(last)
    }
}

/// The class of an object attribute, or the fault that keeps it from being one.
pub fn class_of<'a>(held: &'a Read<Held>) -> Read<Option<&'a Class>> {
    match held {
        Ok(held) => Ok(held.object()?.map(|object| &object.class)),
        Err(fault) => Err(fault.clone()),
    }
}
