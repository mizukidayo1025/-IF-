//! The simulation core of 避難所IF！ without Python (Issue #152, stage 3 of #132).
//!
//! This crate must never depend on pyo3: CI checks `cargo tree -p sheltersim_core`. The PyO3
//! crate `sheltersim_native` (`native/src`) converts Python values, keeps the Python mirrors and
//! maps this crate's errors to Python's exceptions. The plan of the split is in
//! `docs/rust-port/step5-plan.md`.

pub mod cell;
pub mod census;
pub mod checkpoint;
pub mod config;
pub mod contacts;
pub mod entities;
pub mod error;
pub mod events;
pub mod geometry;
pub mod interner;
pub mod json;
pub mod mathx;
pub mod parallel;
pub mod rng;
pub mod routes;
pub mod row;

/// The kernels moved to the core: pure functions over `&World`, whose PyO3 crate's file of the same
/// name gathers what Python holds and makes the Python values (lane 5-4, step5-plan.md).
pub mod rules {
    pub mod metrics;
}
pub mod run;
pub mod stamp;

/// The rows of the namespaces of `model_state` and of the owned world fields. The PyO3 crate's
/// `state/<name>.rs` of the same name reads each from Python and holds its validators.
pub mod state {
    pub mod agent_dynamics;
    pub mod body_state;
    pub mod effect_program;
    pub mod facility_queues;
    pub mod information;
    pub mod metric_state;
    pub mod movement;
    pub mod psychological_distress;
    pub mod resource_ledger;
    pub mod resource_service;
    pub mod shared_operations;
    pub mod social_network;
    pub mod social_propagation;
    pub mod staff_dynamics;
    pub mod staff_operations;
}
pub mod value;
pub mod world;
