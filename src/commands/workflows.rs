use anyhow::Result;
use std::time::Instant;

use crate::client;
use crate::config::Config;
use crate::formatter;

/// Trigger a Datadog Workflow and optionally watch it to completion.
pub async fn run(cfg: &Config, workflow_id: &str, inputs: Vec<String>, watch: bool) -> Result<()> {
    // Parse --input key=value pairs
    let mut inputs_map = serde_json::Map::new();
    for kv in &inputs {
        let mut parts = kv.splitn(2, '=');
        let key = parts
            .next()
            .ok_or_else(|| anyhow::anyhow!("invalid --input value '{kv}': expected key=value"))?;
        let val = parts
            .next()
            .ok_or_else(|| anyhow::anyhow!("invalid --input value '{kv}': expected key=value"))?;
        inputs_map.insert(key.to_string(), serde_json::Value::String(val.to_string()));
    }

    let body = serde_json::json!({ "meta": { "payload": inputs_map } });
    let path = format!("/api/v2/workflows/{workflow_id}/instances");

    let trigger_resp = client::raw_post(cfg, &path, body)
        .await
        .map_err(|e| anyhow::anyhow!("failed to trigger workflow: {e}"))?;

    let instance_id = trigger_resp
        .pointer("/data/id")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_default();

    if !watch || instance_id.is_empty() {
        // Emit trigger response with watch hint in agent mode
        if cfg.agent_mode && !instance_id.is_empty() {
            let result = serde_json::json!({
                "data": trigger_resp.get("data"),
                "metadata": {
                    "watch_command": format!(
                        "pup workflows instances get --workflow-id={workflow_id} --instance-id={instance_id}"
                    )
                }
            });
            formatter::output(cfg, &result)?;
        } else {
            formatter::output(cfg, &trigger_resp)?;
        }
        return Ok(());
    }

    // --watch: poll every 15s until terminal state
    eprintln!("Triggered workflow {workflow_id} → instance {instance_id}");
    eprintln!("Watching for completion...");

    let start = Instant::now();
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(15)).await;

        let elapsed = start.elapsed().as_secs();
        let status_path = format!("/api/v2/workflows/{workflow_id}/instances/{instance_id}");
        let status_resp = client::raw_get(cfg, &status_path)
            .await
            .map_err(|e| anyhow::anyhow!("failed to poll workflow: {e}"))?;

        let state = status_resp
            .pointer("/data/attributes/status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        eprintln!("↻ [{elapsed}s elapsed] status: {state}");

        match state {
            "success" => {
                formatter::output(cfg, &status_resp)?;
                return Ok(());
            }
            "failed" | "error" => {
                formatter::output(cfg, &status_resp)?;
                anyhow::bail!("workflow instance {instance_id} ended with status: {state}");
            }
            _ => {}
        }
    }
}

/// List workflow instances for a given workflow.
pub async fn instances_list(cfg: &Config, workflow_id: &str) -> Result<()> {
    let path = format!("/api/v2/workflows/{workflow_id}/instances");
    let resp = client::raw_get(cfg, &path)
        .await
        .map_err(|e| anyhow::anyhow!("failed to list workflow instances: {e}"))?;
    formatter::output(cfg, &resp)
}

/// Get a specific workflow instance.
pub async fn instances_get(cfg: &Config, workflow_id: &str, instance_id: &str) -> Result<()> {
    let path = format!("/api/v2/workflows/{workflow_id}/instances/{instance_id}");
    let resp = client::raw_get(cfg, &path)
        .await
        .map_err(|e| anyhow::anyhow!("failed to get workflow instance: {e}"))?;
    formatter::output(cfg, &resp)
}
