//! The simulation core of 避難所IF！ without Python (Issue #152, stage 3 of #132).
//!
//! This crate must never depend on pyo3: CI checks `cargo tree -p sheltersim_core`. The PyO3
//! crate `sheltersim_native` (`native/src`) converts Python values, keeps the Python mirrors and
//! maps this crate's errors to Python's exceptions. The plan of the split is in
//! `docs/rust-port/step5-plan.md`.

pub mod census;
pub mod interner;
pub mod json;
pub mod mathx;
pub mod parallel;
pub mod rng;
