//! Behavioral verification of a stage against its own examples.
//!
//! Every stage ships with `examples: [{input, output}, ...]` used for
//! semantic search, documentation, and — in this module — as a minimal
//! behavioral test suite. [`verify_stage`] runs each example through an
//! executor and compares the actual output against the declared output
//! via canonical JSON hashing.
//!
//! ## What gets tested
//!
//! The [`StageSkipReason`] enum identifies stages whose example outputs
//! are **illustrative, not reproducible**: network calls, LLM inference,
//! explicitly non-deterministic effects, and time-sensitive stages. For
//! those, a hash comparison would fail spuriously; callers receive a
//! `Skipped { reason }` outcome instead of `Failed` so CI gates can be
//! set accordingly.
//!
//! For everything else (`Pure`, plain `Fallible`) the comparison is
//! exact — if the implementation returns the wrong shape or the wrong
//! value, the test fails.

use noether_core::effects::Effect;
use noether_core::stage::Stage;
use serde_json::Value;

use crate::executor::{ExecutionError, StageExecutor};

/// Per-example verification outcome.
#[derive(Debug, Clone, PartialEq)]
pub enum ExampleOutcome {
    /// Implementation produced the declared output (canonical hash match).
    Ok,
    /// Implementation produced a different output.
    Mismatch { expected: Value, actual: Value },
    /// Executor returned an error.
    Errored { message: String },
}

/// Why a stage's behavioral verification is not meaningful.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageSkipReason {
    /// Stage carries `Effect::Network`.
    Network,
    /// Stage carries `Effect::Llm`.
    Llm,
    /// Stage carries `Effect::NonDeterministic`.
    NonDeterministic,
    /// Stage carries `Effect::Process` — touches external process state.
    Process,
    /// Stage has no examples to verify against.
    NoExamples,
    /// Executor reports no implementation for this stage ID.
    NoImplementation,
}

impl std::fmt::Display for StageSkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network => write!(f, "network effect — example outputs are illustrative"),
            Self::Llm => write!(f, "LLM effect — output is non-reproducible"),
            Self::NonDeterministic => write!(f, "non-deterministic effect"),
            Self::Process => write!(f, "process effect — side-effectful"),
            Self::NoExamples => write!(f, "no examples declared"),
            Self::NoImplementation => write!(f, "no implementation available in this executor"),
        }
    }
}

/// Verification report for a single stage.
#[derive(Debug, Clone)]
pub struct StageReport {
    pub stage_id: String,
    pub description: String,
    pub outcome: ReportOutcome,
}

#[derive(Debug, Clone)]
pub enum ReportOutcome {
    /// Stage was skipped — no verdict.
    Skipped { reason: StageSkipReason },
    /// Stage was tested. Individual example results live in `examples`.
    Tested { examples: Vec<ExampleOutcome> },
}

impl StageReport {
    /// True when every example matched (or the stage was skipped).
    pub fn passed(&self) -> bool {
        match &self.outcome {
            ReportOutcome::Skipped { .. } => true,
            ReportOutcome::Tested { examples } => {
                examples.iter().all(|e| matches!(e, ExampleOutcome::Ok))
            }
        }
    }

    /// True when any example failed to match the declared output.
    pub fn failed(&self) -> bool {
        matches!(&self.outcome, ReportOutcome::Tested { examples }
            if examples.iter().any(|e| !matches!(e, ExampleOutcome::Ok)))
    }
}

/// Decide whether a stage's behavioral verification is meaningful.
fn skip_reason(stage: &Stage) -> Option<StageSkipReason> {
    if stage.examples.is_empty() {
        return Some(StageSkipReason::NoExamples);
    }
    for effect in stage.signature.effects.iter() {
        match effect {
            Effect::Network => return Some(StageSkipReason::Network),
            Effect::Llm { .. } => return Some(StageSkipReason::Llm),
            Effect::NonDeterministic => return Some(StageSkipReason::NonDeterministic),
            Effect::Process => return Some(StageSkipReason::Process),
            _ => {}
        }
    }
    None
}

/// Canonical comparison — two JSON values are equal iff their
/// JCS-canonical byte strings match. Tolerates field-order differences
/// and numeric canonicalisation (`1.0` vs `1`).
fn canonical_eq(a: &Value, b: &Value) -> bool {
    match (serde_jcs::to_vec(a), serde_jcs::to_vec(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b, // fall back to structural equality if JCS fails
    }
}

/// Run every example through the executor and return a report.
///
/// Skipped stages short-circuit before touching the executor. For tested
/// stages, each example produces an [`ExampleOutcome`].
pub fn verify_stage<E: StageExecutor>(stage: &Stage, executor: &E) -> StageReport {
    if let Some(reason) = skip_reason(stage) {
        return StageReport {
            stage_id: stage.id.0.clone(),
            description: stage.description.clone(),
            outcome: ReportOutcome::Skipped { reason },
        };
    }

    let mut examples = Vec::with_capacity(stage.examples.len());
    for example in &stage.examples {
        let outcome = match executor.execute(&stage.id, &example.input) {
            Ok(actual) => {
                if canonical_eq(&actual, &example.output) {
                    ExampleOutcome::Ok
                } else {
                    ExampleOutcome::Mismatch {
                        expected: example.output.clone(),
                        actual,
                    }
                }
            }
            Err(ExecutionError::StageNotFound(_)) => {
                return StageReport {
                    stage_id: stage.id.0.clone(),
                    description: stage.description.clone(),
                    outcome: ReportOutcome::Skipped {
                        reason: StageSkipReason::NoImplementation,
                    },
                };
            }
            Err(e) => ExampleOutcome::Errored {
                message: format!("{e}"),
            },
        };
        examples.push(outcome);
    }

    StageReport {
        stage_id: stage.id.0.clone(),
        description: stage.description.clone(),
        outcome: ReportOutcome::Tested { examples },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::ExecutionError;
    use noether_core::capability::Capability;
    use noether_core::effects::EffectSet;
    use noether_core::stage::{CostEstimate, Example, Stage, StageId, StageSignature};
    use noether_core::types::NType;
    use serde_json::json;
    use std::collections::BTreeSet;

    /// Mock executor that returns a fixed value, ignoring input.
    struct ConstExec {
        out: Value,
    }

    impl StageExecutor for ConstExec {
        fn execute(
            &self,
            _id: &StageId,
            _input: &Value,
        ) -> Result<Value, crate::executor::ExecutionError> {
            Ok(self.out.clone())
        }
    }

    /// Mock executor that mirrors input back as output.
    struct EchoExec;

    impl StageExecutor for EchoExec {
        fn execute(&self, _id: &StageId, input: &Value) -> Result<Value, ExecutionError> {
            Ok(input.clone())
        }
    }

    fn make_stage(effects: EffectSet, examples: Vec<Example>) -> Stage {
        Stage {
            id: StageId("test-stage".into()),
            signature_id: None,
            signature: StageSignature {
                input: NType::Any,
                output: NType::Any,
                effects,
                implementation_hash: "hash".into(),
            },
            capabilities: BTreeSet::new(),
            cost: CostEstimate {
                time_ms_p50: None,
                tokens_est: None,
                memory_mb: None,
            },
            description: "test".into(),
            examples,
            lifecycle: noether_core::stage::StageLifecycle::Active,
            ed25519_signature: None,
            signer_public_key: None,
            implementation_code: None,
            implementation_language: None,
            ui_style: None,
            tags: vec![],
            aliases: vec![],
            name: None,
        }
    }

    #[test]
    fn pure_stage_passes_when_executor_matches() {
        let stage = make_stage(
            EffectSet::pure(),
            vec![Example {
                input: json!({"x": 1}),
                output: json!({"x": 1}),
            }],
        );
        let report = verify_stage(&stage, &EchoExec);
        assert!(report.passed());
    }

    #[test]
    fn pure_stage_fails_when_executor_diverges() {
        let stage = make_stage(
            EffectSet::pure(),
            vec![Example {
                input: json!({"x": 1}),
                output: json!({"x": 2}),
            }],
        );
        let report = verify_stage(
            &stage,
            &ConstExec {
                out: json!({"x": 1}),
            },
        );
        assert!(report.failed());
    }

    #[test]
    fn network_stage_is_skipped() {
        let stage = make_stage(
            EffectSet::new(vec![Effect::Network]),
            vec![Example {
                input: json!(null),
                output: json!(null),
            }],
        );
        let report = verify_stage(&stage, &EchoExec);
        assert!(matches!(
            report.outcome,
            ReportOutcome::Skipped {
                reason: StageSkipReason::Network
            }
        ));
    }

    #[test]
    fn llm_stage_is_skipped() {
        let stage = make_stage(
            EffectSet::new(vec![Effect::Llm {
                model: "any".into(),
            }]),
            vec![Example {
                input: json!(null),
                output: json!(null),
            }],
        );
        let report = verify_stage(&stage, &EchoExec);
        assert!(matches!(
            report.outcome,
            ReportOutcome::Skipped {
                reason: StageSkipReason::Llm
            }
        ));
    }

    #[test]
    fn canonical_eq_ignores_field_order_and_numeric_form() {
        assert!(canonical_eq(
            &json!({"a": 1, "b": 2}),
            &json!({"b": 2, "a": 1})
        ));
        assert!(canonical_eq(&json!(1.0), &json!(1)));
        assert!(!canonical_eq(&json!({"a": 1}), &json!({"a": 2})));
    }

    #[test]
    fn no_examples_is_skipped() {
        let stage = make_stage(EffectSet::pure(), vec![]);
        let report = verify_stage(&stage, &EchoExec);
        assert!(matches!(
            report.outcome,
            ReportOutcome::Skipped {
                reason: StageSkipReason::NoExamples
            }
        ));
    }

    // Silence the unused warning on the Capability import in certain test
    // configurations.
    #[allow(dead_code)]
    fn _capability_use() -> Capability {
        Capability::Network
    }
}
