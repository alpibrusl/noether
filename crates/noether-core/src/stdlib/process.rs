use crate::capability::Capability;
use crate::effects::{Effect, EffectSet};
use crate::stage::{Stage, StageBuilder};
use crate::types::NType;
use ed25519_dalek::SigningKey;
use serde_json::json;

fn env_map_type() -> NType {
    NType::optional(NType::Map {
        key: Box::new(NType::Text),
        value: Box::new(NType::Text),
    })
}

pub fn stages(key: &SigningKey) -> Vec<Stage> {
    vec![
        // ── spawn_process ─────────────────────────────────────────────────────
        StageBuilder::new("spawn_process")
            .input(NType::record([
                ("cmd", NType::Text),
                ("args", NType::optional(NType::List(Box::new(NType::Text)))),
                ("env", env_map_type()),
                ("cwd", NType::optional(NType::Text)),
            ]))
            .output(NType::record([
                ("pid", NType::Number),
                ("started_at", NType::Number),
            ]))
            .effects(EffectSet::new([Effect::Process, Effect::Fallible]))
            .capability(Capability::Process)
            .description("Spawn a subprocess; returns its PID and Unix start timestamp")
            .example(
                json!({"cmd": "python3", "args": ["agent.py"], "env": null, "cwd": null}),
                json!({"pid": 12345, "started_at": 1712345678}),
            )
            .example(
                json!({"cmd": "node", "args": ["index.js"], "env": {"NODE_ENV": "production"}, "cwd": "/app"}),
                json!({"pid": 12346, "started_at": 1712345680}),
            )
            .example(
                json!({"cmd": "bash", "args": ["-c", "sleep 60"], "env": null, "cwd": null}),
                json!({"pid": 12347, "started_at": 1712345682}),
            )
            .example(
                json!({"cmd": "caloron-harness", "args": ["start"], "env": {"ROLE": "worker"}, "cwd": "/workspace"}),
                json!({"pid": 12348, "started_at": 1712345690}),
            )
            .example(
                json!({"cmd": "ruby", "args": ["agent.rb"], "env": null, "cwd": "/workspace/agent"}),
                json!({"pid": 12349, "started_at": 1712345700}),
            )
            .tag("process").tag("os").tag("subprocess").tag("lifecycle")
            .alias("run_process").alias("exec").alias("popen").alias("execute")
            .build_stdlib(key)
            .unwrap(),

        // ── wait_process ──────────────────────────────────────────────────────
        StageBuilder::new("wait_process")
            .input(NType::record([
                ("pid", NType::Number),
                ("timeout_ms", NType::optional(NType::Number)),
            ]))
            .output(NType::record([
                ("exited", NType::Bool),
                ("timed_out", NType::Bool),
            ]))
            .effects(EffectSet::new([Effect::Process, Effect::Fallible]))
            .capability(Capability::Process)
            .description("Poll until a process exits or timeout_ms elapses; default timeout 30 s")
            .example(
                json!({"pid": 12345, "timeout_ms": 5000}),
                json!({"exited": true, "timed_out": false}),
            )
            .example(
                json!({"pid": 12346, "timeout_ms": 100}),
                json!({"exited": false, "timed_out": true}),
            )
            .example(
                json!({"pid": 99999, "timeout_ms": null}),
                json!({"exited": true, "timed_out": false}),
            )
            .example(
                json!({"pid": 12347, "timeout_ms": 60000}),
                json!({"exited": true, "timed_out": false}),
            )
            .example(
                json!({"pid": 12348, "timeout_ms": 1000}),
                json!({"exited": false, "timed_out": true}),
            )
            .tag("process").tag("os").tag("subprocess").tag("lifecycle")
            .alias("wait_pid").alias("process_wait").alias("join_process")
            .build_stdlib(key)
            .unwrap(),

        // ── signal_process ────────────────────────────────────────────────────
        StageBuilder::new("signal_process")
            .input(NType::record([
                ("pid", NType::Number),
                ("signal", NType::optional(NType::Text)),
            ]))
            .output(NType::record([("sent", NType::Bool)]))
            .effects(EffectSet::new([Effect::Process]))
            .capability(Capability::Process)
            .description(
                "Send a Unix signal to a process (TERM by default); returns whether the signal was delivered",
            )
            .example(
                json!({"pid": 12345, "signal": "TERM"}),
                json!({"sent": true}),
            )
            .example(
                json!({"pid": 12345, "signal": null}),
                json!({"sent": true}),
            )
            .example(
                json!({"pid": 12346, "signal": "HUP"}),
                json!({"sent": true}),
            )
            .example(
                json!({"pid": 12347, "signal": "INT"}),
                json!({"sent": true}),
            )
            .example(
                json!({"pid": 99999, "signal": "TERM"}),
                json!({"sent": false}),
            )
            .tag("process").tag("os").tag("subprocess")
            .alias("kill_signal").alias("send_signal").alias("os_kill")
            .build_stdlib(key)
            .unwrap(),

        // ── kill_process ──────────────────────────────────────────────────────
        StageBuilder::new("kill_process")
            .input(NType::record([("pid", NType::Number)]))
            .output(NType::record([("killed", NType::Bool)]))
            .effects(EffectSet::new([Effect::Process]))
            .capability(Capability::Process)
            .description("Send SIGKILL to a process; returns whether the signal was delivered")
            .example(json!({"pid": 12345}), json!({"killed": true}))
            .example(json!({"pid": 12346}), json!({"killed": true}))
            .example(json!({"pid": 99999}), json!({"killed": false}))
            .example(json!({"pid": 12347}), json!({"killed": true}))
            .example(json!({"pid": 12348}), json!({"killed": true}))
            .tag("process").tag("os").tag("subprocess")
            .alias("terminate").alias("stop_process").alias("sigkill")
            .build_stdlib(key)
            .unwrap(),
    ]
}
