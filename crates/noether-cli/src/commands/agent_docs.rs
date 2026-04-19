//! `noether agent-docs` — emit playbooks as ACLI-shaped JSON so AI
//! agents can query Noether documentation by intent keyword without
//! parsing prose.
//!
//! Playbooks are compiled into the binary via `include_str!`, so the
//! subcommand works offline and regardless of where the binary is
//! installed. When an agent wants the latest content, they can
//! always read `docs/agents/*.md` in a checked-out repo directly —
//! this command exists to make in-process lookup deterministic.

use crate::output::{acli_error, acli_ok};
use serde_json::json;

/// One playbook: a static (key, markdown body) pair. The markdown
/// content is the authoritative source; the parser below extracts a
/// title and intent blurb for the list/search endpoints.
struct Playbook {
    key: &'static str,
    body: &'static str,
}

const PLAYBOOKS: &[Playbook] = &[
    Playbook {
        key: "compose-a-graph",
        body: include_str!("../../../../docs/agents/compose-a-graph.md"),
    },
    Playbook {
        key: "find-an-existing-stage",
        body: include_str!("../../../../docs/agents/find-an-existing-stage.md"),
    },
    Playbook {
        key: "synthesize-a-new-stage",
        body: include_str!("../../../../docs/agents/synthesize-a-new-stage.md"),
    },
    Playbook {
        key: "express-a-property",
        body: include_str!("../../../../docs/agents/express-a-property.md"),
    },
    Playbook {
        key: "debug-a-failed-graph",
        body: include_str!("../../../../docs/agents/debug-a-failed-graph.md"),
    },
];

/// Dispatch: `noether agent-docs` → list; `noether agent-docs <key>`
/// → one playbook; `noether agent-docs --search <q>` → filtered list.
pub fn cmd_agent_docs(key: Option<&str>, search: Option<&str>) {
    if let Some(q) = search {
        let lower = q.to_ascii_lowercase();
        let hits: Vec<_> = PLAYBOOKS
            .iter()
            .filter(|p| p.body.to_ascii_lowercase().contains(&lower))
            .map(playbook_summary)
            .collect();
        println!(
            "{}",
            acli_ok(json!({
                "query": q,
                "hits": hits,
            }))
        );
        return;
    }

    match key {
        None => {
            // List available playbooks with a one-line intent.
            let list: Vec<_> = PLAYBOOKS.iter().map(playbook_summary).collect();
            println!(
                "{}",
                acli_ok(json!({
                    "playbooks": list,
                    "usage": "noether agent-docs <key>   # dump one playbook\n\
                              noether agent-docs --search <term>   # search by keyword",
                }))
            );
        }
        Some(k) => match PLAYBOOKS.iter().find(|p| p.key == k) {
            Some(p) => {
                println!(
                    "{}",
                    acli_ok(json!({
                        "key": p.key,
                        "title": extract_title(p.body),
                        "intent": extract_intent(p.body),
                        "body": p.body,
                    }))
                );
            }
            None => {
                let available: Vec<&str> = PLAYBOOKS.iter().map(|p| p.key).collect();
                eprintln!(
                    "{}",
                    acli_error(&format!(
                        "no playbook with key `{k}`. Available: {}",
                        available.join(", ")
                    ))
                );
                std::process::exit(2);
            }
        },
    }
}

fn playbook_summary(p: &Playbook) -> serde_json::Value {
    json!({
        "key": p.key,
        "title": extract_title(p.body),
        "intent": extract_intent(p.body),
    })
}

/// Pull the H1 from a playbook body (first `# ...` line). Falls back
/// to the key if malformed.
fn extract_title(body: &str) -> String {
    body.lines()
        .find_map(|l| l.strip_prefix("# ").map(|s| s.trim().to_string()))
        .unwrap_or_default()
}

/// Pull the "Intent" section body — the first paragraph after the
/// `## Intent` header. Playbooks follow a fixed shape so this parse
/// is deterministic.
fn extract_intent(body: &str) -> String {
    let mut lines = body.lines();
    // Find the "## Intent" header.
    let mut found_header = false;
    let mut buf = String::new();
    for line in lines.by_ref() {
        if !found_header {
            if line.trim().eq_ignore_ascii_case("## intent") {
                found_header = true;
            }
            continue;
        }
        // Everything until the next blank-separated header.
        let trimmed = line.trim();
        if trimmed.starts_with("## ") {
            break;
        }
        if trimmed.is_empty() && !buf.is_empty() {
            break;
        }
        if !trimmed.is_empty() {
            if !buf.is_empty() {
                buf.push(' ');
            }
            buf.push_str(trimmed);
        }
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_playbook_has_title_and_intent() {
        // Contract check for the playbook shape: each one must have
        // an H1 and an "## Intent" section. Prevents accidental
        // format drift that would silently produce empty summaries
        // in the list/search output.
        for p in PLAYBOOKS {
            assert!(
                !extract_title(p.body).is_empty(),
                "playbook {} has no H1 title",
                p.key
            );
            assert!(
                !extract_intent(p.body).is_empty(),
                "playbook {} has no '## Intent' section",
                p.key
            );
        }
    }

    #[test]
    fn playbook_keys_match_file_headers() {
        // Defence against renaming a playbook file without updating
        // the `key` field. The H1 is `# Playbook: <key>`.
        for p in PLAYBOOKS {
            let title = extract_title(p.body);
            let expected = format!("Playbook: {}", p.key);
            assert_eq!(
                title, expected,
                "playbook {} H1 should be `# {expected}`, got `# {title}`",
                p.key
            );
        }
    }

    #[test]
    fn extract_intent_handles_blank_lines() {
        let body = "# t\n\n## Intent\n\nfirst line.\nsecond line.\n\n## Preconditions\n\nignored\n";
        assert_eq!(
            extract_intent(body),
            "first line. second line.",
            "intent paragraph should terminate at the blank line before the next ## header"
        );
    }
}
