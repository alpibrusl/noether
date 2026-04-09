# Stdlib Stage Catalogue

Noether ships with 80+ deterministically-identified, Ed25519-signed stdlib stages.

## Scalar (5)

| Name | Input | Output | Description |
|---|---|---|---|
| `text_upper` | `Record { text: Text }` | `Record { text: Text }` | Uppercase a string |
| `text_lower` | `Record { text: Text }` | `Record { text: Text }` | Lowercase a string |
| `text_length` | `Record { text: Text }` | `Record { count: Number }` | Character count |
| `number_round` | `Record { value: Number, places: Number }` | `Record { result: Number }` | Round to N decimal places |
| `bool_not` | `Record { value: Bool }` | `Record { result: Bool }` | Boolean NOT |

## Collections (8)

| Name | Input | Output | Description |
|---|---|---|---|
| `list_map` | `Record { items: List(Any), fn: Text }` | `Record { items: List(Any) }` | Apply a transform to each element |
| `list_filter` | `Record { items: List(Any), predicate: Text }` | `Record { items: List(Any) }` | Filter by predicate |
| `list_sort` | `Record { items: List(Any), key: Text }` | `Record { items: List(Any) }` | Sort by key |
| `list_take` | `Record { items: List(Any), n: Number }` | `Record { items: List(Any) }` | Take first N |
| `list_flatten` | `Record { items: List(List(Any)) }` | `Record { items: List(Any) }` | Flatten one level |
| `list_unique` | `Record { items: List(Any) }` | `Record { items: List(Any) }` | Deduplicate |
| `map_get` | `Record { map: Map(Any), key: Text }` | `Record { value: Any }` | Get map value by key |
| `map_keys` | `Record { map: Map(Any) }` | `Record { keys: List(Text) }` | Extract map keys |

## Control (6)

| Name | Input | Output | Description |
|---|---|---|---|
| `identity` | `Any` | `Any` | Pass-through |
| `const` | `Any` | `Any` | Return a literal constant |
| `branch` | `Record { condition: Bool, then: Any, else: Any }` | `Any` | Conditional |
| `retry` | `Record { stage_id: Text, input: Any, max: Number }` | `Any` | Retry on failure |
| `error` | `Record { message: Text }` | `Null` | Raise a named error |
| `log` | `Record { message: Text, level: Text }` | `Null` | Emit a log entry |

## I/O (8)

| Name | Input | Output | Description |
|---|---|---|---|
| `http_get` | `Record { url: Text }` | `Record { status: Number, body: Text }` | HTTP GET |
| `http_post` | `Record { url: Text, body: Text }` | `Record { status: Number, body: Text }` | HTTP POST |
| `json_parse` | `Record { text: Text }` | `Any` | Parse JSON string |
| `json_stringify` | `Any` | `Record { text: Text }` | Serialise to JSON string |
| `file_read` | `Record { path: Text }` | `Record { content: Text }` | Read file |
| `file_write` | `Record { path: Text, content: Text }` | `Null` | Write file |
| `env_get` | `Record { key: Text }` | `Record { value: Text }` | Read environment variable |
| `sleep` | `Record { ms: Number }` | `Null` | Sleep N milliseconds |

## LLM Primitives (4)

| Name | Input | Output | Description |
|---|---|---|---|
| `llm_complete` | `Record { prompt: Text }` | `Record { text: Text }` | Single-turn LLM completion |
| `llm_embed` | `Record { text: Text }` | `Record { embedding: List(Number) }` | Text embedding |
| `llm_classify` | `Record { text: Text, labels: List(Text) }` | `Record { label: Text, score: Number }` | Zero-shot classification |
| `llm_extract` | `Record { text: Text, schema: Text }` | `Any` | Structured extraction |

## Data (7)

| Name | Input | Output | Description |
|---|---|---|---|
| `csv_parse` | `Record { text: Text }` | `Record { rows: List(Map(Text)) }` | Parse CSV |
| `csv_stringify` | `Record { rows: List(Map(Text)) }` | `Record { text: Text }` | Serialise to CSV |
| `stats_mean` | `Record { values: List(Number) }` | `Record { mean: Number }` | Arithmetic mean |
| `stats_stddev` | `Record { values: List(Number) }` | `Record { stddev: Number }` | Standard deviation |
| `stats_histogram` | `Record { values: List(Number), bins: Number }` | `Record { bins: List(Any) }` | Histogram |
| `schema_validate` | `Record { value: Any, schema: Text }` | `Record { valid: Bool, errors: List(Text) }` | JSON Schema validation |
| `diff` | `Record { before: Any, after: Any }` | `Record { changes: List(Any) }` | Structural diff |

## Noether Internal (6)

| Name | Input | Output | Description |
|---|---|---|---|
| `stage_search` | `Record { query: Text, limit: Number }` | `Record { stages: List(Any) }` | Semantic stage search |
| `stage_get` | `Record { id: Text }` | `Any` | Get stage by ID |
| `stage_put` | `Any` | `Record { id: Text }` | Register a stage |
| `composition_run` | `Record { graph: Any }` | `Any` | Execute a graph |
| `trace_get` | `Record { id: Text }` | `Any` | Get composition trace |
| `store_stats` | `Null` | `Record { counts: Any }` | Store statistics |

## Text Processing (6)

| Name | Input | Output | Description |
|---|---|---|---|
| `text_split` | `Record { text: Text, sep: Text }` | `Record { parts: List(Text) }` | Split by separator |
| `text_join` | `Record { parts: List(Text), sep: Text }` | `Record { text: Text }` | Join with separator |
| `text_trim` | `Record { text: Text }` | `Record { text: Text }` | Strip whitespace |
| `text_replace` | `Record { text: Text, from: Text, to: Text }` | `Record { text: Text }` | Find and replace |
| `regex_match` | `Record { text: Text, pattern: Text }` | `Record { matches: List(Text) }` | Regex match |
| `regex_replace` | `Record { text: Text, pattern: Text, replacement: Text }` | `Record { text: Text }` | Regex replace |

!!! tip "Searching by type"
    Use `noether stage search` with a description of what you want. The semantic index understands intent, not just names.

    ```bash
    noether stage search "split text into words"
    ```
