use crate::capability::Capability;
use crate::effects::{Effect, EffectSet};
use crate::stage::property::Property;
use crate::stage::{Stage, StageBuilder};
use crate::types::NType;
use ed25519_dalek::SigningKey;
use serde_json::json;

fn http_response_type() -> NType {
    NType::record([
        ("status", NType::Number),
        ("body", NType::Text),
        (
            "headers",
            NType::Map {
                key: Box::new(NType::Text),
                value: Box::new(NType::Text),
            },
        ),
    ])
}

fn http_request_type(with_body: bool) -> NType {
    let mut fields = vec![
        ("url", NType::Text),
        (
            "headers",
            NType::optional(NType::Map {
                key: Box::new(NType::Text),
                value: Box::new(NType::Text),
            }),
        ),
    ];
    if with_body {
        fields.push(("body", NType::Text));
        fields.push(("content_type", NType::optional(NType::Text)));
    }
    NType::record(fields)
}

pub fn stages(key: &SigningKey) -> Vec<Stage> {
    vec![
        StageBuilder::new("read_file")
            .input(NType::record([("path", NType::Text)]))
            .output(NType::record([
                ("content", NType::Text),
                ("size_bytes", NType::Number),
            ]))
            .effects(EffectSet::new([Effect::Fallible, Effect::NonDeterministic]))
            .capability(Capability::FsRead)
            .description("Read a file's contents as text")
            .example(json!({"path": "/tmp/test.txt"}), json!({"content": "hello world", "size_bytes": 11}))
            .example(json!({"path": "data.csv"}), json!({"content": "a,b,c\n1,2,3", "size_bytes": 11}))
            .example(json!({"path": "/etc/hostname"}), json!({"content": "myhost\n", "size_bytes": 7}))
            .example(json!({"path": "empty.txt"}), json!({"content": "", "size_bytes": 0}))
            .example(json!({"path": "config.json"}), json!({"content": "{}", "size_bytes": 2}))
            .tag("io").tag("filesystem").tag("file")
            .alias("file_read").alias("load_file").alias("cat")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("write_file")
            .input(NType::record([
                ("path", NType::Text),
                ("content", NType::Text),
            ]))
            .output(NType::record([
                ("path", NType::Text),
                ("bytes_written", NType::Number),
            ]))
            .effects(EffectSet::new([Effect::Fallible]))
            .capability(Capability::FsWrite)
            .description("Write text content to a file")
            .example(json!({"path": "/tmp/out.txt", "content": "hello"}), json!({"path": "/tmp/out.txt", "bytes_written": 5}))
            .example(json!({"path": "data.csv", "content": "a,b\n1,2"}), json!({"path": "data.csv", "bytes_written": 7}))
            .example(json!({"path": "empty.txt", "content": ""}), json!({"path": "empty.txt", "bytes_written": 0}))
            .example(json!({"path": "log.txt", "content": "entry\n"}), json!({"path": "log.txt", "bytes_written": 6}))
            .example(json!({"path": "out.json", "content": "{}"}), json!({"path": "out.json", "bytes_written": 2}))
            .tag("io").tag("filesystem").tag("file")
            .alias("file_write").alias("save_file").alias("write")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("http_get")
            .input(http_request_type(false))
            .output(http_response_type())
            .effects(EffectSet::new([Effect::Network, Effect::Fallible]))
            .capability(Capability::Network)
            .description("Make an HTTP GET request")
            .example(json!({"url": "https://api.example.com/data", "headers": null}), json!({"status": 200, "body": "{\"ok\":true}", "headers": {"content-type": "application/json"}}))
            .example(json!({"url": "https://example.com", "headers": {"accept": "text/html"}}), json!({"status": 200, "body": "<html></html>", "headers": {"content-type": "text/html"}}))
            .example(json!({"url": "https://api.example.com/404", "headers": null}), json!({"status": 404, "body": "not found", "headers": {"content-type": "text/plain"}}))
            .example(json!({"url": "https://api.example.com/json", "headers": null}), json!({"status": 200, "body": "[]", "headers": {"content-type": "application/json"}}))
            .example(json!({"url": "https://example.com/health", "headers": null}), json!({"status": 200, "body": "ok", "headers": {"content-type": "text/plain"}}))
            .tag("io").tag("http").tag("network").tag("web")
            .alias("fetch").alias("wget").alias("curl_get").alias("get_request")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("http_post")
            .input(http_request_type(true))
            .output(http_response_type())
            .effects(EffectSet::new([Effect::Network, Effect::Fallible]))
            .capability(Capability::Network)
            .description("Make an HTTP POST request")
            .example(json!({"url": "https://api.example.com/submit", "body": "{\"key\":\"value\"}", "headers": null, "content_type": "application/json"}), json!({"status": 201, "body": "{\"id\":1}", "headers": {"content-type": "application/json"}}))
            .example(json!({"url": "https://api.example.com/data", "body": "data", "headers": null, "content_type": null}), json!({"status": 200, "body": "ok", "headers": {"content-type": "text/plain"}}))
            .example(json!({"url": "https://api.example.com/form", "body": "field=value", "headers": null, "content_type": "application/x-www-form-urlencoded"}), json!({"status": 200, "body": "done", "headers": {"content-type": "text/plain"}}))
            .example(json!({"url": "https://api.example.com/err", "body": "{}", "headers": null, "content_type": null}), json!({"status": 400, "body": "bad request", "headers": {"content-type": "text/plain"}}))
            .example(json!({"url": "https://api.example.com/xml", "body": "<data/>", "headers": null, "content_type": "application/xml"}), json!({"status": 200, "body": "<ok/>", "headers": {"content-type": "application/xml"}}))
            .tag("io").tag("http").tag("network").tag("web")
            .alias("post_request").alias("curl_post").alias("submit")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("http_put")
            .input(http_request_type(true))
            .output(http_response_type())
            .effects(EffectSet::new([Effect::Network, Effect::Fallible]))
            .capability(Capability::Network)
            .description("Make an HTTP PUT request")
            .example(json!({"url": "https://api.example.com/resource/1", "body": "{\"name\":\"updated\"}", "headers": null, "content_type": "application/json"}), json!({"status": 200, "body": "{\"name\":\"updated\"}", "headers": {"content-type": "application/json"}}))
            .example(json!({"url": "https://api.example.com/item", "body": "data", "headers": null, "content_type": null}), json!({"status": 204, "body": "", "headers": {"content-type": "text/plain"}}))
            .example(json!({"url": "https://api.example.com/new", "body": "{}", "headers": null, "content_type": "application/json"}), json!({"status": 201, "body": "{\"id\":2}", "headers": {"content-type": "application/json"}}))
            .example(json!({"url": "https://api.example.com/err", "body": "bad", "headers": null, "content_type": null}), json!({"status": 500, "body": "error", "headers": {"content-type": "text/plain"}}))
            .example(json!({"url": "https://api.example.com/ok", "body": "test", "headers": {"x-key": "val"}, "content_type": null}), json!({"status": 200, "body": "ok", "headers": {"content-type": "text/plain"}}))
            .tag("io").tag("http").tag("network").tag("web")
            .alias("put_request").alias("curl_put").alias("update_resource")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("stdin_read")
            .input(NType::Null)
            .output(NType::Text)
            // NonDeterministic: output depends on ambient stdin, not on
            // input. Required for `stage test` to skip it — running it
            // during a test harness would spuriously mismatch against the
            // declared example outputs.
            .effects(EffectSet::new([Effect::Fallible, Effect::NonDeterministic]))
            .description("Read all available text from standard input")
            .example(json!(null), json!("hello world"))
            .example(json!(null), json!("line1\nline2"))
            .example(json!(null), json!(""))
            .example(json!(null), json!("user input"))
            .example(json!(null), json!("42"))
            .tag("io").tag("stdin").tag("pipe")
            .alias("read_stdin").alias("getline").alias("read_input")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("stdout_write")
            .input(NType::record([("text", NType::Text)]))
            .output(NType::Null)
            // Writing to stdout is a process-level side effect, not Pure —
            // leaving it Pure caused `stage test` to contaminate the ACLI
            // report with the stage's own print output when exercised.
            .effects(EffectSet::new([Effect::Process]))
            .description("Write text to standard output")
            .example(json!({"text": "hello"}), json!(null))
            .example(json!({"text": "line1\nline2"}), json!(null))
            .example(json!({"text": ""}), json!(null))
            .example(json!({"text": "result: 42"}), json!(null))
            .example(json!({"text": "done\n"}), json!(null))
            .tag("io").tag("stdout").tag("pipe")
            .alias("print").alias("echo").alias("write_output")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("env_get")
            .input(NType::record([("name", NType::Text)]))
            .output(NType::union(vec![NType::Text, NType::Null]))
            .effects(EffectSet::new([Effect::Fallible, Effect::NonDeterministic]))
            .capability(Capability::FsRead)
            .description("Read an environment variable; returns null if not set")
            .example(json!({"name": "HOME"}), json!("/home/user"))
            .example(json!({"name": "PATH"}), json!("/usr/bin:/usr/local/bin"))
            .example(json!({"name": "UNDEFINED_VAR"}), json!(null))
            .example(json!({"name": "USER"}), json!("alice"))
            .example(json!({"name": "LANG"}), json!("en_US.UTF-8"))
            .tag("io").tag("environment").tag("config")
            .alias("getenv").alias("os_getenv").alias("read_env_var")
            .build_stdlib(key)
            .unwrap(),
        // ── HTTP response adapters ─────────────────────────────────────────────
        StageBuilder::new("http_body")
            .input(http_response_type())
            .output(NType::Text)
            .pure()
            .description("Extract the body text from an HTTP response record")
            .example(json!({"status": 200, "body": "{\"ok\":true}", "headers": {"content-type": "application/json"}}), json!("{\"ok\":true}"))
            .example(json!({"status": 404, "body": "not found", "headers": {}}), json!("not found"))
            .example(json!({"status": 200, "body": "", "headers": {}}), json!(""))
            .example(json!({"status": 201, "body": "{\"id\":1}", "headers": {"content-type": "application/json"}}), json!("{\"id\":1}"))
            .example(json!({"status": 200, "body": "ok", "headers": {"content-type": "text/plain"}}), json!("ok"))
            .tag("io").tag("http").tag("network").tag("pure")
            .alias("response_body").alias("get_body")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("http_status")
            .input(http_response_type())
            .output(NType::Number)
            .pure()
            .description("Extract the status code from an HTTP response record")
            .example(json!({"status": 200, "body": "ok", "headers": {}}), json!(200.0))
            .example(json!({"status": 404, "body": "not found", "headers": {}}), json!(404.0))
            .example(json!({"status": 201, "body": "{}", "headers": {}}), json!(201.0))
            .example(json!({"status": 500, "body": "error", "headers": {}}), json!(500.0))
            .example(json!({"status": 301, "body": "", "headers": {"location": "/new"}}), json!(301.0))
            .tag("io").tag("http").tag("network").tag("pure")
            .alias("response_status").alias("status_code").alias("get_status")
            .property(Property::Range {
                field: "output".into(),
                min: Some(100.0),
                max: Some(599.0),
            })
            .property(Property::Range {
                field: "input.status".into(),
                min: Some(100.0),
                max: Some(599.0),
            })
            .build_stdlib(key)
            .unwrap(),
    ]
}
