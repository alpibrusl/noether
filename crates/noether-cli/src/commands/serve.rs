use super::resolver_utils::resolve_and_emit_diagnostics;
use noether_engine::executor::composite::CompositeExecutor;
use noether_engine::executor::runner::run_composition;
use noether_engine::lagrange::{parse_graph, CompositionGraph};
use noether_store::StageStore;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;

struct Route {
    path: String,
    graph: CompositionGraph,
    description: String,
}

pub fn cmd_serve(
    store: &dyn StageStore,
    executor: &CompositeExecutor,
    config_path: &str,
    bind: &str,
) {
    let content = match std::fs::read_to_string(config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to read {config_path}: {e}");
            std::process::exit(1);
        }
    };

    // Detect format: if it has "routes" → multi-route API config,
    // otherwise → single graph (backward compatible).
    let routes = if content.contains("\"routes\"") {
        let config: serde_json::Value = match serde_json::from_str(&content) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Invalid API config: {e}");
                std::process::exit(1);
            }
        };
        let route_map: BTreeMap<String, String> = config["routes"]
            .as_object()
            .map(|obj| {
                obj.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        let mut routes = Vec::new();
        for (path, graph_path) in &route_map {
            let graph_content = match std::fs::read_to_string(graph_path) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Failed to read graph {graph_path}: {e}");
                    std::process::exit(1);
                }
            };
            let mut graph = match parse_graph(&graph_content) {
                Ok(g) => g,
                Err(e) => {
                    eprintln!("Invalid graph {graph_path}: {e}");
                    std::process::exit(1);
                }
            };
            // Resolve signature/canonical pinning → impl IDs, and
            // auto-follow deprecation chains, so graphs authored today
            // keep serving after an implementation is rotated.
            if let Err(msg) = resolve_and_emit_diagnostics(&mut graph, store) {
                eprintln!("Graph {graph_path}: {msg}");
                std::process::exit(1);
            }
            if let Err(errors) = noether_engine::checker::check_graph(&graph.root, store) {
                let msgs: Vec<String> = errors.iter().map(|e| format!("{e}")).collect();
                eprintln!(
                    "Graph {graph_path} type check failed:\n  {}",
                    msgs.join("\n  ")
                );
                std::process::exit(1);
            }
            let desc = graph.description.clone();
            routes.push(Route {
                path: path.clone(),
                graph,
                description: desc,
            });
        }
        routes
    } else {
        // Single graph file (backward compatible)
        let mut graph = match parse_graph(&content) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("Invalid graph JSON: {e}");
                std::process::exit(1);
            }
        };
        if let Err(msg) = resolve_and_emit_diagnostics(&mut graph, store) {
            eprintln!("{msg}");
            std::process::exit(1);
        }
        if let Err(errors) = noether_engine::checker::check_graph(&graph.root, store) {
            let msgs: Vec<String> = errors.iter().map(|e| format!("{e}")).collect();
            eprintln!("Graph type check failed:\n  {}", msgs.join("\n  "));
            std::process::exit(1);
        }
        let desc = graph.description.clone();
        vec![Route {
            path: "/".to_string(),
            graph,
            description: desc,
        }]
    };

    let addr = if bind.starts_with(':') {
        format!("0.0.0.0{bind}")
    } else {
        bind.to_string()
    };

    let listener = TcpListener::bind(&addr).unwrap_or_else(|e| {
        eprintln!("Cannot bind to {addr}: {e}");
        std::process::exit(1);
    });

    eprintln!("noether serve: {} route(s)", routes.len());
    for r in &routes {
        eprintln!("  POST {:<20} — {}", r.path, r.description);
    }
    eprintln!("  GET  /health");
    eprintln!("Listening on http://{addr}");
    eprintln!("Press Ctrl+C to stop");

    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(s) => s,
            Err(_) => continue,
        };

        let mut buf = vec![0u8; 1_048_576]; // 1MB max request
        let n = stream.read(&mut buf).unwrap_or(0);
        let request = String::from_utf8_lossy(&buf[..n]);

        let first_line = request.lines().next().unwrap_or("");
        let parts: Vec<&str> = first_line.split_whitespace().collect();
        let (method, path) = if parts.len() >= 2 {
            (parts[0], parts[1])
        } else {
            ("GET", "/")
        };

        let body = request
            .split("\r\n\r\n")
            .nth(1)
            .unwrap_or("")
            .trim_end_matches('\0')
            .to_string();

        let (status, response_body) = match method {
            "GET" if path == "/health" => {
                let endpoints: Vec<serde_json::Value> = routes
                    .iter()
                    .map(|r| {
                        serde_json::json!({
                            "path": r.path,
                            "description": r.description,
                        })
                    })
                    .collect();
                let health = serde_json::json!({
                    "ok": true,
                    "routes": endpoints,
                });
                ("200 OK", serde_json::to_string(&health).unwrap())
            }
            "POST" => {
                // Find matching route
                if let Some(route) = routes.iter().find(|r| r.path == path) {
                    let input: serde_json::Value =
                        serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);

                    match run_composition(&route.graph.root, &input, executor, "serve") {
                        Ok(result) => {
                            let resp = serde_json::json!({
                                "ok": true,
                                "output": result.output,
                                "duration_ms": result.trace.duration_ms,
                            });
                            ("200 OK", serde_json::to_string(&resp).unwrap())
                        }
                        Err(e) => {
                            let resp = serde_json::json!({
                                "ok": false,
                                "error": format!("{e}"),
                            });
                            (
                                "500 Internal Server Error",
                                serde_json::to_string(&resp).unwrap(),
                            )
                        }
                    }
                } else {
                    let available: Vec<&str> = routes.iter().map(|r| r.path.as_str()).collect();
                    let resp = serde_json::json!({
                        "ok": false,
                        "error": format!("No route for POST {path}"),
                        "available_routes": available,
                    });
                    ("404 Not Found", serde_json::to_string(&resp).unwrap())
                }
            }
            _ => {
                let resp = serde_json::json!({"ok": false, "error": "Use POST with JSON body"});
                (
                    "405 Method Not Allowed",
                    serde_json::to_string(&resp).unwrap(),
                )
            }
        };

        let http_response = format!(
            "HTTP/1.1 {status}\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             Access-Control-Allow-Origin: *\r\n\
             Access-Control-Allow-Methods: POST, GET, OPTIONS\r\n\
             Access-Control-Allow-Headers: Content-Type\r\n\
             \r\n\
             {}",
            response_body.len(),
            response_body
        );
        let _ = stream.write_all(http_response.as_bytes());

        eprintln!("{method} {path} → {status}");
    }
}
