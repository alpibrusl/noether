//! Graph splitting — rewrite a Lagrange graph so its LLM-calling
//! stages dispatch to remote workers.
//!
//! For each `Stage { id }` whose stored effects include `Effect::Llm`,
//! pick a worker that advertises that model and rewrite the node in
//! place as `RemoteStage { url: "<worker>/stage/<id>", input, output }`.
//! Non-LLM stages are left alone — they'll execute on whichever machine
//! runs the rewritten graph (typically the broker itself).
//!
//! The rewritten graph is then handed to the standard
//! `noether_engine::executor::runner::run_composition` pipeline. The
//! `RemoteStage` executor in noether-engine already knows how to POST
//! `{"input": ...}` to a URL and read back `data.output` from the ACLI
//! envelope — the worker's `/stage/{id}` endpoint matches that contract.

use noether_core::effects::Effect;
use noether_core::stage::Stage;
use noether_engine::lagrange::CompositionNode;
use noether_grid_protocol::WorkerId;
use noether_store::{MemoryStore, StageStore};
use std::collections::BTreeMap;

use crate::router::RoutingRefusal;
use crate::state::WorkerEntry;

/// Result of splitting one graph: the rewritten AST plus the set of
/// workers it now depends on (for in-flight bookkeeping + failure
/// detection).
#[derive(Debug, Clone)]
pub struct SplitResult {
    pub rewritten: CompositionNode,
    /// Workers that received at least one rewritten `RemoteStage`. Used
    /// to increment in-flight counters before dispatch and decrement
    /// after.
    pub assigned_workers: Vec<WorkerId>,
}

/// Rewrite the graph so every Llm-effect stage dispatches to a worker.
///
/// `pick_worker` is called once per Llm-effect stage and returns the
/// chosen worker (or a refusal that aborts the whole rewrite). Tests
/// can pass a deterministic picker; the production caller wires it to
/// the routing module.
///
/// # Pre-condition — pinning must be resolved
///
/// `node` must already have been normalised via
/// [`noether_engine::lagrange::resolve_pinning`] so every `Stage`
/// reference holds a concrete implementation ID. The splitter looks
/// stages up via `store.get(id)`; signature-pinned references (where
/// `id` is a `SignatureId`) would miss and silently leave the node
/// unrewritten. Callers that invoke the splitter directly on a user-
/// authored graph should resolve pinning first.
pub fn split_graph<F>(
    node: &CompositionNode,
    stages: &MemoryStore,
    mut pick_worker: F,
) -> Result<SplitResult, RoutingRefusal>
where
    F: FnMut(&Stage) -> Result<(WorkerId, String), RoutingRefusal>,
{
    let mut assigned: Vec<WorkerId> = Vec::new();
    let rewritten = rewrite(node, stages, &mut pick_worker, &mut assigned)?;
    Ok(SplitResult {
        rewritten,
        assigned_workers: assigned,
    })
}

fn rewrite<F>(
    node: &CompositionNode,
    stages: &MemoryStore,
    pick: &mut F,
    assigned: &mut Vec<WorkerId>,
) -> Result<CompositionNode, RoutingRefusal>
where
    F: FnMut(&Stage) -> Result<(WorkerId, String), RoutingRefusal>,
{
    match node {
        CompositionNode::Stage { id, .. } => {
            let stage = match stages.get(id).ok().flatten() {
                Some(s) => s,
                // Stage isn't in the broker's catalogue — leave the
                // node alone. Either it'll work on the local executor
                // (unlikely) or fail clearly at runtime; we don't try
                // to be cleverer than the type checker here.
                None => return Ok(node.clone()),
            };
            if has_llm_effect(stage) {
                let (worker, worker_url) = pick(stage)?;
                if !assigned.contains(&worker) {
                    assigned.push(worker);
                }
                Ok(CompositionNode::RemoteStage {
                    url: format!("{}/stage/{}", worker_url.trim_end_matches('/'), id.0),
                    input: stage.signature.input.clone(),
                    output: stage.signature.output.clone(),
                })
            } else {
                Ok(node.clone())
            }
        }
        CompositionNode::Sequential { stages: children } => {
            let new_children = children
                .iter()
                .map(|c| rewrite(c, stages, pick, assigned))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(CompositionNode::Sequential {
                stages: new_children,
            })
        }
        CompositionNode::Parallel { branches } => {
            let mut new_branches = BTreeMap::new();
            for (k, v) in branches {
                new_branches.insert(k.clone(), rewrite(v, stages, pick, assigned)?);
            }
            Ok(CompositionNode::Parallel {
                branches: new_branches,
            })
        }
        CompositionNode::Branch {
            predicate,
            if_true,
            if_false,
        } => Ok(CompositionNode::Branch {
            predicate: Box::new(rewrite(predicate, stages, pick, assigned)?),
            if_true: Box::new(rewrite(if_true, stages, pick, assigned)?),
            if_false: Box::new(rewrite(if_false, stages, pick, assigned)?),
        }),
        CompositionNode::Fanout { source, targets } => {
            let new_targets = targets
                .iter()
                .map(|t| rewrite(t, stages, pick, assigned))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(CompositionNode::Fanout {
                source: Box::new(rewrite(source, stages, pick, assigned)?),
                targets: new_targets,
            })
        }
        CompositionNode::Merge { sources, target } => {
            let new_sources = sources
                .iter()
                .map(|s| rewrite(s, stages, pick, assigned))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(CompositionNode::Merge {
                sources: new_sources,
                target: Box::new(rewrite(target, stages, pick, assigned)?),
            })
        }
        CompositionNode::Retry {
            stage,
            max_attempts,
            delay_ms,
        } => Ok(CompositionNode::Retry {
            stage: Box::new(rewrite(stage, stages, pick, assigned)?),
            max_attempts: *max_attempts,
            delay_ms: *delay_ms,
        }),
        CompositionNode::Let { bindings, body } => {
            let mut new_bindings = BTreeMap::new();
            for (k, v) in bindings {
                new_bindings.insert(k.clone(), rewrite(v, stages, pick, assigned)?);
            }
            Ok(CompositionNode::Let {
                bindings: new_bindings,
                body: Box::new(rewrite(body, stages, pick, assigned)?),
            })
        }
        // RemoteStage and Const are pass-through — no stage to look up.
        CompositionNode::RemoteStage { .. } | CompositionNode::Const { .. } => Ok(node.clone()),
    }
}

fn has_llm_effect(stage: &Stage) -> bool {
    stage
        .signature
        .effects
        .iter()
        .any(|e| matches!(e, Effect::Llm { .. }))
}

/// Collect the LLM models a graph requires — used by the router to
/// match against worker capabilities before invoking `split_graph`.
pub fn required_llm_models(node: &CompositionNode, stages: &MemoryStore) -> Vec<String> {
    let mut models: Vec<String> = Vec::new();
    collect_models(node, stages, &mut models);
    models.sort();
    models.dedup();
    models
}

fn collect_models(node: &CompositionNode, stages: &MemoryStore, out: &mut Vec<String>) {
    match node {
        CompositionNode::Stage { id, .. } => {
            if let Ok(Some(stage)) = stages.get(id) {
                for effect in stage.signature.effects.iter() {
                    if let Effect::Llm { model } = effect {
                        out.push(model.clone());
                    }
                }
            }
        }
        CompositionNode::Sequential { stages: children } => {
            children.iter().for_each(|c| collect_models(c, stages, out));
        }
        CompositionNode::Parallel { branches } => {
            branches
                .values()
                .for_each(|v| collect_models(v, stages, out));
        }
        CompositionNode::Branch {
            predicate,
            if_true,
            if_false,
        } => {
            collect_models(predicate, stages, out);
            collect_models(if_true, stages, out);
            collect_models(if_false, stages, out);
        }
        CompositionNode::Fanout { source, targets } => {
            collect_models(source, stages, out);
            targets.iter().for_each(|t| collect_models(t, stages, out));
        }
        CompositionNode::Merge { sources, target } => {
            sources.iter().for_each(|s| collect_models(s, stages, out));
            collect_models(target, stages, out);
        }
        CompositionNode::Retry { stage, .. } => collect_models(stage, stages, out),
        CompositionNode::Let { bindings, body } => {
            bindings
                .values()
                .for_each(|v| collect_models(v, stages, out));
            collect_models(body, stages, out);
        }
        CompositionNode::RemoteStage { .. } | CompositionNode::Const { .. } => {}
    }
}

/// Decision callback for `split_graph` that picks a worker per stage
/// from a snapshot of the worker registry.
pub fn pick_worker_for(
    workers: &[WorkerEntry],
) -> impl FnMut(&Stage) -> Result<(WorkerId, String), RoutingRefusal> + '_ {
    let now = chrono::Utc::now();
    move |stage| {
        let model_needed: Option<String> = stage.signature.effects.iter().find_map(|e| match e {
            Effect::Llm { model } => Some(model.clone()),
            _ => None,
        });
        let needed = model_needed.as_deref().unwrap_or("");
        // Stages declaring a bare-string `"llm"` effect parse as
        // `Llm { model: "unknown" }`. Treat that (and empty) as "any
        // worker with any Llm capability" rather than requiring an
        // exact-model match — otherwise the happy-path pilot with
        // caloron's effect style routes zero stages.
        let any_llm = needed.is_empty() || needed == "unknown";

        let model_match = |c: &noether_grid_protocol::LlmCapability| -> bool {
            c.budget_remaining_cents > 0 && (any_llm || c.model == needed)
        };

        let candidate = workers
            .iter()
            .filter(|w| w.is_healthy(now))
            .filter(|w| w.advertisement.capabilities.iter().any(model_match))
            .max_by_key(|w| {
                w.advertisement
                    .capabilities
                    .iter()
                    .filter(|c| model_match(c))
                    .map(|c| c.budget_remaining_cents)
                    .sum::<u64>()
            });

        match candidate {
            Some(w) => Ok((
                w.advertisement.worker_id.clone(),
                w.advertisement.url.clone(),
            )),
            None => Err(RoutingRefusal::NoCapabilityMatch {
                needed: vec![needed.to_string()],
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noether_core::capability::Capability;
    use noether_core::effects::EffectSet;
    use noether_core::stage::{CostEstimate, Stage, StageId, StageLifecycle, StageSignature};
    use noether_core::types::NType;
    use std::collections::BTreeSet;

    fn make_stage(id: &str, llm_model: Option<&str>) -> Stage {
        let effects = if let Some(model) = llm_model {
            EffectSet::new(vec![Effect::Llm {
                model: model.into(),
            }])
        } else {
            EffectSet::pure()
        };
        Stage {
            id: StageId(id.into()),
            signature_id: None,
            signature: StageSignature {
                input: NType::Any,
                output: NType::Any,
                effects,
                implementation_hash: format!("hash-{id}"),
            },
            capabilities: BTreeSet::new(),
            cost: CostEstimate {
                time_ms_p50: None,
                tokens_est: None,
                memory_mb: None,
            },
            description: format!("test {id}"),
            examples: vec![],
            lifecycle: StageLifecycle::Active,
            ed25519_signature: None,
            signer_public_key: None,
            implementation_code: None,
            implementation_language: None,
            ui_style: None,
            tags: vec![],
            aliases: vec![],
            name: None,
            properties: Vec::new(),
        }
    }

    fn store_with(stages: Vec<Stage>) -> MemoryStore {
        let mut s = MemoryStore::new();
        for stage in stages {
            s.put(stage).unwrap();
        }
        s
    }

    #[test]
    fn pure_stage_passes_through() {
        let store = store_with(vec![make_stage("pure", None)]);
        let node = CompositionNode::Stage {
            id: StageId("pure".into()),
            pinning: noether_engine::lagrange::Pinning::Signature,
            config: None,
        };
        let out = split_graph(&node, &store, |_| {
            panic!("pure stage should not invoke pick")
        })
        .unwrap();
        assert_eq!(out.rewritten, node);
        assert!(out.assigned_workers.is_empty());
    }

    #[test]
    fn llm_stage_rewrites_to_remote_stage() {
        let store = store_with(vec![make_stage("call_llm", Some("claude-opus"))]);
        let node = CompositionNode::Stage {
            id: StageId("call_llm".into()),
            pinning: noether_engine::lagrange::Pinning::Signature,
            config: None,
        };
        let out = split_graph(&node, &store, |_| {
            Ok((WorkerId("alice".into()), "http://alice.corp:8089".into()))
        })
        .unwrap();
        assert_eq!(out.assigned_workers, vec![WorkerId("alice".into())]);
        match out.rewritten {
            CompositionNode::RemoteStage { url, .. } => {
                assert_eq!(url, "http://alice.corp:8089/stage/call_llm");
            }
            other => panic!("expected RemoteStage, got {other:?}"),
        }
    }

    #[test]
    fn mixed_graph_rewrites_only_llm_nodes() {
        let store = store_with(vec![
            make_stage("pure", None),
            make_stage("call_llm", Some("gpt-4")),
        ]);
        let seq = CompositionNode::Sequential {
            stages: vec![
                CompositionNode::Stage {
                    id: StageId("pure".into()),
                    pinning: noether_engine::lagrange::Pinning::Signature,
                    config: None,
                },
                CompositionNode::Stage {
                    id: StageId("call_llm".into()),
                    pinning: noether_engine::lagrange::Pinning::Signature,
                    config: None,
                },
            ],
        };
        let out = split_graph(&seq, &store, |_| {
            Ok((WorkerId("bob".into()), "http://bob.corp:8089".into()))
        })
        .unwrap();
        if let CompositionNode::Sequential { stages: children } = out.rewritten {
            assert_eq!(children.len(), 2);
            assert!(matches!(&children[0], CompositionNode::Stage { .. }));
            assert!(matches!(&children[1], CompositionNode::RemoteStage { .. }));
        } else {
            panic!("expected Sequential");
        }
    }

    #[test]
    fn stage_not_in_catalogue_passes_through() {
        let store = MemoryStore::new();
        let node = CompositionNode::Stage {
            id: StageId("ghost".into()),
            pinning: noether_engine::lagrange::Pinning::Signature,
            config: None,
        };
        let out = split_graph(&node, &store, |_| {
            panic!("unknown stage should not invoke pick")
        })
        .unwrap();
        assert_eq!(out.rewritten, node);
    }

    #[test]
    fn unknown_model_routes_to_any_llm_worker() {
        use noether_grid_protocol::{AuthVia, LlmCapability, WorkerAdvertisement};
        let store = store_with(vec![make_stage("bare_llm", Some("unknown"))]);
        let node = CompositionNode::Stage {
            id: StageId("bare_llm".into()),
            pinning: noether_engine::lagrange::Pinning::Signature,
            config: None,
        };
        let worker = crate::state::WorkerEntry {
            advertisement: WorkerAdvertisement {
                worker_id: WorkerId("alice".into()),
                url: "http://alice.corp:8089".into(),
                capabilities: vec![LlmCapability {
                    provider: "anthropic".into(),
                    model: "claude-opus".into(),
                    auth_via: AuthVia::Cli,
                    budget_monthly_cents: 2000,
                    budget_remaining_cents: 2000,
                    rate_limit_rpm: None,
                }],
                noether_version: "0.3.2".into(),
                heartbeat_interval_secs: 10,
            },
            last_seen: chrono::Utc::now(),
            in_flight_jobs: 0,
            draining: false,
        };
        let workers = vec![worker];
        let out = split_graph(&node, &store, pick_worker_for(&workers)).unwrap();
        assert_eq!(out.assigned_workers, vec![WorkerId("alice".into())]);
    }

    #[test]
    fn required_llm_models_dedups() {
        let store = store_with(vec![
            make_stage("a", Some("claude")),
            make_stage("b", Some("gpt-4")),
            make_stage("c", Some("claude")),
        ]);
        let seq = CompositionNode::Sequential {
            stages: vec![
                CompositionNode::Stage {
                    id: StageId("a".into()),
                    pinning: noether_engine::lagrange::Pinning::Signature,
                    config: None,
                },
                CompositionNode::Stage {
                    id: StageId("b".into()),
                    pinning: noether_engine::lagrange::Pinning::Signature,
                    config: None,
                },
                CompositionNode::Stage {
                    id: StageId("c".into()),
                    pinning: noether_engine::lagrange::Pinning::Signature,
                    config: None,
                },
            ],
        };
        let mut models = required_llm_models(&seq, &store);
        models.sort();
        assert_eq!(models, vec!["claude".to_string(), "gpt-4".to_string()]);
    }

    // Silence the unused import warning when the inner Capability path
    // is not exercised by these particular tests.
    #[allow(dead_code)]
    fn _capability_use() -> Capability {
        Capability::Network
    }
}
