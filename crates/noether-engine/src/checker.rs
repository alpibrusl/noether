use crate::lagrange::{CompositionNode, Pinning};
use noether_core::capability::Capability;
use noether_core::effects::{Effect, EffectKind, EffectSet};
use noether_core::stage::StageId;
use noether_core::types::{is_subtype_of, IncompatibilityReason, NType, TypeCompatibility};
use noether_store::StageStore;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// The resolved input/output types of a composition node.
#[derive(Debug, Clone)]
pub struct ResolvedType {
    pub input: NType,
    pub output: NType,
}

// ── Capability enforcement ─────────────────────────────────────────────────

/// Policy controlling which capabilities a composition is allowed to use.
///
/// `allowed` is empty → all capabilities permitted (default / backward-compatible).
/// `allowed` is non-empty → only the listed capabilities are permitted.
#[derive(Debug, Clone, Default)]
pub struct CapabilityPolicy {
    /// Capabilities the caller grants. Empty set = allow all.
    pub allowed: BTreeSet<Capability>,
}

impl CapabilityPolicy {
    /// A policy that allows every capability.
    pub fn allow_all() -> Self {
        Self {
            allowed: BTreeSet::new(),
        }
    }

    /// A policy that permits only the listed capabilities.
    pub fn restrict(caps: impl IntoIterator<Item = Capability>) -> Self {
        Self {
            allowed: caps.into_iter().collect(),
        }
    }

    fn is_allowed(&self, cap: &Capability) -> bool {
        self.allowed.is_empty() || self.allowed.contains(cap)
    }
}

/// A single capability violation found during pre-flight checking.
#[derive(Debug, Clone)]
pub struct CapabilityViolation {
    pub stage_id: StageId,
    pub required: Capability,
    pub message: String,
}

impl fmt::Display for CapabilityViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "stage {} requires capability {:?} which is not granted",
            self.stage_id.0, self.required
        )
    }
}

/// Pre-flight check: walk the graph and verify every stage's declared capabilities
/// are within the granted policy. Returns an empty vec when all capabilities pass.
pub fn check_capabilities(
    node: &CompositionNode,
    store: &(impl StageStore + ?Sized),
    policy: &CapabilityPolicy,
) -> Vec<CapabilityViolation> {
    let mut violations = Vec::new();
    collect_capability_violations(node, store, policy, &mut violations);
    violations
}

fn collect_capability_violations(
    node: &CompositionNode,
    store: &(impl StageStore + ?Sized),
    policy: &CapabilityPolicy,
    violations: &mut Vec<CapabilityViolation>,
) {
    match node {
        CompositionNode::Stage { id, .. } => {
            if let Ok(Some(stage)) = store.get(id) {
                for cap in &stage.capabilities {
                    if !policy.is_allowed(cap) {
                        violations.push(CapabilityViolation {
                            stage_id: id.clone(),
                            required: cap.clone(),
                            message: format!(
                                "stage '{}' requires {:?}; grant it with --allow-capabilities",
                                stage.description, cap
                            ),
                        });
                    }
                }
            }
        }
        CompositionNode::RemoteStage { .. } => {} // remote stages have no local capabilities
        CompositionNode::Const { .. } => {}       // no capabilities in a constant
        CompositionNode::Sequential { stages } => {
            for s in stages {
                collect_capability_violations(s, store, policy, violations);
            }
        }
        CompositionNode::Parallel { branches } => {
            for branch in branches.values() {
                collect_capability_violations(branch, store, policy, violations);
            }
        }
        CompositionNode::Branch {
            predicate,
            if_true,
            if_false,
        } => {
            collect_capability_violations(predicate, store, policy, violations);
            collect_capability_violations(if_true, store, policy, violations);
            collect_capability_violations(if_false, store, policy, violations);
        }
        CompositionNode::Fanout { source, targets } => {
            collect_capability_violations(source, store, policy, violations);
            for t in targets {
                collect_capability_violations(t, store, policy, violations);
            }
        }
        CompositionNode::Merge { sources, target } => {
            for s in sources {
                collect_capability_violations(s, store, policy, violations);
            }
            collect_capability_violations(target, store, policy, violations);
        }
        CompositionNode::Retry { stage, .. } => {
            collect_capability_violations(stage, store, policy, violations);
        }
        CompositionNode::Let { bindings, body } => {
            for b in bindings.values() {
                collect_capability_violations(b, store, policy, violations);
            }
            collect_capability_violations(body, store, policy, violations);
        }
    }
}

// ── Effect inference & enforcement ────────────────────────────────────────

/// Policy controlling which effect kinds a composition is allowed to declare.
///
/// `allowed` is empty → all effects permitted (default / backward-compatible).
/// `allowed` is non-empty → only the listed effect kinds are permitted; others
/// produce an [`EffectViolation`].
#[derive(Debug, Clone, Default)]
pub struct EffectPolicy {
    /// Effect kinds the caller grants. Empty set = allow all.
    pub allowed: BTreeSet<EffectKind>,
}

impl EffectPolicy {
    /// A policy that allows every effect (default).
    pub fn allow_all() -> Self {
        Self {
            allowed: BTreeSet::new(),
        }
    }

    /// A policy that permits only the listed effect kinds.
    pub fn restrict(kinds: impl IntoIterator<Item = EffectKind>) -> Self {
        Self {
            allowed: kinds.into_iter().collect(),
        }
    }

    pub fn is_allowed(&self, kind: &EffectKind) -> bool {
        self.allowed.is_empty() || self.allowed.contains(kind)
    }
}

/// A single effect violation found during pre-flight checking.
#[derive(Debug, Clone)]
pub struct EffectViolation {
    pub stage_id: StageId,
    pub effect: Effect,
    pub message: String,
}

impl fmt::Display for EffectViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

/// Walk the composition graph and return the union of all effects declared by
/// every stage. `RemoteStage` nodes always contribute `Effect::Network`.
/// Stages not found in the store contribute `Effect::Unknown`.
pub fn infer_effects(node: &CompositionNode, store: &(impl StageStore + ?Sized)) -> EffectSet {
    let mut effects: BTreeSet<Effect> = BTreeSet::new();
    collect_effects_inner(node, store, &mut effects);
    EffectSet::new(effects)
}

fn collect_effects_inner(
    node: &CompositionNode,
    store: &(impl StageStore + ?Sized),
    effects: &mut BTreeSet<Effect>,
) {
    match node {
        CompositionNode::Stage { id, .. } => match store.get(id) {
            Ok(Some(stage)) => {
                for e in stage.signature.effects.iter() {
                    effects.insert(e.clone());
                }
            }
            _ => {
                effects.insert(Effect::Unknown);
            }
        },
        CompositionNode::RemoteStage { .. } => {
            effects.insert(Effect::Network);
            effects.insert(Effect::Fallible);
        }
        CompositionNode::Const { .. } => {
            effects.insert(Effect::Pure);
        }
        CompositionNode::Sequential { stages } => {
            for s in stages {
                collect_effects_inner(s, store, effects);
            }
        }
        CompositionNode::Parallel { branches } => {
            for branch in branches.values() {
                collect_effects_inner(branch, store, effects);
            }
        }
        CompositionNode::Branch {
            predicate,
            if_true,
            if_false,
        } => {
            collect_effects_inner(predicate, store, effects);
            collect_effects_inner(if_true, store, effects);
            collect_effects_inner(if_false, store, effects);
        }
        CompositionNode::Fanout { source, targets } => {
            collect_effects_inner(source, store, effects);
            for t in targets {
                collect_effects_inner(t, store, effects);
            }
        }
        CompositionNode::Merge { sources, target } => {
            for s in sources {
                collect_effects_inner(s, store, effects);
            }
            collect_effects_inner(target, store, effects);
        }
        CompositionNode::Retry { stage, .. } => {
            collect_effects_inner(stage, store, effects);
        }
        CompositionNode::Let { bindings, body } => {
            for b in bindings.values() {
                collect_effects_inner(b, store, effects);
            }
            collect_effects_inner(body, store, effects);
        }
    }
}

/// Pre-flight check: walk the graph and verify every stage's declared effects
/// are within the granted policy. Returns an empty vec when all effects are allowed.
pub fn check_effects(
    node: &CompositionNode,
    store: &(impl StageStore + ?Sized),
    policy: &EffectPolicy,
) -> Vec<EffectViolation> {
    let mut violations = Vec::new();
    collect_effect_violations(node, store, policy, &mut violations);
    violations
}

fn collect_effect_violations(
    node: &CompositionNode,
    store: &(impl StageStore + ?Sized),
    policy: &EffectPolicy,
    violations: &mut Vec<EffectViolation>,
) {
    match node {
        CompositionNode::Stage { id, .. } => match store.get(id) {
            Ok(Some(stage)) => {
                for effect in stage.signature.effects.iter() {
                    let kind = effect.kind();
                    if !policy.is_allowed(&kind) {
                        violations.push(EffectViolation {
                            stage_id: id.clone(),
                            effect: effect.clone(),
                            message: format!(
                                "stage '{}' declares effect {kind}; grant it with --allow-effects {kind}",
                                stage.description
                            ),
                        });
                    }
                }
            }
            _ => {
                let kind = EffectKind::Unknown;
                if !policy.is_allowed(&kind) {
                    violations.push(EffectViolation {
                        stage_id: id.clone(),
                        effect: Effect::Unknown,
                        message: format!(
                            "stage {} has unknown effects (not in store); grant with --allow-effects unknown",
                            id.0
                        ),
                    });
                }
            }
        },
        CompositionNode::RemoteStage { .. } => {
            for effect in &[Effect::Network, Effect::Fallible] {
                let kind = effect.kind();
                if !policy.is_allowed(&kind) {
                    violations.push(EffectViolation {
                        stage_id: StageId("remote".into()),
                        effect: effect.clone(),
                        message: format!(
                            "RemoteStage declares implicit effect {kind}; grant with --allow-effects {kind}"
                        ),
                    });
                }
            }
        }
        CompositionNode::Const { .. } => {}
        CompositionNode::Sequential { stages } => {
            for s in stages {
                collect_effect_violations(s, store, policy, violations);
            }
        }
        CompositionNode::Parallel { branches } => {
            for branch in branches.values() {
                collect_effect_violations(branch, store, policy, violations);
            }
        }
        CompositionNode::Branch {
            predicate,
            if_true,
            if_false,
        } => {
            collect_effect_violations(predicate, store, policy, violations);
            collect_effect_violations(if_true, store, policy, violations);
            collect_effect_violations(if_false, store, policy, violations);
        }
        CompositionNode::Fanout { source, targets } => {
            collect_effect_violations(source, store, policy, violations);
            for t in targets {
                collect_effect_violations(t, store, policy, violations);
            }
        }
        CompositionNode::Merge { sources, target } => {
            for s in sources {
                collect_effect_violations(s, store, policy, violations);
            }
            collect_effect_violations(target, store, policy, violations);
        }
        CompositionNode::Retry { stage, .. } => {
            collect_effect_violations(stage, store, policy, violations);
        }
        CompositionNode::Let { bindings, body } => {
            for b in bindings.values() {
                collect_effect_violations(b, store, policy, violations);
            }
            collect_effect_violations(body, store, policy, violations);
        }
    }
}

// ── Signature verification ─────────────────────────────────────────────────

/// Why a stage's signature check failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignatureViolationKind {
    /// The stage has no `ed25519_signature` / `signer_public_key` — it was built unsigned.
    Missing,
    /// A signature is present but cryptographic verification failed (tampered stage).
    Invalid,
}

impl fmt::Display for SignatureViolationKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => write!(f, "unsigned"),
            Self::Invalid => write!(f, "invalid signature"),
        }
    }
}

/// A single signature violation found during pre-flight checking.
#[derive(Debug, Clone)]
pub struct SignatureViolation {
    pub stage_id: StageId,
    pub kind: SignatureViolationKind,
    pub message: String,
}

impl fmt::Display for SignatureViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "stage {} — {}", self.stage_id.0, self.message)
    }
}

/// Pre-flight check: walk the graph and verify every stage's Ed25519 signature.
///
/// Returns an empty vec when all signatures pass. Stages with a missing
/// signature OR an invalid signature are both reported as violations.
pub fn verify_signatures(
    node: &CompositionNode,
    store: &(impl StageStore + ?Sized),
) -> Vec<SignatureViolation> {
    let mut violations = Vec::new();
    collect_signature_violations(node, store, &mut violations);
    violations
}

fn collect_signature_violations(
    node: &CompositionNode,
    store: &(impl StageStore + ?Sized),
    violations: &mut Vec<SignatureViolation>,
) {
    match node {
        CompositionNode::Stage { id, .. } => {
            if let Ok(Some(stage)) = store.get(id) {
                match (&stage.ed25519_signature, &stage.signer_public_key) {
                    (None, _) | (_, None) => {
                        violations.push(SignatureViolation {
                            stage_id: id.clone(),
                            kind: SignatureViolationKind::Missing,
                            message: format!(
                                "stage '{}' has no signature — add it via the signing pipeline",
                                stage.description
                            ),
                        });
                    }
                    (Some(sig_hex), Some(pub_hex)) => {
                        match noether_core::stage::verify_stage_signature(id, sig_hex, pub_hex) {
                            Ok(true) => {} // valid
                            Ok(false) => {
                                violations.push(SignatureViolation {
                                    stage_id: id.clone(),
                                    kind: SignatureViolationKind::Invalid,
                                    message: format!(
                                        "stage '{}' signature verification failed — possible tampering",
                                        stage.description
                                    ),
                                });
                            }
                            Err(e) => {
                                violations.push(SignatureViolation {
                                    stage_id: id.clone(),
                                    kind: SignatureViolationKind::Invalid,
                                    message: format!(
                                        "stage '{}' signature could not be decoded: {e}",
                                        stage.description
                                    ),
                                });
                            }
                        }
                    }
                }
            }
            // If the stage is not in the store, the type-checker will already
            // have reported an unknown-stage error; skip here.
        }
        CompositionNode::Const { .. } => {} // constants have no signature to verify
        CompositionNode::RemoteStage { .. } => {} // remote stages have no local signature to verify
        CompositionNode::Sequential { stages } => {
            for s in stages {
                collect_signature_violations(s, store, violations);
            }
        }
        CompositionNode::Parallel { branches } => {
            for branch in branches.values() {
                collect_signature_violations(branch, store, violations);
            }
        }
        CompositionNode::Branch {
            predicate,
            if_true,
            if_false,
        } => {
            collect_signature_violations(predicate, store, violations);
            collect_signature_violations(if_true, store, violations);
            collect_signature_violations(if_false, store, violations);
        }
        CompositionNode::Fanout { source, targets } => {
            collect_signature_violations(source, store, violations);
            for t in targets {
                collect_signature_violations(t, store, violations);
            }
        }
        CompositionNode::Merge { sources, target } => {
            for s in sources {
                collect_signature_violations(s, store, violations);
            }
            collect_signature_violations(target, store, violations);
        }
        CompositionNode::Retry { stage, .. } => {
            collect_signature_violations(stage, store, violations);
        }
        CompositionNode::Let { bindings, body } => {
            for b in bindings.values() {
                collect_signature_violations(b, store, violations);
            }
            collect_signature_violations(body, store, violations);
        }
    }
}

// ── Effect warnings ────────────────────────────────────────────────────────

/// Warnings about effect usage detected during graph type-checking.
///
/// These are soft issues — the graph is structurally valid but may have
/// surprising runtime behaviour. Callers decide whether to block or surface them.
#[derive(Debug, Clone)]
pub enum EffectWarning {
    /// A `Fallible` stage is not wrapped in a `Retry` node. Failures propagate.
    FallibleWithoutRetry { stage_id: StageId },
    /// A `NonDeterministic` stage's output feeds a `Pure` stage.
    NonDeterministicFeedingPure { from: StageId, to: StageId },
    /// The sum of declared `Cost` effects exceeds the given budget (in cents).
    CostBudgetExceeded { total_cents: u64, budget_cents: u64 },
}

impl fmt::Display for EffectWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EffectWarning::FallibleWithoutRetry { stage_id } => write!(
                f,
                "stage {} is Fallible but has no Retry wrapper; failures will propagate",
                stage_id.0
            ),
            EffectWarning::NonDeterministicFeedingPure { from, to } => write!(
                f,
                "stage {} is NonDeterministic but feeds Pure stage {}; Pure caching will be bypassed",
                from.0, to.0
            ),
            EffectWarning::CostBudgetExceeded { total_cents, budget_cents } => write!(
                f,
                "estimated composition cost ({total_cents}¢) exceeds budget ({budget_cents}¢)"
            ),
        }
    }
}

/// The result of a successful graph type-check: resolved types plus any effect warnings.
#[derive(Debug, Clone)]
pub struct CheckResult {
    pub resolved: ResolvedType,
    pub warnings: Vec<EffectWarning>,
}

// ── Errors detected during graph type checking ────────────────────────────
#[derive(Debug, Clone)]
pub enum GraphTypeError {
    StageNotFound {
        id: StageId,
    },
    SequentialTypeMismatch {
        position: usize,
        from_output: NType,
        to_input: NType,
        reason: IncompatibilityReason,
    },
    BranchPredicateNotBool {
        actual: NType,
    },
    BranchOutputMismatch {
        true_output: NType,
        false_output: NType,
        reason: IncompatibilityReason,
    },
    FanoutInputMismatch {
        target_index: usize,
        source_output: NType,
        target_input: NType,
        reason: IncompatibilityReason,
    },
    MergeOutputMismatch {
        merged_type: NType,
        target_input: NType,
        reason: IncompatibilityReason,
    },
    EmptyNode {
        operator: String,
    },
}

impl fmt::Display for GraphTypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GraphTypeError::StageNotFound { id } => {
                write!(f, "stage {} not found in store", id.0)
            }
            GraphTypeError::SequentialTypeMismatch {
                position,
                from_output,
                to_input,
                reason,
            } => write!(
                f,
                "type mismatch at position {position}: output {from_output} is not subtype of input {to_input}: {reason}"
            ),
            GraphTypeError::BranchPredicateNotBool { actual } => {
                write!(f, "branch predicate must produce Bool, got {actual}")
            }
            GraphTypeError::BranchOutputMismatch {
                true_output,
                false_output,
                reason,
            } => write!(
                f,
                "branch outputs must be compatible: if_true produces {true_output}, if_false produces {false_output}: {reason}"
            ),
            GraphTypeError::FanoutInputMismatch {
                target_index,
                source_output,
                target_input,
                reason,
            } => write!(
                f,
                "fanout target {target_index}: source output {source_output} is not subtype of target input {target_input}: {reason}"
            ),
            GraphTypeError::MergeOutputMismatch {
                merged_type,
                target_input,
                reason,
            } => write!(
                f,
                "merge: merged type {merged_type} is not subtype of target input {target_input}: {reason}"
            ),
            GraphTypeError::EmptyNode { operator } => {
                write!(f, "empty {operator} node")
            }
        }
    }
}

/// Type-check a composition graph against the stage store.
///
/// Returns `CheckResult` (resolved types + effect warnings) on success,
/// or a list of hard type errors on failure.
pub fn check_graph(
    node: &CompositionNode,
    store: &(impl StageStore + ?Sized),
) -> Result<CheckResult, Vec<GraphTypeError>> {
    let mut errors = Vec::new();
    let result = check_node(node, store, &mut errors);
    if errors.is_empty() {
        let resolved = result.unwrap();
        let warnings = collect_effect_warnings(node, store, None);
        Ok(CheckResult { resolved, warnings })
    } else {
        Err(errors)
    }
}

/// Collect effect warnings by walking the graph.
/// `cost_budget_cents` — pass `Some(n)` to enable budget enforcement.
pub fn collect_effect_warnings(
    node: &CompositionNode,
    store: &(impl StageStore + ?Sized),
    cost_budget_cents: Option<u64>,
) -> Vec<EffectWarning> {
    let mut warnings = Vec::new();
    let mut total_cost: u64 = 0;
    collect_warnings_inner(node, store, &mut warnings, &mut total_cost, false);
    if let Some(budget) = cost_budget_cents {
        if total_cost > budget {
            warnings.push(EffectWarning::CostBudgetExceeded {
                total_cents: total_cost,
                budget_cents: budget,
            });
        }
    }
    warnings
}

fn collect_warnings_inner(
    node: &CompositionNode,
    store: &(impl StageStore + ?Sized),
    warnings: &mut Vec<EffectWarning>,
    total_cost: &mut u64,
    _parent_is_retry: bool,
) {
    match node {
        CompositionNode::Stage { id, .. } => {
            if let Ok(Some(stage)) = store.get(id) {
                // Accumulate cost
                for effect in stage.signature.effects.iter() {
                    if let Effect::Cost { cents } = effect {
                        *total_cost = total_cost.saturating_add(*cents);
                    }
                }
                // Fallible without retry is handled at the parent sequential level
            }
        }
        CompositionNode::RemoteStage { .. } => {} // remote calls have no local effects to warn about
        CompositionNode::Const { .. } => {}       // no effects in a constant
        CompositionNode::Sequential { stages } => {
            for (i, s) in stages.iter().enumerate() {
                collect_warnings_inner(s, store, warnings, total_cost, false);

                // Rule: Fallible stage not wrapped in Retry
                if let CompositionNode::Stage { id, .. } = s {
                    if let Ok(Some(stage)) = store.get(id) {
                        if stage.signature.effects.contains(&Effect::Fallible) {
                            warnings.push(EffectWarning::FallibleWithoutRetry {
                                stage_id: id.clone(),
                            });
                        }
                    }
                }

                // Rule: NonDeterministic output → Pure input
                if i + 1 < stages.len() {
                    if let (
                        CompositionNode::Stage { id: from_id, .. },
                        CompositionNode::Stage { id: to_id, .. },
                    ) = (s, &stages[i + 1])
                    {
                        let from_nd = store
                            .get(from_id)
                            .ok()
                            .flatten()
                            .map(|s| s.signature.effects.contains(&Effect::NonDeterministic))
                            .unwrap_or(false);
                        let to_pure = store
                            .get(to_id)
                            .ok()
                            .flatten()
                            .map(|s| s.signature.effects.contains(&Effect::Pure))
                            .unwrap_or(false);

                        if from_nd && to_pure {
                            warnings.push(EffectWarning::NonDeterministicFeedingPure {
                                from: from_id.clone(),
                                to: to_id.clone(),
                            });
                        }
                    }
                }
            }
        }
        CompositionNode::Parallel { branches } => {
            for branch in branches.values() {
                collect_warnings_inner(branch, store, warnings, total_cost, false);
            }
        }
        CompositionNode::Branch {
            predicate,
            if_true,
            if_false,
        } => {
            collect_warnings_inner(predicate, store, warnings, total_cost, false);
            collect_warnings_inner(if_true, store, warnings, total_cost, false);
            collect_warnings_inner(if_false, store, warnings, total_cost, false);
        }
        CompositionNode::Fanout { source, targets } => {
            collect_warnings_inner(source, store, warnings, total_cost, false);
            for t in targets {
                collect_warnings_inner(t, store, warnings, total_cost, false);
            }
        }
        CompositionNode::Merge { sources, target } => {
            for s in sources {
                collect_warnings_inner(s, store, warnings, total_cost, false);
            }
            collect_warnings_inner(target, store, warnings, total_cost, false);
        }
        CompositionNode::Retry { stage, .. } => {
            // Retry wraps Fallible — suppress FallibleWithoutRetry for direct child
            collect_warnings_inner(stage, store, warnings, total_cost, true);
            // Remove any FallibleWithoutRetry that was just added for the immediate child
            if let CompositionNode::Stage { id, .. } = stage.as_ref() {
                warnings.retain(|w| !matches!(w, EffectWarning::FallibleWithoutRetry { stage_id } if stage_id == id));
            }
        }
        CompositionNode::Let { bindings, body } => {
            for b in bindings.values() {
                collect_warnings_inner(b, store, warnings, total_cost, false);
            }
            collect_warnings_inner(body, store, warnings, total_cost, false);
        }
    }
}

fn check_node(
    node: &CompositionNode,
    store: &(impl StageStore + ?Sized),
    errors: &mut Vec<GraphTypeError>,
) -> Option<ResolvedType> {
    match node {
        CompositionNode::Stage {
            id,
            pinning,
            config,
        } => {
            let resolved = check_stage(id, *pinning, store, errors)?;
            // When config provides fields, reduce the effective input type
            if let Some(cfg) = config {
                if !cfg.is_empty() {
                    if let NType::Record(fields) = &resolved.input {
                        let remaining: std::collections::BTreeMap<String, NType> = fields
                            .iter()
                            .filter(|(name, _)| !cfg.contains_key(*name))
                            .map(|(name, ty)| (name.clone(), ty.clone()))
                            .collect();
                        let effective = if remaining.is_empty() || remaining.len() == 1 {
                            NType::Any
                        } else {
                            NType::Record(remaining)
                        };
                        return Some(ResolvedType {
                            input: effective,
                            output: resolved.output,
                        });
                    }
                }
            }
            Some(resolved)
        }
        // RemoteStage: types are declared inline — no store lookup needed.
        // The type checker trusts the declared input/output types.
        CompositionNode::RemoteStage { input, output, .. } => Some(ResolvedType {
            input: input.clone(),
            output: output.clone(),
        }),
        // Const: accepts Any input, emits Any output (actual type is inferred from value at runtime)
        CompositionNode::Const { .. } => Some(ResolvedType {
            input: NType::Any,
            output: NType::Any,
        }),
        CompositionNode::Sequential { stages } => check_sequential(stages, store, errors),
        CompositionNode::Parallel { branches } => check_parallel(branches, store, errors),
        CompositionNode::Branch {
            predicate,
            if_true,
            if_false,
        } => check_branch(predicate, if_true, if_false, store, errors),
        CompositionNode::Fanout { source, targets } => check_fanout(source, targets, store, errors),
        CompositionNode::Merge { sources, target } => check_merge(sources, target, store, errors),
        CompositionNode::Retry { stage, .. } => check_node(stage, store, errors),
        CompositionNode::Let { bindings, body } => check_let(bindings, body, store, errors),
    }
}

/// Type-check a `Let` node.
///
/// Each binding sees the **outer Let input**. The body sees an augmented
/// record `{ ...outer-input fields, <binding>: <binding-output> }`. The
/// Let's overall input requirement is the union of:
///   - every binding's input field requirements (each binding sees the same
///     outer input), and
///   - any field the body's input requires that is *not* satisfied by a
///     binding (those must come through from the outer input).
///
/// The Let's output is the body's output. When inputs are not Records (e.g.
/// `Any`), we conservatively widen to `NType::Any` rather than failing.
fn check_let(
    bindings: &BTreeMap<String, CompositionNode>,
    body: &CompositionNode,
    store: &(impl StageStore + ?Sized),
    errors: &mut Vec<GraphTypeError>,
) -> Option<ResolvedType> {
    if bindings.is_empty() {
        errors.push(GraphTypeError::EmptyNode {
            operator: "Let".into(),
        });
        return None;
    }

    // Resolve every binding's types.
    let mut binding_outputs: BTreeMap<String, NType> = BTreeMap::new();
    let mut required_input: BTreeMap<String, NType> = BTreeMap::new();
    let mut any_input = false;

    for (name, node) in bindings {
        let resolved = check_node(node, store, errors)?;
        binding_outputs.insert(name.clone(), resolved.output);
        match resolved.input {
            NType::Record(fields) => {
                for (f, ty) in fields {
                    required_input.insert(f, ty);
                }
            }
            NType::Any => {
                any_input = true;
            }
            other => {
                // A binding that wants a non-Record, non-Any input doesn't
                // compose cleanly with the Let's record-shaped input. We
                // conservatively require the outer input to be Any.
                let _ = other;
                any_input = true;
            }
        }
    }

    // Build the body's input record by merging outer-input requirements with
    // the binding outputs (bindings shadow outer fields with the same name).
    let mut body_input_fields = required_input.clone();
    for (name, out_ty) in &binding_outputs {
        body_input_fields.insert(name.clone(), out_ty.clone());
    }

    let body_resolved = check_node(body, store, errors)?;

    // Verify the body's input is satisfied by the augmented record. For each
    // field the body requires, either it must come from a binding output (in
    // which case the binding's output must be a subtype of the expected
    // field) or from the outer input — in which case we add it to the Let's
    // overall input requirement.
    if let NType::Record(body_fields) = &body_resolved.input {
        for (name, expected_ty) in body_fields {
            let provided = body_input_fields.get(name).cloned();
            match provided {
                Some(actual) => {
                    if let TypeCompatibility::Incompatible(reason) =
                        is_subtype_of(&actual, expected_ty)
                    {
                        errors.push(GraphTypeError::SequentialTypeMismatch {
                            position: 0,
                            from_output: actual,
                            to_input: expected_ty.clone(),
                            reason,
                        });
                    }
                }
                None => {
                    // Body needs a field neither bindings nor known outer
                    // requirements provide. Mark it as required from outer
                    // input.
                    required_input.insert(name.clone(), expected_ty.clone());
                }
            }
        }
    }

    let input = if any_input || required_input.is_empty() {
        NType::Any
    } else {
        NType::Record(required_input)
    };

    Some(ResolvedType {
        input,
        output: body_resolved.output,
    })
}

fn check_stage(
    id: &StageId,
    pinning: Pinning,
    store: &(impl StageStore + ?Sized),
    errors: &mut Vec<GraphTypeError>,
) -> Option<ResolvedType> {
    match crate::lagrange::resolve_stage_ref(id, pinning, store) {
        Some(stage) => Some(ResolvedType {
            input: stage.signature.input.clone(),
            output: stage.signature.output.clone(),
        }),
        None => {
            errors.push(GraphTypeError::StageNotFound { id: id.clone() });
            None
        }
    }
}

fn check_sequential(
    stages: &[CompositionNode],
    store: &(impl StageStore + ?Sized),
    errors: &mut Vec<GraphTypeError>,
) -> Option<ResolvedType> {
    if stages.is_empty() {
        errors.push(GraphTypeError::EmptyNode {
            operator: "Sequential".into(),
        });
        return None;
    }

    let resolved: Vec<Option<ResolvedType>> = stages
        .iter()
        .map(|s| check_node(s, store, errors))
        .collect();

    // Check consecutive pairs
    for i in 0..resolved.len() - 1 {
        if let (Some(from), Some(to)) = (&resolved[i], &resolved[i + 1]) {
            if let TypeCompatibility::Incompatible(reason) = is_subtype_of(&from.output, &to.input)
            {
                errors.push(GraphTypeError::SequentialTypeMismatch {
                    position: i,
                    from_output: from.output.clone(),
                    to_input: to.input.clone(),
                    reason,
                });
            }
        }
    }

    let first_input = resolved
        .first()
        .and_then(|r| r.as_ref())
        .map(|r| r.input.clone());
    let last_output = resolved
        .last()
        .and_then(|r| r.as_ref())
        .map(|r| r.output.clone());

    match (first_input, last_output) {
        (Some(input), Some(output)) => Some(ResolvedType { input, output }),
        _ => None,
    }
}

fn check_parallel(
    branches: &BTreeMap<String, CompositionNode>,
    store: &(impl StageStore + ?Sized),
    errors: &mut Vec<GraphTypeError>,
) -> Option<ResolvedType> {
    if branches.is_empty() {
        errors.push(GraphTypeError::EmptyNode {
            operator: "Parallel".into(),
        });
        return None;
    }

    let mut input_fields = BTreeMap::new();
    let mut output_fields = BTreeMap::new();

    for (name, node) in branches {
        if let Some(resolved) = check_node(node, store, errors) {
            input_fields.insert(name.clone(), resolved.input);
            output_fields.insert(name.clone(), resolved.output);
        }
    }

    if input_fields.len() == branches.len() {
        Some(ResolvedType {
            input: NType::Record(input_fields),
            output: NType::Record(output_fields),
        })
    } else {
        None
    }
}

fn check_branch(
    predicate: &CompositionNode,
    if_true: &CompositionNode,
    if_false: &CompositionNode,
    store: &(impl StageStore + ?Sized),
    errors: &mut Vec<GraphTypeError>,
) -> Option<ResolvedType> {
    let pred = check_node(predicate, store, errors);
    let true_branch = check_node(if_true, store, errors);
    let false_branch = check_node(if_false, store, errors);

    // Check predicate output is Bool
    if let Some(ref p) = pred {
        if let TypeCompatibility::Incompatible(_) = is_subtype_of(&p.output, &NType::Bool) {
            errors.push(GraphTypeError::BranchPredicateNotBool {
                actual: p.output.clone(),
            });
        }
    }

    // Branch outputs are unioned — both paths are valid return types.
    // No compatibility check required between branches; the consumer
    // of the branch output must handle the union type.
    match (pred, true_branch, false_branch) {
        (Some(p), Some(t), Some(f)) => Some(ResolvedType {
            input: p.input,
            output: NType::union(vec![t.output, f.output]),
        }),
        _ => None,
    }
}

fn check_fanout(
    source: &CompositionNode,
    targets: &[CompositionNode],
    store: &(impl StageStore + ?Sized),
    errors: &mut Vec<GraphTypeError>,
) -> Option<ResolvedType> {
    if targets.is_empty() {
        errors.push(GraphTypeError::EmptyNode {
            operator: "Fanout".into(),
        });
        return None;
    }

    let src = check_node(source, store, errors);
    let tgts: Vec<Option<ResolvedType>> = targets
        .iter()
        .map(|t| check_node(t, store, errors))
        .collect();

    // Check source output is subtype of each target input
    if let Some(ref s) = src {
        for (i, t) in tgts.iter().enumerate() {
            if let Some(ref t) = t {
                if let TypeCompatibility::Incompatible(reason) = is_subtype_of(&s.output, &t.input)
                {
                    errors.push(GraphTypeError::FanoutInputMismatch {
                        target_index: i,
                        source_output: s.output.clone(),
                        target_input: t.input.clone(),
                        reason,
                    });
                }
            }
        }
    }

    let output_types: Vec<NType> = tgts
        .iter()
        .filter_map(|t| t.as_ref().map(|r| r.output.clone()))
        .collect();

    match src {
        Some(s) if output_types.len() == targets.len() => Some(ResolvedType {
            input: s.input,
            output: NType::List(Box::new(if output_types.len() == 1 {
                output_types.into_iter().next().unwrap()
            } else {
                NType::union(output_types)
            })),
        }),
        _ => None,
    }
}

fn check_merge(
    sources: &[CompositionNode],
    target: &CompositionNode,
    store: &(impl StageStore + ?Sized),
    errors: &mut Vec<GraphTypeError>,
) -> Option<ResolvedType> {
    if sources.is_empty() {
        errors.push(GraphTypeError::EmptyNode {
            operator: "Merge".into(),
        });
        return None;
    }

    let srcs: Vec<Option<ResolvedType>> = sources
        .iter()
        .map(|s| check_node(s, store, errors))
        .collect();
    let tgt = check_node(target, store, errors);

    // Build merged output record from sources
    let mut merged_fields = BTreeMap::new();
    for (i, s) in srcs.iter().enumerate() {
        if let Some(ref r) = s {
            merged_fields.insert(format!("source_{i}"), r.output.clone());
        }
    }
    let merged_type = NType::Record(merged_fields);

    // Check merged type is subtype of target input
    if let Some(ref t) = tgt {
        if let TypeCompatibility::Incompatible(reason) = is_subtype_of(&merged_type, &t.input) {
            errors.push(GraphTypeError::MergeOutputMismatch {
                merged_type: merged_type.clone(),
                target_input: t.input.clone(),
                reason,
            });
        }
    }

    // Overall: input is record of source inputs, output is target output
    let mut input_fields = BTreeMap::new();
    for (i, s) in srcs.iter().enumerate() {
        if let Some(ref r) = s {
            input_fields.insert(format!("source_{i}"), r.input.clone());
        }
    }

    match tgt {
        Some(t) => Some(ResolvedType {
            input: NType::Record(input_fields),
            output: t.output,
        }),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noether_core::capability::Capability;
    use noether_core::effects::EffectSet;
    use noether_core::stage::{CostEstimate, Stage, StageSignature};
    use noether_store::MemoryStore;
    use std::collections::BTreeSet;

    fn make_stage(id: &str, input: NType, output: NType) -> Stage {
        Stage {
            id: StageId(id.into()),
            signature_id: None,
            signature: StageSignature {
                input,
                output,
                effects: EffectSet::pure(),
                implementation_hash: format!("impl_{id}"),
            },
            capabilities: BTreeSet::new(),
            cost: CostEstimate {
                time_ms_p50: Some(10),
                tokens_est: None,
                memory_mb: None,
            },
            description: format!("test stage {id}"),
            examples: vec![],
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

    fn test_store() -> MemoryStore {
        let mut store = MemoryStore::new();
        store
            .put(make_stage("text_to_num", NType::Text, NType::Number))
            .unwrap();
        store
            .put(make_stage("num_to_bool", NType::Number, NType::Bool))
            .unwrap();
        store
            .put(make_stage("text_to_text", NType::Text, NType::Text))
            .unwrap();
        store
            .put(make_stage("bool_pred", NType::Text, NType::Bool))
            .unwrap();
        store
            .put(make_stage("any_to_text", NType::Any, NType::Text))
            .unwrap();
        store
    }

    fn stage(id: &str) -> CompositionNode {
        CompositionNode::Stage {
            id: StageId(id.into()),
            pinning: Pinning::Signature,
            config: None,
        }
    }

    #[test]
    fn check_single_stage() {
        let store = test_store();
        let result = check_graph(&stage("text_to_num"), &store);
        let check = result.unwrap();
        assert_eq!(check.resolved.input, NType::Text);
        assert_eq!(check.resolved.output, NType::Number);
    }

    #[test]
    fn check_missing_stage() {
        let store = test_store();
        let result = check_graph(&stage("nonexistent"), &store);
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(matches!(errors[0], GraphTypeError::StageNotFound { .. }));
    }

    #[test]
    fn check_valid_sequential() {
        let store = test_store();
        let node = CompositionNode::Sequential {
            stages: vec![stage("text_to_num"), stage("num_to_bool")],
        };
        let result = check_graph(&node, &store);
        let check = result.unwrap();
        assert_eq!(check.resolved.input, NType::Text);
        assert_eq!(check.resolved.output, NType::Bool);
    }

    #[test]
    fn check_invalid_sequential() {
        let store = test_store();
        // Bool output cannot feed Text input
        let node = CompositionNode::Sequential {
            stages: vec![stage("num_to_bool"), stage("text_to_num")],
        };
        let result = check_graph(&node, &store);
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(matches!(
            errors[0],
            GraphTypeError::SequentialTypeMismatch { .. }
        ));
    }

    #[test]
    fn check_parallel() {
        let store = test_store();
        let node = CompositionNode::Parallel {
            branches: BTreeMap::from([
                ("nums".into(), stage("text_to_num")),
                ("bools".into(), stage("bool_pred")),
            ]),
        };
        let result = check_graph(&node, &store);
        let check = result.unwrap();
        // Input is Record { bools: Text, nums: Text }
        // Output is Record { bools: Bool, nums: Number }
        assert!(matches!(check.resolved.input, NType::Record(_)));
        assert!(matches!(check.resolved.output, NType::Record(_)));
    }

    #[test]
    fn check_branch_valid() {
        let store = test_store();
        let node = CompositionNode::Branch {
            predicate: Box::new(stage("bool_pred")),
            if_true: Box::new(stage("text_to_num")),
            if_false: Box::new(stage("text_to_text")),
        };
        // Predicate: Text -> Bool ✓
        // Both branches take Text, so input matches
        // Outputs are Number and Text, which union into Number | Text
        let result = check_graph(&node, &store);
        let check = result.unwrap();
        assert_eq!(check.resolved.input, NType::Text);
    }

    #[test]
    fn check_retry_transparent() {
        let store = test_store();
        let node = CompositionNode::Retry {
            stage: Box::new(stage("text_to_num")),
            max_attempts: 3,
            delay_ms: Some(100),
        };
        let result = check_graph(&node, &store);
        let check = result.unwrap();
        assert_eq!(check.resolved.input, NType::Text);
        assert_eq!(check.resolved.output, NType::Number);
    }

    #[test]
    fn capability_policy_allow_all_passes() {
        let mut store = test_store();
        let mut stage_net = make_stage("net_stage", NType::Text, NType::Text);
        stage_net.capabilities.insert(Capability::Network);
        store.put(stage_net).unwrap();

        let policy = CapabilityPolicy::allow_all();
        let violations = check_capabilities(&stage("net_stage"), &store, &policy);
        assert!(violations.is_empty());
    }

    #[test]
    fn capability_policy_restrict_blocks_network() {
        let mut store = test_store();
        let mut stage_net = make_stage("net_stage2", NType::Text, NType::Text);
        stage_net.capabilities.insert(Capability::Network);
        store.put(stage_net).unwrap();

        let policy = CapabilityPolicy::restrict([Capability::FsRead]);
        let violations = check_capabilities(&stage("net_stage2"), &store, &policy);
        assert_eq!(violations.len(), 1);
        assert!(matches!(violations[0].required, Capability::Network));
    }

    #[test]
    fn capability_policy_restrict_allows_declared() {
        let mut store = test_store();
        let mut stage_net = make_stage("net_stage3", NType::Text, NType::Text);
        stage_net.capabilities.insert(Capability::Network);
        store.put(stage_net).unwrap();

        let policy = CapabilityPolicy::restrict([Capability::Network]);
        let violations = check_capabilities(&stage("net_stage3"), &store, &policy);
        assert!(violations.is_empty());
    }

    #[test]
    fn remote_stage_resolves_declared_types() {
        let store = test_store();
        let node = CompositionNode::RemoteStage {
            url: "http://api.example.com".into(),
            input: NType::Text,
            output: NType::Number,
        };
        let result = check_graph(&node, &store).unwrap();
        assert_eq!(result.resolved.input, NType::Text);
        assert_eq!(result.resolved.output, NType::Number);
    }

    #[test]
    fn remote_stage_in_sequential_type_flows() {
        let mut store = test_store();
        store
            .put(make_stage("num_render", NType::Number, NType::Text))
            .unwrap();

        // Text -> RemoteStage(Text->Number) -> num_render(Number->Text) = Text->Text
        let node = CompositionNode::Sequential {
            stages: vec![
                CompositionNode::RemoteStage {
                    url: "http://api:8080".into(),
                    input: NType::Text,
                    output: NType::Number,
                },
                CompositionNode::Stage {
                    id: StageId("num_render".into()),
                    pinning: Pinning::Signature,
                    config: None,
                },
            ],
        };
        let result = check_graph(&node, &store).unwrap();
        assert_eq!(result.resolved.input, NType::Text);
        assert_eq!(result.resolved.output, NType::Text);
    }

    #[test]
    fn remote_stage_type_mismatch_is_detected() {
        let store = test_store();
        // RemoteStage outputs Number, but next stage expects Text
        let node = CompositionNode::Sequential {
            stages: vec![
                CompositionNode::RemoteStage {
                    url: "http://api:8080".into(),
                    input: NType::Text,
                    output: NType::Bool,
                },
                CompositionNode::Stage {
                    id: StageId("text_to_num".into()),
                    pinning: Pinning::Signature,
                    config: None,
                },
            ],
        };
        let result = check_graph(&node, &store);
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, GraphTypeError::SequentialTypeMismatch { .. })));
    }

    // ── Effect inference ────────────────────────────────────────────────────

    fn make_stage_with_effects(id: &str, effects: EffectSet) -> Stage {
        let mut s = make_stage(id, NType::Any, NType::Any);
        s.signature.effects = effects;
        s
    }

    #[test]
    fn infer_effects_pure_stage() {
        let mut store = MemoryStore::new();
        let stage = make_stage_with_effects("pure1", EffectSet::pure());
        store.put(stage.clone()).unwrap();
        let node = CompositionNode::Stage {
            id: StageId("pure1".into()),
            pinning: Pinning::Signature,
            config: None,
        };
        let effects = infer_effects(&node, &store);
        assert!(effects.contains(&Effect::Pure));
        assert!(!effects.contains(&Effect::Network));
    }

    #[test]
    fn infer_effects_union_sequential() {
        let mut store = MemoryStore::new();
        store
            .put(make_stage_with_effects("a", EffectSet::new([Effect::Pure])))
            .unwrap();
        store
            .put(make_stage_with_effects(
                "b",
                EffectSet::new([Effect::Network]),
            ))
            .unwrap();
        let node = CompositionNode::Sequential {
            stages: vec![
                CompositionNode::Stage {
                    id: StageId("a".into()),
                    pinning: Pinning::Signature,
                    config: None,
                },
                CompositionNode::Stage {
                    id: StageId("b".into()),
                    pinning: Pinning::Signature,
                    config: None,
                },
            ],
        };
        let effects = infer_effects(&node, &store);
        assert!(effects.contains(&Effect::Pure));
        assert!(effects.contains(&Effect::Network));
    }

    #[test]
    fn infer_effects_remote_stage_adds_network() {
        let store = MemoryStore::new();
        let node = CompositionNode::RemoteStage {
            url: "http://localhost:8080".into(),
            input: NType::Any,
            output: NType::Any,
        };
        let effects = infer_effects(&node, &store);
        assert!(effects.contains(&Effect::Network));
        assert!(effects.contains(&Effect::Fallible));
    }

    #[test]
    fn infer_effects_missing_stage_adds_unknown() {
        let store = MemoryStore::new();
        let node = CompositionNode::Stage {
            id: StageId("missing".into()),
            pinning: Pinning::Signature,
            config: None,
        };
        let effects = infer_effects(&node, &store);
        assert!(effects.contains(&Effect::Unknown));
    }

    // ── Effect policy ───────────────────────────────────────────────────────

    #[test]
    fn effect_policy_allow_all_never_violates() {
        let mut store = MemoryStore::new();
        store
            .put(make_stage_with_effects(
                "net",
                EffectSet::new([Effect::Network, Effect::Fallible]),
            ))
            .unwrap();
        let node = CompositionNode::Stage {
            id: StageId("net".into()),
            pinning: Pinning::Signature,
            config: None,
        };
        let policy = EffectPolicy::allow_all();
        assert!(check_effects(&node, &store, &policy).is_empty());
    }

    #[test]
    fn effect_policy_restrict_blocks_network() {
        let mut store = MemoryStore::new();
        store
            .put(make_stage_with_effects(
                "net",
                EffectSet::new([Effect::Network]),
            ))
            .unwrap();
        let node = CompositionNode::Stage {
            id: StageId("net".into()),
            pinning: Pinning::Signature,
            config: None,
        };
        let policy = EffectPolicy::restrict([EffectKind::Pure]);
        let violations = check_effects(&node, &store, &policy);
        assert!(!violations.is_empty());
        assert!(violations[0].message.contains("network"));
    }

    #[test]
    fn effect_policy_restrict_allows_matching_effect() {
        let mut store = MemoryStore::new();
        store
            .put(make_stage_with_effects(
                "llm",
                EffectSet::new([Effect::Llm {
                    model: "gpt-4o".into(),
                }]),
            ))
            .unwrap();
        let node = CompositionNode::Stage {
            id: StageId("llm".into()),
            pinning: Pinning::Signature,
            config: None,
        };
        let policy = EffectPolicy::restrict([EffectKind::Llm]);
        assert!(check_effects(&node, &store, &policy).is_empty());
    }
}
