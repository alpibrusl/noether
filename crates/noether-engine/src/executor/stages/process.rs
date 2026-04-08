use crate::executor::ExecutionError;
use noether_core::stage::StageId;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn fail(stage: &str, msg: impl Into<String>) -> ExecutionError {
    ExecutionError::StageFailed {
        stage_id: StageId(stage.into()),
        message: msg.into(),
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn spawn_process(input: &Value) -> Result<Value, ExecutionError> {
    let cmd = input
        .get("cmd")
        .and_then(|v| v.as_str())
        .ok_or_else(|| fail("spawn_process", "missing field 'cmd'"))?;

    let args: Vec<String> = input
        .get("args")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let env: HashMap<String, String> = input
        .get("env")
        .and_then(|v| v.as_object())
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();

    let cwd = input.get("cwd").and_then(|v| v.as_str());

    let mut command = std::process::Command::new(cmd);
    command.args(&args);
    for (k, v) in &env {
        command.env(k, v);
    }
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }

    let child = command
        .spawn()
        .map_err(|e| fail("spawn_process", e.to_string()))?;

    let pid = child.id();
    let started_at = unix_now();

    // Reap the child in a background thread to prevent zombie processes.
    std::thread::spawn(move || {
        let mut c = child;
        let _ = c.wait();
    });

    Ok(json!({ "pid": pid, "started_at": started_at }))
}

pub fn wait_process(input: &Value) -> Result<Value, ExecutionError> {
    let pid = input
        .get("pid")
        .and_then(|v| v.as_f64())
        .map(|v| v as u32)
        .ok_or_else(|| fail("wait_process", "missing field 'pid'"))?;

    let timeout_ms = input
        .get("timeout_ms")
        .and_then(|v| v.as_f64())
        .map(|v| v as u64)
        .unwrap_or(30_000);

    let deadline = Instant::now() + Duration::from_millis(timeout_ms);

    loop {
        // `kill -0 <pid>` succeeds if the process exists, fails otherwise.
        let alive = std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        if !alive {
            return Ok(json!({ "exited": true, "timed_out": false }));
        }

        if Instant::now() >= deadline {
            return Ok(json!({ "exited": false, "timed_out": true }));
        }

        std::thread::sleep(Duration::from_millis(100));
    }
}

pub fn signal_process(input: &Value) -> Result<Value, ExecutionError> {
    let pid = input
        .get("pid")
        .and_then(|v| v.as_f64())
        .map(|v| v as u32)
        .ok_or_else(|| fail("signal_process", "missing field 'pid'"))?;

    let signal = input
        .get("signal")
        .and_then(|v| v.as_str())
        .unwrap_or("TERM");

    // Guard against shell injection: only allow alphanumeric signal names.
    if !signal.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(fail(
            "signal_process",
            format!("invalid signal name '{signal}'; must be alphanumeric (e.g. TERM, HUP, INT)"),
        ));
    }

    let sent = std::process::Command::new("kill")
        .args([&format!("-{signal}"), &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    Ok(json!({ "sent": sent }))
}

pub fn kill_process(input: &Value) -> Result<Value, ExecutionError> {
    let pid = input
        .get("pid")
        .and_then(|v| v.as_f64())
        .map(|v| v as u32)
        .ok_or_else(|| fail("kill_process", "missing field 'pid'"))?;

    let killed = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    Ok(json!({ "killed": killed }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn spawn_and_kill_short_lived_process() {
        // Spawn a sleep process then immediately kill it.
        let input = json!({"cmd": "sleep", "args": ["60"], "env": null, "cwd": null});
        let result = spawn_process(&input).unwrap();
        let pid = result["pid"].as_f64().unwrap() as u32;
        assert!(pid > 0);

        let kill_result = kill_process(&json!({"pid": pid})).unwrap();
        assert_eq!(kill_result["killed"], true);
    }

    #[test]
    fn wait_process_detects_exit() {
        // Spawn a process that exits immediately.
        let input = json!({"cmd": "true", "args": null, "env": null, "cwd": null});
        let result = spawn_process(&input).unwrap();
        let pid = result["pid"].as_f64().unwrap() as u32;

        // Give it a moment to exit, then wait.
        std::thread::sleep(Duration::from_millis(50));
        let wait_result = wait_process(&json!({"pid": pid, "timeout_ms": 2000})).unwrap();
        assert_eq!(wait_result["exited"], true);
        assert_eq!(wait_result["timed_out"], false);
    }

    #[test]
    fn wait_process_times_out() {
        let input = json!({"cmd": "sleep", "args": ["60"], "env": null, "cwd": null});
        let result = spawn_process(&input).unwrap();
        let pid = result["pid"].as_f64().unwrap() as u32;

        let wait_result = wait_process(&json!({"pid": pid, "timeout_ms": 150})).unwrap();
        assert_eq!(wait_result["timed_out"], true);
        assert_eq!(wait_result["exited"], false);

        // Clean up.
        let _ = kill_process(&json!({"pid": pid}));
    }

    #[test]
    fn signal_process_term() {
        let input = json!({"cmd": "sleep", "args": ["60"], "env": null, "cwd": null});
        let result = spawn_process(&input).unwrap();
        let pid = result["pid"].as_f64().unwrap() as u32;

        let sig_result = signal_process(&json!({"pid": pid, "signal": "TERM"})).unwrap();
        assert_eq!(sig_result["sent"], true);
    }

    #[test]
    fn signal_process_rejects_invalid_signal() {
        let err = signal_process(&json!({"pid": 1, "signal": "TERM;rm -rf /"}));
        assert!(err.is_err());
    }

    #[test]
    fn kill_nonexistent_process_returns_false() {
        // PID 2^31-1 almost certainly does not exist.
        let result = kill_process(&json!({"pid": 2147483647})).unwrap();
        assert_eq!(result["killed"], false);
    }

    #[test]
    fn spawn_process_missing_cmd() {
        let err = spawn_process(&json!({"args": null, "env": null, "cwd": null}));
        assert!(err.is_err());
    }
}
