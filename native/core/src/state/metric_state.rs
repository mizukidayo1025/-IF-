//! `world.metric_state`: floats by name, a world field rather than a part of `model_state`
//! (`OWNED_FIELDS` in `core/state_mirror.py`). Python's rules write single keys here, and each
//! write is forwarded; the distress monitor of `rules/psychology.rs` is Rust's own writer.

use crate::row::IdMap;

pub type MetricRow = IdMap<f64>;
