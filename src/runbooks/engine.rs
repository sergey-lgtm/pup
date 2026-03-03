use anyhow::Result;
use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::time::Instant;

use super::template;
use super::{Runbook, Step};
use crate::config::Config;

/// Execute a runbook sequentially, with variable substitution and status updates.
pub async fn run(cfg: &Config, runbook: &Runbook, vars: HashMap<String, String>) -> Result<()> {
    let total = runbook.steps.len();
    let mut step_vars = vars;

    // Pre-fill defaults for vars not provided by --set
    if let Some(var_defs) = &runbook.vars {
        for (k, def) in var_defs {
            if !step_vars.contains_key(k) {
                if let Some(default) = &def.default {
                    step_vars.insert(k.clone(), default.clone());
                }
            }
        }
    }

    eprintln!("Running runbook: {}", runbook.name);
    if let Some(desc) = &runbook.description {
        eprintln!("  {desc}");
    }
    eprintln!();

    let mut last_failed = false;

    for (i, step) in runbook.steps.iter().enumerate() {
        let step_num = i + 1;

        // Check when condition
        let when = step.when.as_deref().unwrap_or("on_success");
        if when == "on_success" && last_failed {
            eprintln!(
                "  [{step_num}/{total}] {} — skipped (previous step failed)",
                step.name
            );
            continue;
        }

        eprintln!("  [{step_num}/{total}] {} ...", step.name);

        let result = execute_step(cfg, step, &step_vars).await;

        match result {
            Ok(output) => {
                if let Some(capture_var) = &step.capture {
                    step_vars.insert(capture_var.clone(), output.trim().to_string());
                }
                last_failed = false;
            }
            Err(e) => {
                let optional = step.optional.unwrap_or(false);
                let on_failure = step.on_failure.as_deref().unwrap_or("fail");

                if optional {
                    eprintln!(
                        "  [{step_num}/{total}] {} — skipped (optional): {e}",
                        step.name
                    );
                    last_failed = false;
                    continue;
                }

                match on_failure {
                    "warn" => {
                        eprintln!("  [{step_num}/{total}] {} — warning: {e}", step.name);
                        last_failed = true;
                    }
                    "confirm" => {
                        eprintln!("  [{step_num}/{total}] {} — failed: {e}", step.name);
                        if !prompt_continue(cfg)? {
                            anyhow::bail!("runbook aborted by user at step {step_num}");
                        }
                        last_failed = true;
                    }
                    _ => {
                        return Err(e.context(format!("step {step_num}/{total}: {}", step.name)));
                    }
                }
            }
        }
    }

    eprintln!();
    eprintln!("  runbook complete ({total} steps)");
    Ok(())
}

async fn execute_step(cfg: &Config, step: &Step, vars: &HashMap<String, String>) -> Result<String> {
    match step.kind.as_str() {
        "pup" => execute_pup(cfg, step, vars).await,
        "shell" => execute_shell(step, vars).await,
        "datadog-workflow" => execute_datadog_workflow(cfg, step, vars).await,
        "confirm" => execute_confirm(cfg, step, vars),
        "http" => execute_http(cfg, step, vars).await,
        other => anyhow::bail!(
            "unknown step kind '{}' (expected pup, shell, datadog-workflow, confirm, http)",
            other
        ),
    }
}

async fn execute_pup(cfg: &Config, step: &Step, vars: &HashMap<String, String>) -> Result<String> {
    let run = step
        .run
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("pup step '{}' missing 'run' field", step.name))?;

    let rendered = template::render(run, vars);
    let exe = std::env::current_exe()
        .map_err(|e| anyhow::anyhow!("cannot find current executable: {e}"))?;
    let parts: Vec<String> = rendered.split_whitespace().map(String::from).collect();

    if let Some(poll) = &step.poll {
        let interval = template::parse_duration(&poll.interval)?;
        let timeout = template::parse_duration(&poll.timeout)?;
        let until = poll.until.clone();
        let start = Instant::now();
        let mut baseline: Option<f64> = None;

        loop {
            if start.elapsed() >= timeout {
                anyhow::bail!("poll timeout after {}s", timeout.as_secs());
            }

            let out = tokio::process::Command::new(&exe)
                .args(&parts)
                .args(["--output", "json"])
                .output()
                .await
                .map_err(|e| anyhow::anyhow!("failed to run pup: {e}"))?;

            let stdout = String::from_utf8_lossy(&out.stdout).into_owned();

            if out.status.success() && eval_condition(&stdout, &until, &mut baseline)? {
                print!("{stdout}");
                return Ok(stdout);
            }

            let elapsed = start.elapsed().as_secs();
            eprintln!("    ↻ polling ({elapsed}s elapsed)...");
            tokio::time::sleep(interval).await;
        }
    } else {
        let out = tokio::process::Command::new(&exe)
            .args(&parts)
            .args(["--output", &cfg.output_format.to_string()])
            .output()
            .await
            .map_err(|e| anyhow::anyhow!("failed to run pup: {e}"))?;

        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            anyhow::bail!("pup command failed: {}", stderr.trim());
        }

        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        print!("{stdout}");
        Ok(stdout)
    }
}

async fn execute_shell(step: &Step, vars: &HashMap<String, String>) -> Result<String> {
    let run = step
        .run
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("shell step '{}' missing 'run' field", step.name))?;

    let rendered = template::render(run, vars);
    let optional = step.optional.unwrap_or(false);

    let result = tokio::process::Command::new("sh")
        .args(["-c", &rendered])
        .output()
        .await;

    match result {
        Err(e) if optional => {
            eprintln!("    (skipped — command not found: {e})");
            Ok(String::new())
        }
        Err(e) => anyhow::bail!("failed to run shell command: {e}"),
        Ok(out) => {
            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr);
                anyhow::bail!("shell command failed: {}", stderr.trim());
            }
            let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
            print!("{stdout}");
            Ok(stdout)
        }
    }
}

async fn execute_datadog_workflow(
    cfg: &Config,
    step: &Step,
    vars: &HashMap<String, String>,
) -> Result<String> {
    let workflow_id = step.workflow_id.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "datadog-workflow step '{}' missing 'workflow_id'",
            step.name
        )
    })?;

    let workflow_id = template::render(workflow_id, vars);

    // Build inputs payload
    let mut inputs_map = serde_json::Map::new();
    if let Some(inputs) = &step.inputs {
        for (k, v) in inputs {
            let rendered_v = template::render(v, vars);
            inputs_map.insert(k.clone(), serde_json::Value::String(rendered_v));
        }
    }
    let body = serde_json::json!({ "meta": { "payload": inputs_map } });

    // Trigger the workflow
    let path = format!("/api/v2/workflows/{workflow_id}/instances");
    let trigger_resp = crate::client::raw_post(cfg, &path, body)
        .await
        .map_err(|e| anyhow::anyhow!("failed to trigger workflow: {e}"))?;

    let instance_id = trigger_resp
        .pointer("/data/id")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_default();

    // Emit agent-mode metadata hint
    if cfg.agent_mode || instance_id.is_empty() {
        let meta = serde_json::json!({
            "metadata": {
                "kind": "datadog-workflow",
                "workflow_id": workflow_id,
                "instance_id": instance_id,
                "watch_command": format!(
                    "pup workflows instances get --workflow-id={workflow_id} --instance-id={instance_id}"
                )
            }
        });
        println!("{}", serde_json::to_string_pretty(&meta).unwrap());
    }

    if instance_id.is_empty() {
        return Ok(trigger_resp.to_string());
    }

    // Auto-poll until terminal state (15s interval, up to the step's poll timeout or 10 min)
    let poll_timeout = if let Some(poll) = &step.poll {
        template::parse_duration(&poll.timeout)?
    } else {
        std::time::Duration::from_secs(600) // 10-minute default
    };

    let start = Instant::now();
    loop {
        if start.elapsed() >= poll_timeout {
            anyhow::bail!("workflow poll timeout after {}s", poll_timeout.as_secs());
        }

        let elapsed = start.elapsed().as_secs();
        eprintln!("    ↻ [{elapsed}s elapsed] checking workflow instance status...");
        tokio::time::sleep(std::time::Duration::from_secs(15)).await;

        let status_path = format!("/api/v2/workflows/{workflow_id}/instances/{instance_id}");
        let status_resp = crate::client::raw_get(cfg, &status_path)
            .await
            .map_err(|e| anyhow::anyhow!("failed to poll workflow: {e}"))?;

        let state = status_resp
            .pointer("/data/attributes/status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        eprintln!("    status: {state}");

        match state {
            "success" => {
                let out = status_resp.to_string();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&status_resp).unwrap_or_default()
                );
                return Ok(out);
            }
            "failed" | "error" => {
                anyhow::bail!("workflow instance {instance_id} ended with status: {state}");
            }
            _ => {}
        }
    }
}

fn execute_confirm(cfg: &Config, step: &Step, vars: &HashMap<String, String>) -> Result<String> {
    let message = step
        .message
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("confirm step '{}' missing 'message' field", step.name))?;

    let rendered = template::render(message, vars);

    if cfg.auto_approve {
        eprintln!("    {rendered} [auto-approved]");
        return Ok(String::new());
    }

    eprint!("    {rendered} [y/N] ");
    io::stderr().flush().ok();

    let stdin = io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line)?;

    if matches!(line.trim().to_lowercase().as_str(), "y" | "yes") {
        Ok(String::new())
    } else {
        anyhow::bail!("user declined at confirm step")
    }
}

async fn execute_http(cfg: &Config, step: &Step, vars: &HashMap<String, String>) -> Result<String> {
    let url = step
        .url
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("http step '{}' missing 'url' field", step.name))?;

    let rendered_url = template::render(url, vars);
    let method = step.method.as_deref().unwrap_or("GET").to_uppercase();

    let resp = if method == "GET" {
        // Use raw_get only for paths under the configured API base
        if rendered_url.starts_with('/') {
            crate::client::raw_get(cfg, &rendered_url).await?
        } else {
            // Absolute URL — use reqwest directly
            reqwest::Client::new()
                .get(&rendered_url)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("HTTP GET failed: {e}"))?
                .json::<serde_json::Value>()
                .await
                .map_err(|e| anyhow::anyhow!("failed to parse response: {e}"))?
        }
    } else {
        let body = serde_json::json!({});
        if rendered_url.starts_with('/') {
            crate::client::raw_post(cfg, &rendered_url, body).await?
        } else {
            reqwest::Client::new()
                .post(&rendered_url)
                .json(&body)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("HTTP POST failed: {e}"))?
                .json::<serde_json::Value>()
                .await
                .map_err(|e| anyhow::anyhow!("failed to parse response: {e}"))?
        }
    };

    let out = serde_json::to_string_pretty(&resp).unwrap_or_default();
    println!("{out}");
    Ok(out)
}

/// Evaluate a poll condition against JSON output.
/// Supported: "empty", "status == <val>", "value < N", "decreasing"
fn eval_condition(output: &str, condition: &str, baseline: &mut Option<f64>) -> Result<bool> {
    let condition = condition.trim();

    if condition == "empty" {
        let v: serde_json::Value = serde_json::from_str(output).unwrap_or(serde_json::Value::Null);
        return Ok(match &v {
            serde_json::Value::Array(arr) => arr.is_empty(),
            serde_json::Value::Null => true,
            serde_json::Value::String(s) => s.is_empty(),
            _ => false,
        });
    }

    if condition == "decreasing" {
        let v: serde_json::Value = serde_json::from_str(output).unwrap_or(serde_json::Value::Null);
        let current = extract_numeric(&v);
        if let Some(current) = current {
            let result = if let Some(b) = *baseline {
                current < b
            } else {
                false
            };
            *baseline = Some(current);
            return Ok(result);
        }
        return Ok(false);
    }

    // "status == OK"
    if let Some(rest) = condition.strip_prefix("status ==") {
        let expected = rest.trim().trim_matches('"');
        let v: serde_json::Value = serde_json::from_str(output).unwrap_or(serde_json::Value::Null);
        let status = v.get("status").and_then(|s| s.as_str()).unwrap_or("");
        return Ok(status == expected);
    }

    // "value < N"
    if let Some(rest) = condition.strip_prefix("value <") {
        let threshold: f64 = rest
            .trim()
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid threshold in condition: {condition}"))?;
        let v: serde_json::Value = serde_json::from_str(output).unwrap_or(serde_json::Value::Null);
        if let Some(n) = extract_numeric(&v) {
            return Ok(n < threshold);
        }
        return Ok(false);
    }

    // Unrecognized condition — treat as "always true" so polling continues once
    Ok(true)
}

fn extract_numeric(v: &serde_json::Value) -> Option<f64> {
    match v {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::Object(map) => map.get("value").and_then(|v| v.as_f64()),
        _ => None,
    }
}

/// Prompt the user to continue after a failure.
fn prompt_continue(cfg: &Config) -> Result<bool> {
    if cfg.auto_approve {
        return Ok(true);
    }
    eprint!("  Continue despite failure? [y/N] ");
    io::stderr().flush().ok();
    let stdin = io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line)?;
    Ok(matches!(line.trim().to_lowercase().as_str(), "y" | "yes"))
}
