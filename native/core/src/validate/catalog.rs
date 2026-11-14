//! What the validation reads of the installed code (`catalog.json`): the Model Catalog
//! (`MODEL_DEFINITIONS` of `contracts/policies.py` with `model_catalog`'s state schema and
//! `has_installed_factory`), `DEFAULT_MODELS`, `CORE_MODEL_SUITES`, the installed state contracts
//! (`_CONTRACTS` of `contracts/runtime.py`) and the psychology models of each kind
//! (`domain/psychological_state.py`).
//!
//! `tests/support/core_validate.py` (`validate_catalog`) writes the file from Python, and
//! `integration/test_core_fixtures.py` fails when it is not what Python has now.

use std::sync::{Arc, OnceLock};

use indexmap::IndexMap;
use serde_json::Value;

use crate::error::{Error, Result};
use crate::value::V;

/// The bounds of a `behavior.` parameter of a model.
#[derive(Clone, Debug)]
pub struct Bounds {
    pub lower: Option<V>,
    pub upper: Option<V>,
    /// `metadata.get("lower_exclusive")`, truthy.
    pub lower_exclusive: bool,
}

/// A model of the Model Catalog.
#[derive(Clone, Debug)]
pub struct ModelDefinition {
    pub domain: Arc<str>,
    /// `has_installed_factory(domain, model_id)`.
    pub factory: bool,
    /// `required_state_contracts`: a dict of namespace to signature, or another value (which
    /// `selected_models` refuses).
    pub required_state_contracts: V,
    pub state_schema_version: i64,
    /// The parameters whose name starts with `behavior.`, by name.
    pub behavior_parameters: IndexMap<Arc<str>, Bounds>,
}

/// An installed state contract (`StateDescriptor`).
#[derive(Clone, Debug)]
pub struct StateContract {
    pub namespace: Arc<str>,
    pub implementation_id: Arc<str>,
    pub implementation_version: Arc<str>,
    pub schema_version: i64,
    pub policy_domain: Option<Arc<str>>,
}

/// The catalog.
#[derive(Debug)]
pub struct Catalog {
    /// `MODEL_DEFINITIONS`, by model ID in its order.
    pub models: IndexMap<Arc<str>, ModelDefinition>,
    pub default_models: Vec<(Arc<str>, Arc<str>)>,
    pub suites: IndexMap<Arc<str>, Vec<(Arc<str>, Arc<str>)>>,
    /// By namespace, sorted.
    pub state_contracts: IndexMap<Arc<str>, StateContract>,
    pub life_state_psychology: Vec<Arc<str>>,
    pub mean_reverting_psychology: Vec<Arc<str>>,
}

impl Catalog {
    /// Whether a model is a life-state psychology model (`LIFE_STATE_PSYCHOLOGY_POLICIES`).
    pub fn is_life_state(&self, model: &str) -> bool {
        self.life_state_psychology.iter().any(|known| &**known == model)
    }

    /// Whether a model is a mean-reverting psychology model (`MEAN_REVERTING_PSYCHOLOGY_POLICIES`).
    pub fn is_mean_reverting(&self, model: &str) -> bool {
        self.mean_reverting_psychology.iter().any(|known| &**known == model)
    }

    /// Whether some model of the catalog has this domain.
    pub fn has_domain(&self, domain: &str) -> bool {
        self.models.values().any(|model| &*model.domain == domain)
    }
}

fn bad() -> Error {
    Error::unsupported("the validation's catalog does not read")
}

fn text(value: &Value) -> Result<Arc<str>> {
    value.as_str().map(Arc::from).ok_or_else(bad)
}

fn pairs(value: &Value) -> Result<Vec<(Arc<str>, Arc<str>)>> {
    value.as_array().ok_or_else(bad)?.iter().map(|pair| Ok((text(&pair[0])?, text(&pair[1])?))).collect()
}

fn texts(value: &Value) -> Result<Vec<Arc<str>>> {
    value.as_array().ok_or_else(bad)?.iter().map(text).collect()
}

fn bound(value: &Value) -> Result<Option<V>> {
    match value {
        Value::Null => Ok(None),
        other => V::from_json(other).map(Some).ok_or_else(bad),
    }
}

fn read() -> Result<Catalog> {
    let json: Value = serde_json::from_str(include_str!("catalog.json")).map_err(|_| bad())?;
    let mut models = IndexMap::new();
    for model in json["models"].as_array().ok_or_else(bad)? {
        let mut behavior_parameters = IndexMap::new();
        for (name, metadata) in model["behavior_parameters"].as_object().ok_or_else(bad)? {
            let bounds = Bounds {
                lower: bound(&metadata["lower"])?,
                upper: bound(&metadata["upper"])?,
                lower_exclusive: metadata["lower_exclusive"].as_bool().unwrap_or(false),
            };
            behavior_parameters.insert(Arc::from(name.as_str()), bounds);
        }
        let definition = ModelDefinition {
            domain: text(&model["domain"])?,
            factory: model["factory"].as_bool().ok_or_else(bad)?,
            required_state_contracts: V::from_json(&model["required_state_contracts"]).ok_or_else(bad)?,
            state_schema_version: model["state_schema_version"].as_i64().ok_or_else(bad)?,
            behavior_parameters,
        };
        models.insert(text(&model["id"])?, definition);
    }
    let mut suites = IndexMap::new();
    for (suite, members) in json["suites"].as_object().ok_or_else(bad)? {
        suites.insert(Arc::from(suite.as_str()), pairs(members)?);
    }
    let mut state_contracts = IndexMap::new();
    for contract in json["state_contracts"].as_array().ok_or_else(bad)? {
        let contract = StateContract {
            namespace: text(&contract["namespace"])?,
            implementation_id: text(&contract["implementation_id"])?,
            implementation_version: text(&contract["implementation_version"])?,
            schema_version: contract["schema_version"].as_i64().ok_or_else(bad)?,
            policy_domain: contract["policy_domain"].as_str().map(Arc::from),
        };
        state_contracts.insert(contract.namespace.clone(), contract);
    }
    Ok(Catalog {
        models,
        default_models: pairs(&json["default_models"])?,
        suites,
        state_contracts,
        life_state_psychology: texts(&json["life_state_psychology"])?,
        mean_reverting_psychology: texts(&json["mean_reverting_psychology"])?,
    })
}

/// The catalog, read once.
pub fn catalog() -> Result<&'static Catalog> {
    static CATALOG: OnceLock<std::result::Result<Catalog, Error>> = OnceLock::new();
    CATALOG.get_or_init(read).as_ref().map_err(Clone::clone)
}
