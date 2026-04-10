use crate::index::SearchResult;
use noether_core::stage::Stage;
use noether_core::types::NType;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── Synthesis types ────────────────────────────────────────────────────────

/// Specification for a stage the Composition Agent wants synthesized.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SynthesisSpec {
    pub name: String,
    pub description: String,
    pub input: NType,
    pub output: NType,
    pub rationale: String,
}

/// Code + examples returned by the synthesis codegen LLM call.
#[derive(Debug, Clone, Deserialize)]
pub struct SynthesisResponse {
    pub examples: Vec<SynthesisExample>,
    pub implementation: String,
    #[serde(default = "default_language")]
    pub language: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SynthesisExample {
    pub input: Value,
    pub output: Value,
}

fn default_language() -> String {
    "python".into()
}

// ── Prompt builders ────────────────────────────────────────────────────────
/// Build the system prompt for the Composition Agent.
pub fn build_system_prompt(candidates: &[(&SearchResult, &Stage)]) -> String {
    let mut prompt = String::new();

    // --- Role ---
    prompt.push_str(
        "You are Noether's Composition Agent. You translate problem descriptions into \
         composition graphs in Lagrange JSON format.\n\n",
    );

    // --- Critical rules ---
    prompt.push_str("## CRITICAL RULES\n\n");
    prompt.push_str("1. ONLY use stage IDs from the AVAILABLE STAGES list. Never invent IDs.\n");
    prompt.push_str("2. Types MUST match: the output type of one stage must be a subtype of the next stage's input type.\n");
    prompt.push_str("3. Most stages take Record inputs with SPECIFIC FIELD NAMES. If a stage needs Record{items,key,...} but your pipeline produces a bare List, DO NOT try Parallel+Const wiring — SYNTHESIZE a stage instead.\n");
    prompt.push_str("4. Output ONLY a JSON code block — no explanation before or after.\n");
    prompt.push_str("5. EVERY node in the graph (including nested ones) MUST have an `\"op\"` field. There are NO exceptions.\n");
    prompt.push_str("   Valid values: `\"Stage\"`, `\"Const\"`, `\"Sequential\"`, `\"Parallel\"`, `\"Branch\"`, `\"Fanout\"`, `\"Retry\"`.\n");
    prompt.push_str("6. NEVER use a Stage branch in Parallel to \"pass through\" the input. Parallel branches receive the input but Stage branches transform it. Use Const for literal values only.\n\n");

    // --- Type system primer ---
    prompt.push_str("## Type System\n\n");
    prompt
        .push_str("- `Any` accepts any value. `Text`, `Number`, `Bool`, `Null` are primitives.\n");
    prompt.push_str("- `Record { field: Type }` is an object with named fields. The stage REQUIRES exactly those fields.\n");
    prompt.push_str("- `List<T>` is an array. `Map<K,V>` is a key-value object.\n");
    prompt.push_str("- `T | Null` means the field is optional (can be null).\n");
    prompt.push_str(
        "- Width subtyping: `{a, b, c}` is subtype of `{a, b}` — extra fields are OK.\n\n",
    );

    // --- Operators ---
    prompt.push_str("## Operators\n\n");
    prompt.push_str("- **Stage**: `{\"op\": \"Stage\", \"id\": \"<hash>\"}` — optionally add `\"config\": {\"key\": \"value\"}` to provide static parameters\n");
    prompt.push_str("- **Const**: `{\"op\": \"Const\", \"value\": <any JSON value>}` — emits a literal constant, ignores its input entirely\n");
    prompt.push_str("- **Sequential**: `{\"op\": \"Sequential\", \"stages\": [A, B, C]}` — output of A feeds B, then C\n");
    prompt.push_str("- **Parallel**: `{\"op\": \"Parallel\", \"branches\": {\"key1\": A, \"key2\": B}}` — ALL branches receive the SAME full input (or the field matching the branch name if the input is a Record); outputs are merged into a Record `{\"key1\": <out_A>, \"key2\": <out_B>}`\n");
    prompt.push_str("- **Branch**: `{\"op\": \"Branch\", \"predicate\": P, \"if_true\": A, \"if_false\": B}` — P receives the original input and MUST return Bool; A and B also receive the SAME original input (NOT the Bool)\n");
    prompt.push_str("- **Fanout**: `{\"op\": \"Fanout\", \"source\": A, \"targets\": [B, C]}`\n");
    prompt.push_str("- **Retry**: `{\"op\": \"Retry\", \"stage\": A, \"max_attempts\": 3, \"delay_ms\": 500}`\n\n");

    // --- Stage config: the key pattern for parameterized stages ---
    prompt.push_str("## Stage Config — VERY IMPORTANT\n\n");
    prompt.push_str(
        "Many stages need `Record { items: List, key: Text, descending: Bool }` as input.\n",
    );
    prompt.push_str("The pipeline only provides the DATA (e.g., a `List`). The PARAMETERS (`key`, `descending`) are static.\n\n");
    prompt.push_str("**Use `config` to supply static parameters.** The executor merges config fields with the pipeline input:\n\n");
    prompt.push_str("```json\n");
    prompt.push_str("{\n");
    prompt.push_str("  \"op\": \"Stage\",\n");
    prompt.push_str("  \"id\": \"<list_sort_id>\",\n");
    prompt.push_str("  \"config\": {\"key\": \"score\", \"descending\": true}\n");
    prompt.push_str("}\n");
    prompt.push_str("```\n\n");
    prompt.push_str("The pipeline flows `List<Any>` → the executor produces `{items: <the list>, key: \"score\", descending: true}` → `list_sort` receives exactly what it needs.\n\n");
    prompt.push_str("**Rules for config:**\n");
    prompt.push_str("- Use config for PARAMETER fields (key, count, delimiter, pattern, etc.)\n");
    prompt.push_str("- The pipeline provides the DATA field (items, text, data, records, etc.)\n");
    prompt.push_str("- Config keys must match the stage's Record field names exactly\n");
    prompt.push_str("- Config values are JSON literals (strings, numbers, booleans, null)\n\n");
    prompt.push_str("**Example: CSV parse → sort by revenue → take top 3 → serialize**\n");
    prompt.push_str("```json\n");
    prompt.push_str("{\n");
    prompt.push_str("  \"op\": \"Sequential\",\n");
    prompt.push_str("  \"stages\": [\n");
    prompt.push_str("    {\"op\": \"Stage\", \"id\": \"<csv_parse_id>\"},\n");
    prompt.push_str("    {\"op\": \"Stage\", \"id\": \"<list_sort_id>\", \"config\": {\"key\": \"revenue\", \"descending\": true}},\n");
    prompt.push_str(
        "    {\"op\": \"Stage\", \"id\": \"<list_take_id>\", \"config\": {\"count\": 3}},\n",
    );
    prompt.push_str("    {\"op\": \"Stage\", \"id\": \"<json_serialize_id>\"}\n");
    prompt.push_str("  ]\n");
    prompt.push_str("}\n");
    prompt.push_str("```\n\n");
    prompt.push_str("**Parallel** is still used for running branches concurrently on the same input — NOT for assembling Record parameters.\n\n");

    // --- Branch operator guidance ---
    prompt.push_str("## Branch Operator — How It Works\n\n");
    prompt.push_str("```\nBranch receives input X.\n");
    prompt.push_str("1. Runs predicate(X) → must return Bool\n");
    prompt.push_str("2. If true:  runs if_true(X)  — same X, NOT the Bool\n");
    prompt.push_str("3. If false: runs if_false(X) — same X, NOT the Bool\n```\n\n");
    prompt.push_str("Do NOT use Branch when you mean a stage that selects between values.\n");
    prompt.push_str(
        "Branch is for routing execution to different sub-graphs based on a condition.\n\n",
    );

    // --- Synthesis option ---
    prompt.push_str("## When to Synthesize a New Stage\n\n");
    prompt.push_str("**PREFER SYNTHESIS over complex composition** in these cases:\n\n");
    prompt.push_str("- The required primitive operation (e.g. modulo, even/odd, filter, sort-by-key) has no matching stage.\n");
    prompt.push_str("- Solving the problem would need 3+ stages of awkward Record manipulation.\n");
    prompt.push_str("- You need to filter a list, transform each element with custom logic, or reshape data in a bespoke way.\n");
    prompt.push_str(
        "- **You need to call a SPECIFIC external HTTP API** — always synthesize for API calls.\n",
    );
    prompt.push_str("  The `http_get` stdlib stage is for generic URL fetching; it cannot parse JSON, extract fields,\n");
    prompt.push_str("  or format results specific to a given API. Always synthesize a stage that does the full\n");
    prompt.push_str("  HTTP call + parse + reshape in one Python function.\n\n");
    prompt.push_str("**CRITICAL: A synthesis request is a STANDALONE top-level document.**\n");
    prompt.push_str(
        "It CANNOT be embedded inside a `Sequential.stages` list or any other graph node.\n",
    );
    prompt.push_str("You MUST choose ONE of these two responses per turn:\n");
    prompt.push_str("  Option A) A synthesis request (to register a missing stage), OR\n");
    prompt.push_str(
        "  Option B) A composition graph (using existing + already-registered stages).\n",
    );
    prompt.push_str(
        "If you return a synthesis request, the stage will be registered and you WILL get\n",
    );
    prompt
        .push_str("another turn to compose using that stage. Do NOT mix them in one response.\n\n");
    prompt
        .push_str("**Synthesis format (respond with ONLY this — no graph, no explanation):**\n\n");
    prompt.push_str("```json\n");
    prompt.push_str("{\n");
    prompt.push_str("  \"action\": \"synthesize\",\n");
    prompt.push_str("  \"spec\": {\n");
    prompt.push_str("    \"name\": \"snake_case_stage_name\",\n");
    prompt.push_str("    \"description\": \"One-sentence description of what this stage does\",\n");
    prompt.push_str("    \"input\": {\"kind\": \"Text\"},\n");
    prompt.push_str("    \"output\": {\"kind\": \"Number\"},\n");
    prompt.push_str("    \"rationale\": \"Why no available stage satisfies this\"\n");
    prompt.push_str("  }\n");
    prompt.push_str("}\n");
    prompt.push_str("```\n\n");
    prompt.push_str("NType JSON format:\n");
    prompt.push_str("- Primitives: `{\"kind\":\"Text\"}`, `{\"kind\":\"Number\"}`, `{\"kind\":\"Bool\"}`, `{\"kind\":\"Any\"}`, `{\"kind\":\"Null\"}`\n");
    prompt.push_str("- List: `{\"kind\":\"List\",\"value\":<T>}`\n");
    prompt.push_str("- Map: `{\"kind\":\"Map\",\"value\":{\"key\":{\"kind\":\"Text\"},\"value\":<T>}}` ← note: Map.value is an object with `key` and `value` fields\n");
    prompt.push_str("- Record: `{\"kind\":\"Record\",\"value\":{\"field_name\":<T>,...}}`\n");
    prompt.push_str("- Union: `{\"kind\":\"Union\",\"value\":[<T>,...]}`\n\n");
    prompt.push_str("**Keep synthesis types SIMPLE:**\n");
    prompt.push_str(
        "- Use `Any` for complex or heterogeneous output (lists of dicts, nested structures).\n",
    );
    prompt.push_str("- Use `Text` for input when it's raw data (CSV text, JSON string).\n");
    prompt.push_str("- Do NOT use `Map<Text, Any>` — use `Any` instead.\n");
    prompt.push_str(
        "- Prefer flat types: `Text → Any`, `Record{text: Text} → Any`, `Any → Text`.\n\n",
    );
    prompt.push_str("**Examples that SHOULD use synthesis:**\n");
    prompt.push_str(
        "- \"check if a number is even or odd\" → synthesize `is_even_or_odd` (Number → Text)\n",
    );
    prompt.push_str("- \"filter a list keeping items that match a pattern\" → synthesize `filter_by_pattern` (Record { items, pattern } → List)\n");
    prompt.push_str("- \"sort a list by a field\" → synthesize `sort_by_field` (Record { items, field } → List)\n");
    prompt.push_str("- \"sort a list and take the top N\" → synthesize `sort_and_take` (Record { items, n } → List)\n");
    prompt.push_str("- \"search npm packages and return results\" → synthesize `npm_search` (Record { query, limit } → List) — NEVER try to compose with http_get\n");
    prompt.push_str("- \"search GitHub repos\" → synthesize `github_search` — NEVER try to compose with http_get\n");
    prompt.push_str("- ANY call to a named external API (GitHub, npm, Hacker News, Spotify, etc.) → synthesize\n\n");

    // --- Few-shot examples using real IDs when available ---
    let parse_json_id = find_candidate_id(candidates, "Parse a JSON string");
    let to_json_id = find_candidate_id(candidates, "Serialize any value to a JSON");
    let is_null_id = find_candidate_id(candidates, "Check if a value is null");
    let text_upper_id = find_candidate_id(candidates, "Convert text to uppercase");
    let text_lower_id = find_candidate_id(candidates, "Convert text to lowercase");

    prompt.push_str("## EXAMPLE 1: Sequential composition\n\n");
    prompt.push_str("Problem: \"Parse a JSON string and serialize it back\"\n\n");
    prompt.push_str("The stage `parse_json` has input `Text` and output `Any`.\n");
    prompt.push_str("The stage `to_json` has input `Any` and output `Text`.\n");
    prompt.push_str("Since `Any` (output of parse_json) is subtype of `Any` (input of to_json), they compose.\n\n");
    prompt.push_str("```json\n");
    prompt.push_str("{\n");
    prompt.push_str("  \"description\": \"Parse JSON then serialize back to text\",\n");
    prompt.push_str("  \"version\": \"0.1.0\",\n");
    prompt.push_str("  \"root\": {\n");
    prompt.push_str("    \"op\": \"Sequential\",\n");
    prompt.push_str("    \"stages\": [\n");
    prompt.push_str(&format!(
        "      {{\"op\": \"Stage\", \"id\": \"{}\"}},\n",
        parse_json_id
    ));
    prompt.push_str(&format!(
        "      {{\"op\": \"Stage\", \"id\": \"{}\"}}\n",
        to_json_id
    ));
    prompt.push_str("    ]\n");
    prompt.push_str("  }\n");
    prompt.push_str("}\n");
    prompt.push_str("```\n\n");

    prompt.push_str("## EXAMPLE 2: Branch operator (condition-based routing)\n\n");
    prompt.push_str("Problem: \"Convert text to uppercase if it is not null, otherwise return empty string\"\n\n");
    prompt.push_str(
        "The `Branch` predicate receives the original `Text | Null` input and returns `Bool`.\n",
    );
    prompt.push_str("`if_true` and `if_false` ALSO receive the original input — NOT the Bool.\n\n");
    prompt.push_str("```json\n");
    prompt.push_str("{\n");
    prompt.push_str("  \"description\": \"Uppercase non-null text\",\n");
    prompt.push_str("  \"version\": \"0.1.0\",\n");
    prompt.push_str("  \"root\": {\n");
    prompt.push_str("    \"op\": \"Branch\",\n");
    prompt.push_str(&format!(
        "    \"predicate\": {{\"op\": \"Stage\", \"id\": \"{}\"}},\n",
        is_null_id
    ));
    prompt.push_str(&format!(
        "    \"if_true\": {{\"op\": \"Stage\", \"id\": \"{}\"}},\n",
        text_lower_id
    ));
    prompt.push_str(&format!(
        "    \"if_false\": {{\"op\": \"Stage\", \"id\": \"{}\"}}\n",
        text_upper_id
    ));
    prompt.push_str("  }\n");
    prompt.push_str("}\n");
    prompt.push_str("```\n\n");

    prompt.push_str("## EXAMPLE 3: Const + Parallel to assemble a multi-field Record\n\n");
    prompt.push_str(
        "Problem: \"Search for repos, then format a report with a fixed topic and summary\"\n\n",
    );
    prompt.push_str("The search stage returns a List. The format stage needs `Record{topic, results, summary}`.\n");
    prompt.push_str("Use Parallel: `results` branch runs the search (receives full input), `topic` and `summary` are Const literals.\n\n");
    prompt.push_str("```json\n");
    prompt.push_str("{\n");
    prompt.push_str("  \"description\": \"Search then format a report\",\n");
    prompt.push_str("  \"version\": \"0.1.0\",\n");
    prompt.push_str("  \"root\": {\n");
    prompt.push_str("    \"op\": \"Sequential\",\n");
    prompt.push_str("    \"stages\": [\n");
    prompt.push_str("      {\n");
    prompt.push_str("        \"op\": \"Parallel\",\n");
    prompt.push_str("        \"branches\": {\n");
    prompt.push_str("          \"results\": {\"op\": \"Stage\", \"id\": \"<search_stage_id>\"},\n");
    prompt.push_str("          \"topic\":   {\"op\": \"Const\", \"value\": \"async runtime\"},\n");
    prompt.push_str(
        "          \"summary\": {\"op\": \"Const\", \"value\": \"Top async runtime libraries\"}\n",
    );
    prompt.push_str("        }\n");
    prompt.push_str("      },\n");
    prompt.push_str("      {\"op\": \"Stage\", \"id\": \"<format_stage_id>\"}\n");
    prompt.push_str("    ]\n");
    prompt.push_str("  }\n");
    prompt.push_str("}\n");
    prompt.push_str("```\n\n");

    // --- Example 4: config-based composition ---
    let sort_id = find_candidate_id(candidates, "Sort a list");
    let take_id = find_candidate_id(candidates, "Take the first N elements");
    let json_ser_id = find_candidate_id(candidates, "Serialize any value to a JSON");

    prompt.push_str("## EXAMPLE 4: Using config for parameterized stages\n\n");
    prompt.push_str("Problem: \"Sort a list by score descending and take the top 3\"\n\n");
    prompt.push_str("The `list_sort` stage needs `Record{items, key, descending}` but the pipeline provides a bare `List`.\n");
    prompt.push_str("**Use config** to supply the parameter fields:\n\n");
    prompt.push_str("```json\n");
    prompt.push_str("{\n");
    prompt.push_str("  \"description\": \"Sort by score and take top 3\",\n");
    prompt.push_str("  \"version\": \"0.1.0\",\n");
    prompt.push_str("  \"root\": {\n");
    prompt.push_str("    \"op\": \"Sequential\",\n");
    prompt.push_str("    \"stages\": [\n");
    prompt.push_str(&format!(
        "      {{\"op\": \"Stage\", \"id\": \"{sort_id}\", \"config\": {{\"key\": \"score\", \"descending\": true}}}},\n"
    ));
    prompt.push_str(&format!(
        "      {{\"op\": \"Stage\", \"id\": \"{take_id}\", \"config\": {{\"count\": 3}}}},\n"
    ));
    prompt.push_str(&format!(
        "      {{\"op\": \"Stage\", \"id\": \"{json_ser_id}\"}}\n"
    ));
    prompt.push_str("    ]\n");
    prompt.push_str("  }\n");
    prompt.push_str("}\n");
    prompt.push_str("```\n\n");
    prompt.push_str("The executor merges config with pipeline data automatically. No Parallel+Const needed.\n\n");
    prompt.push_str("**When to synthesize instead:** when the operation has complex custom logic (API calls, data transformations that no existing stage covers).\n\n");

    // --- Available stages with examples, ordered by relevance score ---
    prompt.push_str("## Available Stages\n\n");
    prompt.push_str("Stages are listed by relevance to your problem (highest first).\n\n");

    for (result, stage) in candidates {
        prompt.push_str(&format!(
            "### `{}` — {} _(relevance: {:.2})_\n",
            stage.id.0, stage.description, result.score
        ));
        prompt.push_str(&format!(
            "- **Input**: `{}`\n- **Output**: `{}`\n",
            stage.signature.input, stage.signature.output,
        ));

        // Show first 2 examples with concrete data
        for ex in stage.examples.iter().take(2) {
            let input_str = serde_json::to_string(&ex.input).unwrap_or_default();
            let output_str = serde_json::to_string(&ex.output).unwrap_or_default();
            prompt.push_str(&format!("- Example: `{input_str}` → `{output_str}`\n"));
        }
        prompt.push('\n');
    }

    // --- Output format ---
    prompt.push_str("## Your Response\n\n");
    prompt.push_str("Respond with ONLY this JSON (no other text):\n");
    prompt.push_str("```json\n");
    prompt.push_str("{\n");
    prompt.push_str("  \"description\": \"<what this composition does>\",\n");
    prompt.push_str("  \"version\": \"0.1.0\",\n");
    prompt.push_str("  \"root\": { <composition using operators above> }\n");
    prompt.push_str("}\n");
    prompt.push_str("```\n");

    prompt
}

/// Search `candidates` for a stage whose description contains `needle`
/// and return its ID. Falls back to `<needle>` as a labelled placeholder
/// so the few-shot example is always syntactically valid JSON.
fn find_candidate_id(candidates: &[(&SearchResult, &Stage)], needle: &str) -> String {
    candidates
        .iter()
        .find(|(_, s)| s.description.contains(needle))
        .map(|(_, s)| s.id.0.clone())
        .unwrap_or_else(|| format!("<{needle}>"))
}

/// Build the effect inference prompt.
///
/// Given the implementation code, asks the LLM which Noether effects the code has.
/// Expected LLM response: a JSON array of effect names, e.g. `["Network", "Fallible"]`.
pub fn build_effect_inference_prompt(code: &str, language: &str) -> String {
    let mut p = String::new();
    p.push_str("You are analyzing code to determine its computational effects for the Noether platform.\n\n");
    p.push_str("## Noether Effect Types\n\n");
    p.push_str("- **Pure**: No side effects. Same inputs always produce same outputs. No I/O, no randomness.\n");
    p.push_str("- **Fallible**: The operation may fail or raise an exception.\n");
    p.push_str("- **Network**: Makes HTTP/TCP/DNS requests or any network I/O.\n");
    p.push_str("- **NonDeterministic**: Output may vary even with identical inputs (random, timestamp, etc.).\n");
    p.push_str("- **Llm**: Calls an LLM or AI model API.\n");
    p.push_str("- **Unknown**: Cannot determine effects from code inspection.\n\n");

    p.push_str(&format!("## Code to Analyze ({language})\n\n"));
    p.push_str("```\n");
    p.push_str(code);
    p.push_str("\n```\n\n");

    p.push_str("## Task\n\n");
    p.push_str("List ONLY the effects that apply to this code. If the code has no side effects and is deterministic, return `[\"Pure\"]`.\n\n");
    p.push_str("Rules:\n");
    p.push_str("- Pure and NonDeterministic are mutually exclusive (non-deterministic implies NOT Pure).\n");
    p.push_str(
        "- If the code imports urllib, requests, httpx, aiohttp, or any HTTP library → Network.\n",
    );
    p.push_str("- If the code has try/except or can raise → Fallible.\n");
    p.push_str("- If you cannot determine the effects → Unknown (not Pure).\n\n");

    p.push_str("## Response Format\n\n");
    p.push_str("Respond with ONLY a JSON array of effect names (no other text):\n");
    p.push_str("```json\n");
    p.push_str("[\"Effect1\", \"Effect2\"]\n");
    p.push_str("```\n");
    p
}

/// Parse an effect inference response from the LLM into an `EffectSet`.
///
/// Accepts `["Pure"]`, `["Network", "Fallible"]`, etc.
/// Falls back to `EffectSet::unknown()` on any parse error.
pub fn extract_effect_response(response: &str) -> noether_core::effects::EffectSet {
    use noether_core::effects::{Effect, EffectSet};

    let json_str = match extract_json_array(response) {
        Some(s) => s,
        None => return EffectSet::unknown(),
    };

    let names: Vec<String> = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return EffectSet::unknown(),
    };

    let effects: Vec<Effect> = names
        .iter()
        .filter_map(|name| match name.as_str() {
            "Pure" => Some(Effect::Pure),
            "Fallible" => Some(Effect::Fallible),
            "Network" => Some(Effect::Network),
            "NonDeterministic" => Some(Effect::NonDeterministic),
            "Llm" => Some(Effect::Llm {
                model: "unknown".into(),
            }),
            "Unknown" => Some(Effect::Unknown),
            _ => None,
        })
        .collect();

    if effects.is_empty() {
        EffectSet::unknown()
    } else {
        EffectSet::new(effects)
    }
}

/// Extract the first JSON array `[...]` from a response string.
fn extract_json_array(response: &str) -> Option<&str> {
    // Prefer ```json ... ``` fenced block
    if let Some(start) = response.find("```json") {
        let content = &response[start + 7..];
        if let Some(end) = content.find("```") {
            return Some(content[..end].trim());
        }
    }
    // Plain ``` ... ``` fenced block
    if let Some(start) = response.find("```") {
        let content = &response[start + 3..];
        if let Some(end) = content.find("```") {
            let candidate = content[..end].trim();
            if candidate.starts_with('[') {
                return Some(candidate);
            }
        }
    }
    // Raw array anywhere
    if let Some(start) = response.find('[') {
        let bytes = response.as_bytes();
        let mut depth: i32 = 0;
        let mut in_string = false;
        let mut escape = false;
        for (i, &b) in bytes[start..].iter().enumerate() {
            if escape {
                escape = false;
                continue;
            }
            if in_string {
                match b {
                    b'\\' => escape = true,
                    b'"' => in_string = false,
                    _ => {}
                }
                continue;
            }
            match b {
                b'"' => in_string = true,
                b'[' => depth += 1,
                b']' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(response[start..start + i + 1].trim());
                    }
                }
                _ => {}
            }
        }
    }
    None
}

/// Build the codegen prompt that asks the LLM to implement a synthesized stage.
pub fn build_synthesis_prompt(spec: &SynthesisSpec) -> String {
    let mut p = String::new();
    p.push_str(
        "You are generating a stage implementation for the Noether composition platform.\n\n",
    );
    p.push_str("## Stage Specification\n\n");
    p.push_str(&format!("- **Name**: `{}`\n", spec.name));
    p.push_str(&format!("- **Description**: {}\n", spec.description));
    p.push_str(&format!("- **Input type**: `{}`\n", spec.input));
    p.push_str(&format!("- **Output type**: `{}`\n\n", spec.output));

    p.push_str("## Your Task\n\n");
    p.push_str(
        "1. Produce at least 3 concrete input/output example pairs matching the type signature.\n",
    );
    p.push_str("2. Write a Python function `execute(input_value)` that implements this stage.\n");
    p.push_str(
        "   `input_value` is a Python dict/str/number/list/bool/None matching the input type.\n",
    );
    p.push_str("   Return a value matching the output type.\n\n");
    p.push_str("## Python Implementation Rules\n\n");
    p.push_str("- **Prefer Python stdlib over third-party packages** when possible.\n");
    p.push_str(
        "  - For HTTP: use `urllib.request` / `urllib.parse` (always available), NOT `requests`.\n",
    );
    p.push_str("  - For JSON: use `json` (always available).\n");
    p.push_str("  - For dates: use `datetime` (always available).\n");
    p.push_str("  - For regex: use `re` (always available).\n");
    p.push_str("- Only use third-party packages (`requests`, `pandas`, etc.) when there is no stdlib alternative.\n");
    p.push_str(
        "- **CRITICAL**: ALL imports MUST be placed at the top of the `execute` function body,\n",
    );
    p.push_str(
        "  BEFORE any use of those modules. Never use a module without importing it first.\n\n",
    );
    p.push_str("## Correct HTTP Implementation Pattern\n\n");
    p.push_str("```python\n");
    p.push_str("def execute(input_value):\n");
    p.push_str("    # ALWAYS import at the top of execute\n");
    p.push_str("    import urllib.request, urllib.parse, json\n");
    p.push_str("    url = 'https://api.example.com/search?' + urllib.parse.urlencode({'q': input_value['query']})\n");
    p.push_str("    with urllib.request.urlopen(url) as resp:\n");
    p.push_str("        data = json.loads(resp.read().decode())\n");
    p.push_str("    return data['items']\n");
    p.push_str("```\n\n");

    p.push_str("## Output Format\n\n");
    p.push_str("Respond with ONLY this JSON (no other text):\n");
    p.push_str("```json\n");
    p.push_str("{\n");
    p.push_str("  \"examples\": [\n");
    p.push_str("    {\"input\": <value>, \"output\": <value>},\n");
    p.push_str("    {\"input\": <value>, \"output\": <value>},\n");
    p.push_str("    {\"input\": <value>, \"output\": <value>}\n");
    p.push_str("  ],\n");
    p.push_str("  \"implementation\": \"def execute(input_value):\\n    ...\",\n");
    p.push_str("  \"language\": \"python\"\n");
    p.push_str("}\n");
    p.push_str("```\n");
    p
}

/// Try to parse a synthesis request from the LLM response.
/// Returns `Some(SynthesisSpec)` only when the JSON contains `"action": "synthesize"`.
pub fn extract_synthesis_spec(response: &str) -> Option<SynthesisSpec> {
    let json_str = extract_json(response)?;
    let v: serde_json::Value = serde_json::from_str(json_str).ok()?;
    if v.get("action").and_then(|a| a.as_str()) != Some("synthesize") {
        return None;
    }
    let spec = v.get("spec")?;
    serde_json::from_value(spec.clone()).ok()
}

/// Try to parse a synthesis response (examples + implementation) from the LLM.
pub fn extract_synthesis_response(response: &str) -> Option<SynthesisResponse> {
    let json_str = extract_json(response)?;
    serde_json::from_str(json_str).ok()
}

pub fn extract_json(response: &str) -> Option<&str> {
    // 1. Prefer ```json ... ``` fenced block
    if let Some(start) = response.find("```json") {
        let json_start = start + 7;
        let json_content = &response[json_start..];
        if let Some(end) = json_content.find("```") {
            return Some(json_content[..end].trim());
        }
    }

    // 2. Plain ``` ... ``` fenced block (skip language tag on first line if any)
    if let Some(start) = response.find("```") {
        let content_start = start + 3;
        let content = &response[content_start..];
        // Skip a non-brace first line (e.g. a language tag like "json" without the marker)
        let (skip, rest) = match content.find('\n') {
            Some(nl) => {
                let first_line = content[..nl].trim();
                if first_line.starts_with('{') {
                    (0, content)
                } else {
                    (nl + 1, &content[nl + 1..])
                }
            }
            None => (0, content),
        };
        let _ = skip;
        if let Some(end) = rest.find("```") {
            let candidate = rest[..end].trim();
            if candidate.starts_with('{') {
                return Some(candidate);
            }
        }
    }

    // 3. Raw JSON anywhere in the response: scan for the first top-level { ... } span
    // using brace depth counting (handles nested objects correctly).
    if let Some(brace_start) = response.find('{') {
        let bytes = response.as_bytes();
        let mut depth: i32 = 0;
        let mut in_string = false;
        let mut escape = false;
        let mut brace_end: Option<usize> = None;

        for (i, &b) in bytes[brace_start..].iter().enumerate() {
            if escape {
                escape = false;
                continue;
            }
            if in_string {
                match b {
                    b'\\' => escape = true,
                    b'"' => in_string = false,
                    _ => {}
                }
                continue;
            }
            match b {
                b'"' => in_string = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        brace_end = Some(brace_start + i + 1);
                        break;
                    }
                }
                _ => {}
            }
        }

        if let Some(end) = brace_end {
            let candidate = response[brace_start..end].trim();
            if !candidate.is_empty() {
                return Some(candidate);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::SearchResult;
    use noether_core::stage::StageId;

    fn make_search_result(id: &str, score: f32) -> SearchResult {
        SearchResult {
            stage_id: StageId(id.into()),
            score,
            signature_score: score,
            semantic_score: score,
            example_score: score,
        }
    }

    #[test]
    fn extract_json_from_code_block() {
        let response = "Here's the graph:\n```json\n{\"test\": true}\n```\nDone.";
        assert_eq!(extract_json(response), Some("{\"test\": true}"));
    }

    #[test]
    fn extract_json_from_plain_block() {
        let response = "```\n{\"test\": true}\n```";
        assert_eq!(extract_json(response), Some("{\"test\": true}"));
    }

    #[test]
    fn extract_raw_json() {
        let response = "{\"test\": true}";
        assert_eq!(extract_json(response), Some("{\"test\": true}"));
    }

    #[test]
    fn extract_json_none_for_text() {
        let response = "No JSON here, just text.";
        assert_eq!(extract_json(response), None);
    }

    #[test]
    fn extract_json_with_whitespace() {
        let response = "  \n```json\n  {\"a\": 1}  \n```\n  ";
        assert_eq!(extract_json(response), Some("{\"a\": 1}"));
    }

    #[test]
    fn extract_synthesis_spec_parses_valid_request() {
        let input_json = serde_json::to_string(&NType::Text).unwrap();
        let output_json = serde_json::to_string(&NType::Number).unwrap();
        let response = format!(
            "```json\n{}\n```",
            serde_json::json!({
                "action": "synthesize",
                "spec": {
                    "name": "count_words",
                    "description": "Count the number of words in a text",
                    "input": serde_json::from_str::<serde_json::Value>(&input_json).unwrap(),
                    "output": serde_json::from_str::<serde_json::Value>(&output_json).unwrap(),
                    "rationale": "No existing stage counts words"
                }
            })
        );
        let spec = extract_synthesis_spec(&response).unwrap();
        assert_eq!(spec.name, "count_words");
        assert_eq!(spec.input, NType::Text);
        assert_eq!(spec.output, NType::Number);
    }

    #[test]
    fn extract_synthesis_spec_returns_none_for_composition_graph() {
        let response = "```json\n{\"description\":\"test\",\"version\":\"0.1.0\",\"root\":{\"op\":\"Stage\",\"id\":\"abc\"}}\n```";
        assert!(extract_synthesis_spec(response).is_none());
    }

    #[test]
    fn extract_synthesis_response_parses_examples_and_code() {
        let response = "```json\n{\"examples\":[{\"input\":\"hello world\",\"output\":2},{\"input\":\"foo\",\"output\":1}],\"implementation\":\"def execute(v): return len(v.split())\",\"language\":\"python\"}\n```";
        let resp = extract_synthesis_response(response).unwrap();
        assert_eq!(resp.examples.len(), 2);
        assert_eq!(resp.language, "python");
        assert!(resp.implementation.contains("execute"));
    }

    #[test]
    fn build_synthesis_prompt_contains_spec_fields() {
        let spec = SynthesisSpec {
            name: "reverse_text".into(),
            description: "Reverse a string".into(),
            input: NType::Text,
            output: NType::Text,
            rationale: "no existing stage reverses text".into(),
        };
        let prompt = build_synthesis_prompt(&spec);
        assert!(prompt.contains("reverse_text"));
        assert!(prompt.contains("Reverse a string"));
        assert!(prompt.contains("execute(input_value)"));
    }

    #[test]
    fn few_shot_uses_real_ids_when_candidates_present() {
        use noether_core::stdlib::load_stdlib;

        let stages = load_stdlib();
        let parse_json = stages
            .iter()
            .find(|s| s.description.contains("Parse a JSON string"))
            .unwrap();
        let to_json = stages
            .iter()
            .find(|s| s.description.contains("Serialize any value to a JSON"))
            .unwrap();

        let r1 = make_search_result(&parse_json.id.0, 0.9);
        let r2 = make_search_result(&to_json.id.0, 0.8);
        let candidates: Vec<(&SearchResult, &Stage)> = vec![(&r1, parse_json), (&r2, to_json)];

        let prompt = build_system_prompt(&candidates);

        // The few-shot example must contain the real hashes, not placeholders.
        assert!(
            prompt.contains(&parse_json.id.0),
            "prompt should contain real parse_json hash"
        );
        assert!(
            prompt.contains(&to_json.id.0),
            "prompt should contain real to_json hash"
        );
    }

    #[test]
    fn few_shot_falls_back_to_placeholder_when_stages_absent() {
        let prompt = build_system_prompt(&[]);
        // With no candidates the fallback label appears (angle-bracket wrapped needle text).
        assert!(
            prompt.contains("<Parse a JSON string>"),
            "expected placeholder when parse_json not in candidates"
        );
    }

    #[test]
    fn prompt_contains_branch_guidance() {
        let prompt = build_system_prompt(&[]);
        assert!(
            prompt.contains("predicate"),
            "prompt should explain Branch predicate"
        );
        assert!(
            prompt.contains("original input"),
            "prompt should clarify that if_true/if_false receive original input"
        );
        assert!(
            prompt.contains("Stage Config"),
            "prompt should have Stage Config section"
        );
        assert!(
            prompt.contains("\"Const\""),
            "prompt should list Const as a valid op"
        );
        assert!(
            prompt.contains("config") && prompt.contains("key"),
            "prompt should explain config pattern for parameterized stages"
        );
    }

    #[test]
    fn candidates_show_relevance_score() {
        use noether_core::stdlib::load_stdlib;

        let stages = load_stdlib();
        let stage = stages.first().unwrap();
        let r = make_search_result(&stage.id.0, 0.75);
        let candidates: Vec<(&SearchResult, &Stage)> = vec![(&r, stage)];

        let prompt = build_system_prompt(&candidates);
        assert!(
            prompt.contains("relevance: 0.75"),
            "prompt should display the fused relevance score"
        );
    }
}
