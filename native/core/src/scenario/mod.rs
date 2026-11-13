//! Building a Scenario in the browser without Python (Issue #291, under #285): the pieces of
//! `experiments/initial_scenario.py`'s `InitialScenarioBuilder` and the contracts it reads, each a
//! function with typed input and output that gives what the Python code gives for the same input.
//! The builder (#299) joins them.

pub mod request;
pub mod text;
