//! Where a Run's records go and whether it has an LLM: `controller.artifacts` (its decision sink
//! and trace level), `controller.llm_router` and `controller.social_policy` (lane P8-0b).
//!
//! The kernels read these at each step (`decisions.rs` `sink_of`, `trace.rs` `Level::of`,
//! `proposals.rs` and `staff.rs`); the Run reads them once. Whether a trace writer has started or
//! finished changes during the Run, so the `Host` asks that where it records.

use std::sync::Arc;

use crate::config::read::Read;
use crate::config::{Fault, FaultClass};
use crate::value::V;

use super::held::{Attr, Held, Obj, Spec};

/// `controller.artifacts`: the trace level, and `decision_sink` of its own class.
pub const ARTIFACTS: Attr = Attr::object("artifacts", &ARTIFACTS_SPEC);
const ARTIFACTS_SPEC: Spec = Spec { attrs: &[Attr::value("trace_level")], fields: false, class_own: &["decision_sink"], own: &[] };

/// `controller.llm_router`, read for whether it is None.
pub const LLM_ROUTER: Attr = Attr::value("llm_router");

/// The refusal of a trace level `Level::of` does not know.
pub const UNKNOWN_LEVEL: &str = "a trace level this kernel does not know";

/// What a sink does with a decision (`decision_sink` of the sink's own class).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecisionSink {
    /// `"silent"`: nothing (`_MetricSink`).
    Silent,
    /// `"indexed"`: the record's JSON bytes go into `observations` (`_IndexedMetricSink`).
    Indexed,
    /// `"artifacts"`: `RunArtifactWriter`, which takes the document while its trace is on.
    Artifacts,
    /// No such declaration, another word, or no `artifacts`: `record_decision` runs in Python.
    Python,
}

/// `artifacts.trace_level`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TraceLevel {
    Off,
    Summary,
    Full,
}

/// Whether the Run has an LLM: `llm_router is not None`, `social_policy is not None`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Llm {
    pub router: bool,
    pub social_policy: bool,
}

/// What the Run read of its sinks and its LLM.
#[derive(Clone, Debug, PartialEq)]
pub struct Sinks {
    pub decision_sink: Read<DecisionSink>,
    pub trace_level: Read<TraceLevel>,
    pub llm_router: Read<bool>,
    pub social_policy: Read<bool>,
}

impl Sinks {
    pub fn of(controller: &Obj) -> Self {
        let artifacts = controller.get("artifacts");
        Self {
            decision_sink: decision_sink(artifacts.clone().map(Held::object)),
            trace_level: artifacts.and_then(|held| match held.object()? {
                Some(artifacts) => trace_level(artifacts),
                // `getattr(None, "trace_level")`.
                None => Err(Fault::new(
                    FaultClass::Other(Arc::from("builtins.AttributeError")),
                    "'NoneType' object has no attribute 'trace_level'",
                )),
            }),
            llm_router: controller.is_none("llm_router").map(|none| !none),
            social_policy: controller.is_none("social_policy").map(|none| !none),
        }
    }

    /// Both flags, or the first fault.
    pub fn llm(&self) -> Read<Llm> {
        Ok(Llm { router: self.llm_router.clone()?, social_policy: self.social_policy.clone()? })
    }

    /// Every field the refusal of a Run without a controller.
    pub fn absent() -> Self {
        let fault = || Fault::refused(super::held::NO_CONTROLLER);
        Self { decision_sink: Err(fault()), trace_level: Err(fault()), llm_router: Err(fault()), social_policy: Err(fault()) }
    }
}

/// `sink_of` of `decisions.rs` without its moment: no `artifacts`, or a class without its own
/// `decision_sink`, is Python's; a declaration that is not exactly a str raises (`TypeError`).
fn decision_sink(artifacts: Read<Read<Option<&Arc<Obj>>>>) -> Read<DecisionSink> {
    let Ok(Ok(Some(artifacts))) = artifacts else {
        return Ok(DecisionSink::Python);
    };
    let Some(kind) = artifacts.class_own.get("decision_sink") else {
        return Ok(DecisionSink::Python);
    };
    let Held::Value(V::Str(kind)) = kind else {
        return Err(Fault::new(FaultClass::Type, "decision_sink is not a str"));
    };
    Ok(match &**kind {
        "silent" => DecisionSink::Silent,
        "indexed" => DecisionSink::Indexed,
        "artifacts" => DecisionSink::Artifacts,
        _ => DecisionSink::Python,
    })
}

/// `Level::of` of `trace.rs`: a str or a str subclass, one of three words.
fn trace_level(artifacts: &Obj) -> Read<TraceLevel> {
    let level = artifacts.get("trace_level")?;
    let Some(text) = level.text() else {
        return Err(Fault::new(FaultClass::Type, "trace_level is not a str"));
    };
    match text {
        "off" => Ok(TraceLevel::Off),
        "summary" => Ok(TraceLevel::Summary),
        "full" => Ok(TraceLevel::Full),
        _ => Err(Fault::refused(UNKNOWN_LEVEL)),
    }
}
