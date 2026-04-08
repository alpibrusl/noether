use crate::stage::{Stage, StageBuilder};
use crate::types::NType;
use ed25519_dalek::SigningKey;
use serde_json::json;

pub fn stages(key: &SigningKey) -> Vec<Stage> {
    vec![
        StageBuilder::new("router")
            .input(NType::record([
                // The current route path (e.g. "/todos", "/settings")
                ("route", NType::Text),
                // Fallback route key if `route` matches nothing
                ("default", NType::Text),
                // Map of route path → VNode (provided as a Record of VNode values)
                ("routes", NType::Any),
            ]))
            .output(NType::VNode)
            .pure()
            .description("Route a path to a VNode: return routes[route] or routes[default]")
            .example(
                json!({ "route": "/home", "default": "/home",
                         "routes": { "/home": {"tag":"div","props":{},"children":[]} } }),
                json!({"tag":"div","props":{},"children":[]}),
            )
            .example(
                json!({ "route": "/missing", "default": "/home",
                         "routes": { "/home": {"tag":"span","props":{},"children":[]} } }),
                json!({"tag":"span","props":{},"children":[]}),
            )
            .example(
                json!({ "route": "/a", "default": "/a",
                         "routes": { "/a": {"$text":"A"}, "/b": {"$text":"B"} } }),
                json!({"$text":"A"}),
            )
            .example(
                json!({ "route": "/b", "default": "/a",
                         "routes": { "/a": {"$text":"A"}, "/b": {"$text":"B"} } }),
                json!({"$text":"B"}),
            )
            .example(
                json!({ "route": "/unknown", "default": "/a",
                         "routes": { "/a": {"$text":"Home"} } }),
                json!({"$text":"Home"}),
            )
            .build_stdlib(key)
            .unwrap(),
    ]
}
