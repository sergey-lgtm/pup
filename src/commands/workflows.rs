use anyhow::Result;
use std::collections::BTreeMap;

#[cfg(not(target_arch = "wasm32"))]
use datadog_api_client::datadogV2::api_workflow_automation::{
    ListWorkflowInstancesOptionalParams, WorkflowAutomationAPI,
};

#[cfg(not(target_arch = "wasm32"))]
use crate::client;
use crate::config::Config;
use crate::formatter::{self, Metadata};
use crate::util;

// ---------------------------------------------------------------------------
// Helper: build a WorkflowAutomationAPI with bearer-token support
// ---------------------------------------------------------------------------

#[cfg(not(target_arch = "wasm32"))]
fn make_api(cfg: &Config) -> WorkflowAutomationAPI {
    let dd_cfg = client::make_dd_config(cfg);
    match client::make_bearer_client(cfg) {
        Some(c) => WorkflowAutomationAPI::with_client_and_config(dd_cfg, c),
        None => WorkflowAutomationAPI::with_config(dd_cfg),
    }
}

// ---------------------------------------------------------------------------
// Workflow CRUD
// ---------------------------------------------------------------------------

#[cfg(not(target_arch = "wasm32"))]
pub async fn get(cfg: &Config, workflow_id: &str) -> Result<()> {
    let api = make_api(cfg);
    let resp = api
        .get_workflow(workflow_id.to_string())
        .await
        .map_err(|e| anyhow::anyhow!("failed to get workflow: {:?}", e))?;
    formatter::output(cfg, &resp)
}

#[cfg(target_arch = "wasm32")]
pub async fn get(cfg: &Config, workflow_id: &str) -> Result<()> {
    let data = crate::api::get(cfg, &format!("/api/v2/workflows/{workflow_id}"), &[]).await?;
    crate::formatter::output(cfg, &data)
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn create(cfg: &Config, file: &str) -> Result<()> {
    let body: datadog_api_client::datadogV2::model::CreateWorkflowRequest =
        util::read_json_file(file)?;
    let api = make_api(cfg);
    let resp = api
        .create_workflow(body)
        .await
        .map_err(|e| anyhow::anyhow!("failed to create workflow: {:?}", e))?;
    formatter::output(cfg, &resp)
}

#[cfg(target_arch = "wasm32")]
pub async fn create(cfg: &Config, file: &str) -> Result<()> {
    let body: serde_json::Value = util::read_json_file(file)?;
    let data = crate::api::post(cfg, "/api/v2/workflows", &body).await?;
    crate::formatter::output(cfg, &data)
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn update(cfg: &Config, workflow_id: &str, file: &str) -> Result<()> {
    let body: datadog_api_client::datadogV2::model::UpdateWorkflowRequest =
        util::read_json_file(file)?;
    let api = make_api(cfg);
    let resp = api
        .update_workflow(workflow_id.to_string(), body)
        .await
        .map_err(|e| anyhow::anyhow!("failed to update workflow: {:?}", e))?;
    formatter::output(cfg, &resp)
}

#[cfg(target_arch = "wasm32")]
pub async fn update(cfg: &Config, workflow_id: &str, file: &str) -> Result<()> {
    let body: serde_json::Value = util::read_json_file(file)?;
    let data = crate::api::patch(cfg, &format!("/api/v2/workflows/{workflow_id}"), &body).await?;
    crate::formatter::output(cfg, &data)
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn delete(cfg: &Config, workflow_id: &str) -> Result<()> {
    let api = make_api(cfg);
    api.delete_workflow(workflow_id.to_string())
        .await
        .map_err(|e| anyhow::anyhow!("failed to delete workflow: {:?}", e))?;
    eprintln!("Workflow {workflow_id} deleted.");
    Ok(())
}

#[cfg(target_arch = "wasm32")]
pub async fn delete(cfg: &Config, workflow_id: &str) -> Result<()> {
    crate::api::delete(cfg, &format!("/api/v2/workflows/{workflow_id}")).await?;
    eprintln!("Workflow {workflow_id} deleted.");
    Ok(())
}

// ---------------------------------------------------------------------------
// Workflow execution (API trigger only — requires DD_API_KEY + DD_APP_KEY)
// ---------------------------------------------------------------------------

#[cfg(not(target_arch = "wasm32"))]
pub async fn run(
    cfg: &Config,
    workflow_id: &str,
    payload: Option<String>,
    payload_file: Option<String>,
    wait: bool,
    timeout: &str,
) -> Result<()> {
    cfg.validate_api_and_app_keys()?;

    let dd_cfg = client::make_dd_config(cfg);
    let api = WorkflowAutomationAPI::with_config(dd_cfg);

    let input_payload: Option<BTreeMap<String, serde_json::Value>> = match (&payload, &payload_file)
    {
        (Some(_), Some(_)) => {
            return Err(anyhow::anyhow!(
                "cannot specify both --payload and --payload-file"
            ))
        }
        (Some(json_str), None) => Some(serde_json::from_str(json_str)?),
        (None, Some(path)) => {
            let contents = std::fs::read_to_string(path)
                .map_err(|e| anyhow::anyhow!("failed to read payload file {path:?}: {e}"))?;
            Some(serde_json::from_str(&contents)?)
        }
        (None, None) => None,
    };

    let mut request = datadog_api_client::datadogV2::model::WorkflowInstanceCreateRequest::new();
    if let Some(p) = input_payload {
        request = request.meta(
            datadog_api_client::datadogV2::model::WorkflowInstanceCreateMeta::new().payload(p),
        );
    }

    let response = api
        .create_workflow_instance(workflow_id.to_string(), request)
        .await
        .map_err(|e| anyhow::anyhow!("failed to execute workflow: {:?}", e))?;

    if !wait {
        formatter::output(cfg, &response)?;
        return Ok(());
    }

    let instance_id = response
        .data
        .as_ref()
        .and_then(|d| d.id.as_ref())
        .ok_or_else(|| anyhow::anyhow!("no instance ID in response"))?
        .clone();

    eprintln!("Instance {instance_id} started, waiting for completion...");

    let timeout_duration = parse_duration(timeout)?;
    let start = std::time::Instant::now();

    loop {
        if start.elapsed() > timeout_duration {
            return Err(anyhow::anyhow!(
                "timed out after {} waiting for instance {instance_id}",
                timeout
            ));
        }

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        let dd_cfg = client::make_dd_config(cfg);
        let api = WorkflowAutomationAPI::with_config(dd_cfg);
        let status = api
            .get_workflow_instance(workflow_id.to_string(), instance_id.clone())
            .await
            .map_err(|e| anyhow::anyhow!("failed to get instance status: {:?}", e))?;

        let state = status
            .data
            .as_ref()
            .and_then(|d| d.attributes.as_ref())
            .and_then(|a| a.additional_properties.get("status"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        match state {
            "COMPLETED" | "FAILED" | "CANCELED" | "CANCELLED" | "ERROR" => {
                formatter::output(cfg, &status)?;
                return Ok(());
            }
            _ => continue,
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub async fn run(
    cfg: &Config,
    workflow_id: &str,
    payload: Option<String>,
    payload_file: Option<String>,
    _wait: bool,
    _timeout: &str,
) -> Result<()> {
    let body: serde_json::Value = match (&payload, &payload_file) {
        (Some(_), Some(_)) => {
            return Err(anyhow::anyhow!(
                "cannot specify both --payload and --payload-file"
            ))
        }
        (Some(json_str), None) => {
            let p: serde_json::Value = serde_json::from_str(json_str)?;
            serde_json::json!({ "meta": { "payload": p } })
        }
        (None, Some(path)) => {
            let contents = std::fs::read_to_string(path)?;
            let p: serde_json::Value = serde_json::from_str(&contents)?;
            serde_json::json!({ "meta": { "payload": p } })
        }
        (None, None) => serde_json::json!({}),
    };
    let data = crate::api::post(
        cfg,
        &format!("/api/v2/workflows/{workflow_id}/instances"),
        &body,
    )
    .await?;
    crate::formatter::output(cfg, &data)
}

// ---------------------------------------------------------------------------
// Workflow instances
// ---------------------------------------------------------------------------

#[cfg(not(target_arch = "wasm32"))]
pub async fn instance_list(cfg: &Config, workflow_id: &str, limit: i64, page: i64) -> Result<()> {
    let api = make_api(cfg);

    let mut params = ListWorkflowInstancesOptionalParams::default();
    params = params.page_size(limit.clamp(1, 100));
    if page > 0 {
        params = params.page_number(page);
    }

    let resp = api
        .list_workflow_instances(workflow_id.to_string(), params)
        .await
        .map_err(|e| anyhow::anyhow!("failed to list workflow instances: {:?}", e))?;

    let count = resp.data.as_ref().map(|d| d.len());
    let meta = Metadata {
        count,
        truncated: false,
        command: Some("workflows instances list".to_string()),
        next_action: None,
    };
    formatter::format_and_print(&resp, &cfg.output_format, cfg.agent_mode, Some(&meta))
}

#[cfg(target_arch = "wasm32")]
pub async fn instance_list(cfg: &Config, workflow_id: &str, limit: i64, page: i64) -> Result<()> {
    let mut query = vec![("page[size]", limit.clamp(1, 100).to_string())];
    if page > 0 {
        query.push(("page[number]", page.to_string()));
    }
    let q: Vec<(&str, String)> = query;
    let data = crate::api::get(
        cfg,
        &format!("/api/v2/workflows/{workflow_id}/instances"),
        &q,
    )
    .await?;
    crate::formatter::output(cfg, &data)
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn instance_get(cfg: &Config, workflow_id: &str, instance_id: &str) -> Result<()> {
    let api = make_api(cfg);
    let resp = api
        .get_workflow_instance(workflow_id.to_string(), instance_id.to_string())
        .await
        .map_err(|e| anyhow::anyhow!("failed to get workflow instance: {:?}", e))?;
    formatter::output(cfg, &resp)
}

#[cfg(target_arch = "wasm32")]
pub async fn instance_get(cfg: &Config, workflow_id: &str, instance_id: &str) -> Result<()> {
    let data = crate::api::get(
        cfg,
        &format!("/api/v2/workflows/{workflow_id}/instances/{instance_id}"),
        &[],
    )
    .await?;
    crate::formatter::output(cfg, &data)
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn instance_cancel(cfg: &Config, workflow_id: &str, instance_id: &str) -> Result<()> {
    let api = make_api(cfg);
    let resp = api
        .cancel_workflow_instance(workflow_id.to_string(), instance_id.to_string())
        .await
        .map_err(|e| anyhow::anyhow!("failed to cancel workflow instance: {:?}", e))?;
    formatter::output(cfg, &resp)
}

#[cfg(target_arch = "wasm32")]
pub async fn instance_cancel(cfg: &Config, workflow_id: &str, instance_id: &str) -> Result<()> {
    let data = crate::api::put(
        cfg,
        &format!("/api/v2/workflows/{workflow_id}/instances/{instance_id}/cancel"),
        &serde_json::json!({}),
    )
    .await?;
    crate::formatter::output(cfg, &data)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parse_duration(input: &str) -> Result<std::time::Duration> {
    use std::sync::LazyLock;
    static RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(?i)^(\d+)\s*(s|m|h)$").unwrap());

    let input = input.trim();
    if let Some(caps) = RE.captures(input) {
        let num: u64 = caps[1].parse()?;
        let secs = match caps[2].to_lowercase().as_str() {
            "s" => num,
            "m" => num * 60,
            "h" => num * 3600,
            _ => return Err(anyhow::anyhow!("unknown duration unit")),
        };
        Ok(std::time::Duration::from_secs(secs))
    } else {
        Err(anyhow::anyhow!(
            "invalid duration: {input:?} (expected e.g. 30s, 5m, 1h)"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_duration_seconds() {
        let d = parse_duration("30s").unwrap();
        assert_eq!(d, std::time::Duration::from_secs(30));
    }

    #[test]
    fn test_parse_duration_minutes() {
        let d = parse_duration("5m").unwrap();
        assert_eq!(d, std::time::Duration::from_secs(300));
    }

    #[test]
    fn test_parse_duration_hours() {
        let d = parse_duration("1h").unwrap();
        assert_eq!(d, std::time::Duration::from_secs(3600));
    }

    #[test]
    fn test_parse_duration_case_insensitive() {
        let d = parse_duration("2M").unwrap();
        assert_eq!(d, std::time::Duration::from_secs(120));
    }

    #[test]
    fn test_parse_duration_whitespace_trimmed() {
        let d = parse_duration("  10s  ").unwrap();
        assert_eq!(d, std::time::Duration::from_secs(10));
    }

    #[test]
    fn test_parse_duration_invalid_unit() {
        assert!(parse_duration("5x").is_err());
    }

    #[test]
    fn test_parse_duration_no_unit() {
        assert!(parse_duration("100").is_err());
    }

    #[test]
    fn test_parse_duration_empty() {
        assert!(parse_duration("").is_err());
    }

    #[test]
    fn test_parse_duration_garbage() {
        assert!(parse_duration("abc").is_err());
    }
}
