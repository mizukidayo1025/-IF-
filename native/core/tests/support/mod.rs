//! What the core's tests share (not part of the crate: decision 4 is about its API).
//!
//! `TestHost` is the host of a build without Python (orchestrator decision (c) on design-p8.md
//! section 4.3): its `contract` gives its input back as given, its LLM methods do what a Run
//! without an LLM does (nothing; `llm()` says there is none), its sinks write down each call, and
//! the whole world's validation passes (policy E6: the Python checks are not in the core in #152).

#![allow(dead_code)]

use std::sync::Arc;

use sheltersim_core::error::Error;
use sheltersim_core::host::{Contract, Decision, DecisionSink, Host, Llm, Part, Phase, Stage, Trace, TraceLevel, TxnKind};
use sheltersim_core::rules::metrics::MetricSample;
use sheltersim_core::value::V;
use sheltersim_core::world::Snapshot;

/// One call a `TestHost` was asked.
#[derive(Clone, Debug, PartialEq)]
pub enum Call {
    Validate(TxnKind, Stage),
    Canonicalized(TxnKind),
    Restored(TxnKind),
    Committed(TxnKind),
    ValidateWorld,
    ValidateExtensions,
    Contract(Contract, V),
    Prioritize { request: Arc<str>, agent: Arc<str>, hash: Arc<str> },
    SupportRequest { agent: Arc<str>, request: Arc<str> },
    Complaint { agent: Arc<str>, request: Arc<str> },
    Cooperation { initiator: Arc<str>, participant: Arc<str>, trigger: Arc<str> },
    InformationReaction { agent: Arc<str>, participant: Arc<str>, message: Arc<str>, trigger: Arc<str> },
    Metric(MetricSample, i64),
    Decision(Decision),
    Trace(Trace),
    Phase(Phase, bool),
    SpaceRefreshed,
    Draw(Vec<Part>),
}

/// A host without Python that writes down what it was asked.
#[derive(Debug)]
pub struct TestHost {
    pub calls: Vec<Call>,
    pub llm: Llm,
    pub sink: DecisionSink,
    pub level: TraceLevel,
    /// The draws `draw` gives, in turn; none means it refuses as every host but a test's does.
    pub draws: Vec<f64>,
}

impl Default for TestHost {
    fn default() -> Self {
        Self { calls: Vec::new(), llm: Llm::default(), sink: DecisionSink::Silent, level: TraceLevel::Off, draws: Vec::new() }
    }
}

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

impl Host for TestHost {
    type Error = Error;

    fn validate_candidate(&mut self, kind: TxnKind, stage: Stage) -> Result<(), Error> {
        self.calls.push(Call::Validate(kind, stage));
        Ok(())
    }

    fn canonicalized(&mut self, kind: TxnKind, _before: &Snapshot) -> Result<(), Error> {
        self.calls.push(Call::Canonicalized(kind));
        Ok(())
    }

    fn restored(&mut self, kind: TxnKind) -> Result<(), Error> {
        self.calls.push(Call::Restored(kind));
        Ok(())
    }

    fn committed(&mut self, kind: TxnKind) -> Result<(), Error> {
        self.calls.push(Call::Committed(kind));
        Ok(())
    }

    fn validate_world(&mut self) -> Result<(), Error> {
        self.calls.push(Call::ValidateWorld);
        Ok(())
    }

    fn validate_extensions(&mut self) -> Result<(), Error> {
        self.calls.push(Call::ValidateExtensions);
        Ok(())
    }

    fn contract(&mut self, which: &Contract, input: &V) -> Result<V, Error> {
        self.calls.push(Call::Contract(which.clone(), input.clone()));
        Ok(input.clone())
    }

    fn llm(&self) -> Result<Llm, Error> {
        Ok(self.llm)
    }

    fn prioritize_support_request(&mut self, request: &str, agent: &str, hash: &str) -> Result<(), Error> {
        self.calls.push(Call::Prioritize { request: text(request), agent: text(agent), hash: text(hash) });
        Ok(())
    }

    fn consider_support_request(&mut self, agent: &str, request: &str) -> Result<(), Error> {
        self.calls.push(Call::SupportRequest { agent: text(agent), request: text(request) });
        Ok(())
    }

    fn consider_complaint_response(&mut self, agent: &str, request: &str) -> Result<(), Error> {
        self.calls.push(Call::Complaint { agent: text(agent), request: text(request) });
        Ok(())
    }

    fn consider_cooperation(&mut self, initiator: &str, participant: &str, trigger: &str) -> Result<(), Error> {
        self.calls.push(Call::Cooperation { initiator: text(initiator), participant: text(participant), trigger: text(trigger) });
        Ok(())
    }

    fn consider_information_reaction(&mut self, agent: &str, participant: &str, message: &str, trigger: &str) -> Result<(), Error> {
        self.calls.push(Call::InformationReaction {
            agent: text(agent),
            participant: text(participant),
            message: text(message),
            trigger: text(trigger),
        });
        Ok(())
    }

    fn capture_metric(&mut self, sample: &MetricSample, minute: i64) -> Result<(), Error> {
        self.calls.push(Call::Metric(sample.clone(), minute));
        Ok(())
    }

    fn decision_sink(&self) -> Result<DecisionSink, Error> {
        Ok(self.sink)
    }

    fn decision(&mut self, decision: Decision) -> Result<(), Error> {
        self.calls.push(Call::Decision(decision));
        Ok(())
    }

    fn trace_level(&self) -> Result<TraceLevel, Error> {
        Ok(self.level)
    }

    fn trace(&mut self, document: Trace) -> Result<(), Error> {
        self.calls.push(Call::Trace(document));
        Ok(())
    }

    fn phase(&mut self, phase: Phase, before: bool) -> Result<(), Error> {
        self.calls.push(Call::Phase(phase, before));
        Ok(())
    }

    fn space_refreshed(&mut self) -> Result<(), Error> {
        self.calls.push(Call::SpaceRefreshed);
        Ok(())
    }

    fn draw(&mut self, parts: &[Part]) -> Result<f64, Error> {
        self.calls.push(Call::Draw(parts.to_vec()));
        if self.draws.is_empty() {
            return Err(Error::unsupported(sheltersim_core::host::NO_DRAW));
        }
        Ok(self.draws.remove(0))
    }
}
