use crate::capability::Capability;
use crate::effects::{Effect, EffectSet};
use crate::stage::{Stage, StageBuilder};
use crate::types::NType;
use ed25519_dalek::SigningKey;
use serde_json::json;

fn llm_effects() -> EffectSet {
    EffectSet::new([
        Effect::Llm {
            model: "default".into(),
        },
        Effect::NonDeterministic,
        Effect::Fallible,
        Effect::Cost { cents: 5 },
    ])
}

fn llm_embed_effects() -> EffectSet {
    EffectSet::new([
        Effect::Llm {
            model: "default".into(),
        },
        Effect::Fallible,
        Effect::Cost { cents: 1 },
    ])
}

fn llm_classify_effects() -> EffectSet {
    EffectSet::new([
        Effect::Llm {
            model: "default".into(),
        },
        Effect::NonDeterministic,
        Effect::Fallible,
        Effect::Cost { cents: 3 },
    ])
}

pub fn stages(key: &SigningKey) -> Vec<Stage> {
    vec![
        StageBuilder::new("llm_complete")
            .input(NType::record([
                ("prompt", NType::Text),
                ("model", NType::optional(NType::Text)),
                ("max_tokens", NType::optional(NType::Number)),
                ("temperature", NType::optional(NType::Number)),
                ("system", NType::optional(NType::Text)),
            ]))
            .output(NType::record([
                ("text", NType::Text),
                ("tokens_used", NType::Number),
                ("model", NType::Text),
            ]))
            .effects(llm_effects())
            .capability(Capability::Network)
            .capability(Capability::Llm)
            .cost(Some(200), Some(500), Some(10))
            .description("Generate text completion using a language model")
            .example(
                json!({"prompt": "What is 2+2?", "model": null, "max_tokens": 100, "temperature": null, "system": null}),
                json!({"text": "2+2 equals 4.", "tokens_used": 8, "model": "claude-sonnet-4"}),
            )
            .example(
                json!({"prompt": "Hello", "model": "claude-sonnet-4", "max_tokens": null, "temperature": 0.5, "system": "Be brief"}),
                json!({"text": "Hi there!", "tokens_used": 4, "model": "claude-sonnet-4"}),
            )
            .example(
                json!({"prompt": "Translate: hello", "model": null, "max_tokens": 50, "temperature": null, "system": null}),
                json!({"text": "hola", "tokens_used": 3, "model": "claude-sonnet-4"}),
            )
            .example(
                json!({"prompt": "List 3 colors", "model": null, "max_tokens": null, "temperature": null, "system": null}),
                json!({"text": "red, blue, green", "tokens_used": 6, "model": "claude-sonnet-4"}),
            )
            .example(
                json!({"prompt": "Yes or no?", "model": null, "max_tokens": 10, "temperature": 0.0, "system": null}),
                json!({"text": "Yes", "tokens_used": 2, "model": "claude-sonnet-4"}),
            )
            .tag("llm").tag("ai").tag("generation").tag("non-deterministic")
            .alias("gpt").alias("claude").alias("chat_completion").alias("text_generation").alias("prompt")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("llm_embed")
            .input(NType::record([
                ("text", NType::Text),
                ("model", NType::optional(NType::Text)),
            ]))
            .output(NType::record([
                ("embedding", NType::List(Box::new(NType::Number))),
                ("dimensions", NType::Number),
                ("model", NType::Text),
            ]))
            .effects(llm_embed_effects())
            .capability(Capability::Network)
            .capability(Capability::Llm)
            .cost(Some(50), None, Some(5))
            .description("Generate a vector embedding for text")
            .example(json!({"text": "hello world", "model": null}), json!({"embedding": [0.1, 0.2, 0.3], "dimensions": 3, "model": "text-embedding-3-small"}))
            .example(json!({"text": "test", "model": "text-embedding-3-small"}), json!({"embedding": [0.5, -0.1], "dimensions": 2, "model": "text-embedding-3-small"}))
            .example(json!({"text": "", "model": null}), json!({"embedding": [0.0, 0.0], "dimensions": 2, "model": "text-embedding-3-small"}))
            .example(json!({"text": "long text here", "model": null}), json!({"embedding": [0.3, 0.4, 0.5, 0.6], "dimensions": 4, "model": "text-embedding-3-small"}))
            .example(json!({"text": "another", "model": null}), json!({"embedding": [-0.2, 0.8], "dimensions": 2, "model": "text-embedding-3-small"}))
            .tag("llm").tag("ai").tag("embeddings").tag("vector")
            .alias("embed").alias("vectorize").alias("encode_text").alias("semantic_embedding")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("llm_classify")
            .input(NType::record([
                ("text", NType::Text),
                ("categories", NType::List(Box::new(NType::Text))),
                ("model", NType::optional(NType::Text)),
            ]))
            .output(NType::record([
                ("category", NType::Text),
                ("confidence", NType::Number),
                ("model", NType::Text),
            ]))
            .effects(llm_classify_effects())
            .capability(Capability::Network)
            .capability(Capability::Llm)
            .cost(Some(150), Some(200), Some(10))
            .description("Classify text into one of the provided categories")
            .example(json!({"text": "I love this product!", "categories": ["positive", "negative", "neutral"], "model": null}), json!({"category": "positive", "confidence": 0.95, "model": "claude-sonnet-4"}))
            .example(json!({"text": "This is terrible", "categories": ["positive", "negative"], "model": null}), json!({"category": "negative", "confidence": 0.92, "model": "claude-sonnet-4"}))
            .example(json!({"text": "Buy now!", "categories": ["spam", "not_spam"], "model": null}), json!({"category": "spam", "confidence": 0.88, "model": "claude-sonnet-4"}))
            .example(json!({"text": "Hello there", "categories": ["greeting", "farewell", "question"], "model": null}), json!({"category": "greeting", "confidence": 0.97, "model": "claude-sonnet-4"}))
            .example(json!({"text": "What time is it?", "categories": ["question", "statement"], "model": null}), json!({"category": "question", "confidence": 0.99, "model": "claude-sonnet-4"}))
            .tag("llm").tag("ai").tag("classification").tag("non-deterministic")
            .alias("categorize").alias("label").alias("text_classify").alias("sentiment")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("llm_extract")
            .input(NType::record([
                ("text", NType::Text),
                ("schema", NType::Record(Default::default())),
                ("model", NType::optional(NType::Text)),
            ]))
            .output(NType::record([
                ("extracted", NType::Any),
                ("model", NType::Text),
            ]))
            .effects(llm_effects())
            .capability(Capability::Network)
            .capability(Capability::Llm)
            .cost(Some(300), Some(800), Some(10))
            .description("Extract structured data from text according to a schema")
            .example(json!({"text": "John is 30 years old", "schema": {}, "model": null}), json!({"extracted": {"name": "John", "age": 30}, "model": "claude-sonnet-4"}))
            .example(json!({"text": "Email: test@example.com, Phone: 555-1234", "schema": {}, "model": null}), json!({"extracted": {"email": "test@example.com", "phone": "555-1234"}, "model": "claude-sonnet-4"}))
            .example(json!({"text": "The price is $42.99", "schema": {}, "model": null}), json!({"extracted": {"price": 42.99, "currency": "USD"}, "model": "claude-sonnet-4"}))
            .example(json!({"text": "Meeting on 2024-01-15 at 3pm", "schema": {}, "model": null}), json!({"extracted": {"date": "2024-01-15", "time": "15:00"}, "model": "claude-sonnet-4"}))
            .example(json!({"text": "No relevant data here", "schema": {}, "model": null}), json!({"extracted": {}, "model": "claude-sonnet-4"}))
            .tag("llm").tag("ai").tag("extraction").tag("non-deterministic")
            .alias("parse_text").alias("ner").alias("named_entity").alias("information_extraction")
            .build_stdlib(key)
            .unwrap(),
    ]
}
