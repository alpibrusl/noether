use crate::executor::ExecutionError;
use serde_json::Value;

fn fail(stage: &str, msg: impl Into<String>) -> ExecutionError {
    ExecutionError::StageFailed {
        stage_id: noether_core::stage::StageId(stage.into()),
        message: msg.into(),
    }
}

/// Route a path to a VNode.
///
/// Input:  `{ route: Text, default: Text, routes: Record<Text, VNode> }`
/// Output: `VNode` — `routes[route]` if the key exists, otherwise `routes[default]`
pub fn router(input: &Value) -> Result<Value, ExecutionError> {
    let route = input["route"].as_str().unwrap_or("/");
    let default = input["default"].as_str().unwrap_or("/");
    let routes = input["routes"].as_object().ok_or_else(|| {
        fail("noether.router", "routes must be a record (object)")
    })?;

    // Exact match first
    if let Some(vnode) = routes.get(route) {
        return Ok(vnode.clone());
    }

    // Prefix match: find the longest matching prefix
    let mut best: Option<(&str, &Value)> = None;
    for (key, vnode) in routes {
        if route.starts_with(key.as_str()) {
            if best.map_or(true, |(bk, _)| key.len() > bk.len()) {
                best = Some((key, vnode));
            }
        }
    }
    if let Some((_, vnode)) = best {
        return Ok(vnode.clone());
    }

    // Default fallback
    routes
        .get(default)
        .cloned()
        .ok_or_else(|| fail("noether.router", format!("default route '{default}' not found in routes")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn router_exact_match() {
        let input = json!({
            "route": "/home",
            "default": "/home",
            "routes": {
                "/home": {"tag":"div","props":{},"children":[]},
                "/about": {"tag":"span","props":{},"children":[]}
            }
        });
        let out = router(&input).unwrap();
        assert_eq!(out["tag"], "div");
    }

    #[test]
    fn router_falls_back_to_default() {
        let input = json!({
            "route": "/unknown",
            "default": "/home",
            "routes": { "/home": {"tag":"div","props":{},"children":[]} }
        });
        let out = router(&input).unwrap();
        assert_eq!(out["tag"], "div");
    }

    #[test]
    fn router_prefix_match() {
        let input = json!({
            "route": "/todos/123",
            "default": "/",
            "routes": {
                "/": {"tag":"main","props":{},"children":[]},
                "/todos": {"tag":"ul","props":{},"children":[]}
            }
        });
        let out = router(&input).unwrap();
        assert_eq!(out["tag"], "ul");
    }

    #[test]
    fn router_missing_default_is_error() {
        let input = json!({
            "route": "/missing",
            "default": "/also-missing",
            "routes": { "/home": {"tag":"div","props":{},"children":[]} }
        });
        assert!(router(&input).is_err());
    }
}
