//! The simulation core of 避難所IF！ without Python (Issue #152, stage 3 of #132).
//!
//! This crate must never depend on pyo3: CI checks `cargo tree -p sheltersim_core`. The PyO3
//! crate `sheltersim_native` (`native/src`) converts Python values, keeps the Python mirrors and
//! maps this crate's errors to Python's exceptions. The plan of the split is in
//! `docs/rust-port/step5-plan.md`.

pub mod archive;
pub mod canonical;
pub mod cell;
pub mod census;
pub mod checkpoint;
pub mod config;
pub mod contacts;
pub mod engine;
pub mod entities;
pub mod error;
pub mod events;
pub mod frame;
pub mod geometry;
pub mod history;
pub mod host;
pub mod interner;
pub mod json;
pub mod load;
pub mod mathx;
pub mod parallel;
pub mod plain;
pub mod program;
pub mod rng;
pub mod routes;
pub mod row;
pub mod rules;
pub mod run;
pub mod scenario;
pub mod stamp;
pub mod summary;
pub mod text;
pub mod txn;

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
