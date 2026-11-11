//! The models the configs select: `contracts.policies.selected_models(model_configs)`, a map from a
//! domain to a model ID (design-p7.md §1.1, rows 4–4e).

use std::sync::Arc;

/// Domain and model ID, in the order of the Python dict.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Selection {
    models: Box<[(Arc<str>, Arc<str>)]>,
}

impl Selection {
    /// The pairs in the dict's order; a dict has no key twice.
    pub fn new(models: Vec<(Arc<str>, Arc<str>)>) -> Self {
        Self { models: models.into_boxed_slice() }
    }

    /// The model selected for a domain, or None where the dict has no such key.
    pub fn get(&self, domain: &str) -> Option<&Arc<str>> {
        self.models.iter().find(|(name, _)| &**name == domain).map(|(_, model)| model)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.models.iter().map(|(domain, model)| (&**domain, &**model))
    }

    pub fn len(&self) -> usize {
        self.models.len()
    }

    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }
}
