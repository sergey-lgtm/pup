use anyhow::Result;
#[cfg(not(target_arch = "wasm32"))]
use datadog_api_client::datadogV1::api_synthetics::{
    ListTestsOptionalParams, SearchTestsOptionalParams, SyntheticsAPI,
};
#[cfg(not(target_arch = "wasm32"))]
use datadog_api_client::datadogV2::api_synthetics::{
    SearchSuitesOptionalParams, SyntheticsAPI as SyntheticsV2API,
};
#[cfg(not(target_arch = "wasm32"))]
use datadog_api_client::datadogV2::model::{
    DeletedSuitesRequestDelete, DeletedSuitesRequestDeleteAttributes,
    DeletedSuitesRequestDeleteRequest, SuiteCreateEditRequest,
};

#[cfg(not(target_arch = "wasm32"))]
use crate::client;
use crate::config::Config;
use crate::formatter;

#[cfg(not(target_arch = "wasm32"))]
fn synthetics_intake_base_url(cfg: &Config) -> String {
    if cfg.site == "datadoghq.com" || cfg.site == "datad0g.com" {
        format!("https://intake.synthetics.{}/api/v1", cfg.site)
    } else {
        format!("{}/api/v1", cfg.api_base_url())
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn build_auth_headers(cfg: &Config) -> anyhow::Result<reqwest::header::HeaderMap> {
    use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
    let api_key = cfg
        .api_key
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("DD_API_KEY is required for 'synthetics tests run'"))?;
    let app_key = cfg
        .app_key
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("DD_APP_KEY is required for 'synthetics tests run'"))?;
    let mut headers = HeaderMap::new();
    headers.insert(
        HeaderName::from_static("dd-api-key"),
        HeaderValue::from_str(api_key)?,
    );
    headers.insert(
        HeaderName::from_static("dd-application-key"),
        HeaderValue::from_str(app_key)?,
    );
    Ok(headers)
}

const POLL_INTERVAL_SECS: u64 = 5;

#[cfg(not(target_arch = "wasm32"))]
pub async fn tests_run(
    cfg: &Config,
    public_ids: Vec<String>,
    use_tunnel: bool,
    timeout_secs: u64,
) -> Result<()> {
    if public_ids.is_empty() {
        anyhow::bail!("at least one public ID is required");
    }

    let auth_headers = build_auth_headers(cfg)?;
    let intake_url = synthetics_intake_base_url(cfg);
    let client = reqwest::Client::new();

    let active_tunnel = if use_tunnel {
        eprintln!(
            "Fetching tunnel presigned URL for {} test(s)...",
            public_ids.len()
        );
        let query: Vec<(&str, &str)> = public_ids
            .iter()
            .map(|id| ("test_id", id.as_str()))
            .collect();
        let tunnel_resp = client
            .get(format!("{intake_url}/synthetics/ci/tunnel"))
            .query(&query)
            .headers(auth_headers.clone())
            .send()
            .await?;
        if !tunnel_resp.status().is_success() {
            let status = tunnel_resp.status();
            let body = tunnel_resp.text().await.unwrap_or_default();
            anyhow::bail!("failed to get tunnel URL (HTTP {status}): {body}");
        }
        let tunnel_json: serde_json::Value = tunnel_resp.json().await?;
        let presigned_url = tunnel_json["url"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'url' in tunnel response"))?
            .to_string();

        eprintln!("Starting tunnel...");
        let (tunnel_info, tunnel) =
            crate::tunnel::Tunnel::start(&presigned_url, public_ids.clone()).await?;
        eprintln!("Tunnel connected (id: {})", tunnel_info.id);
        Some((tunnel_info, tunnel))
    } else {
        None
    };

    let tests_payload: Vec<serde_json::Value> = public_ids
        .iter()
        .map(|id| {
            let mut test = serde_json::json!({ "public_id": id });
            if let Some((ref info, _)) = active_tunnel {
                test["tunnel"] = serde_json::json!({
                    "id": info.id,
                    "host": info.host,
                    "privateKey": info.private_key,
                });
            }
            test
        })
        .collect();

    let trigger_payload = serde_json::json!({ "tests": tests_payload });

    let agent = crate::useragent::detect_agent_info();
    let trigger_app = if agent.detected {
        format!("pup_cli/{}", agent.name) // e.g. `pup_cli/claude-code`
    } else {
        "pup_cli".to_string()
    };

    eprintln!("Triggering {} test(s)...", public_ids.len());
    let trigger_resp = client
        .post(format!("{intake_url}/synthetics/tests/trigger/ci"))
        .headers(auth_headers.clone())
        .header("X-Trigger-App", trigger_app)
        .json(&trigger_payload)
        .send()
        .await?;
    if !trigger_resp.status().is_success() {
        let status = trigger_resp.status();
        let body = trigger_resp.text().await.unwrap_or_default();
        if let Some((_, tunnel)) = active_tunnel {
            tunnel.stop();
        }
        anyhow::bail!("failed to trigger tests (HTTP {status}): {body}");
    }
    let trigger_json: serde_json::Value = trigger_resp.json().await?;
    let batch_id = trigger_json["batch_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing 'batch_id' in trigger response"))?
        .to_string();
    eprintln!("Batch ID: {batch_id}");

    let poll_url = format!(
        "{}/api/v1/synthetics/ci/batch/{batch_id}",
        cfg.api_base_url()
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);

    let final_result = loop {
        tokio::time::sleep(std::time::Duration::from_secs(POLL_INTERVAL_SECS)).await;

        let batch_resp = client
            .get(&poll_url)
            .headers(auth_headers.clone())
            .send()
            .await?;
        if !batch_resp.status().is_success() {
            let status = batch_resp.status();
            let body = batch_resp.text().await.unwrap_or_default();
            if let Some((_, tunnel)) = active_tunnel {
                tunnel.stop();
            }
            anyhow::bail!("failed to poll batch (HTTP {status}): {body}");
        }
        let batch_json: serde_json::Value = batch_resp.json().await?;
        let status = batch_json["data"]["status"].as_str().unwrap_or("unknown");
        eprintln!("Batch status: {status}");

        if status != "in_progress" {
            break batch_json;
        }
        if std::time::Instant::now() >= deadline {
            if let Some((_, tunnel)) = active_tunnel {
                tunnel.stop();
            }
            anyhow::bail!("timeout after {timeout_secs}s waiting for test results");
        }
    };

    if let Some((_, tunnel)) = active_tunnel {
        tunnel.stop();
    }
    let results = &final_result["data"]["results"];
    if cfg.output_format == crate::config::OutputFormat::Table {
        let table_rows: Vec<serde_json::Value> = results
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|r| {
                        serde_json::json!({
                            "status": r["status"],
                            "test_name": r["test_name"],
                            "test_public_id": r["test_public_id"],
                            "location": r["location"],
                            "duration_ms": r["duration"],
                            "test_type": r["test_type"],
                            "execution_rule": r["execution_rule"],
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        formatter::output(cfg, &table_rows)
    } else {
        formatter::output(cfg, results)
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn tests_list(cfg: &Config) -> Result<()> {
    let dd_cfg = client::make_dd_config(cfg);
    let api = match client::make_bearer_client(cfg) {
        Some(c) => SyntheticsAPI::with_client_and_config(dd_cfg, c),
        None => SyntheticsAPI::with_config(dd_cfg),
    };
    let resp = api
        .list_tests(ListTestsOptionalParams::default())
        .await
        .map_err(|e| anyhow::anyhow!("failed to list tests: {e:?}"))?;
    formatter::output(cfg, &resp)
}

#[cfg(target_arch = "wasm32")]
pub async fn tests_list(cfg: &Config) -> Result<()> {
    let data = crate::api::get(cfg, "/api/v1/synthetics/tests", &[]).await?;
    crate::formatter::output(cfg, &data)
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn tests_get(cfg: &Config, public_id: &str) -> Result<()> {
    let dd_cfg = client::make_dd_config(cfg);
    let api = match client::make_bearer_client(cfg) {
        Some(c) => SyntheticsAPI::with_client_and_config(dd_cfg, c),
        None => SyntheticsAPI::with_config(dd_cfg),
    };
    let resp = api
        .get_test(public_id.to_string())
        .await
        .map_err(|e| anyhow::anyhow!("failed to get test: {e:?}"))?;
    formatter::output(cfg, &resp)
}

#[cfg(target_arch = "wasm32")]
pub async fn tests_get(cfg: &Config, public_id: &str) -> Result<()> {
    let path = format!("/api/v1/synthetics/tests/{public_id}");
    let data = crate::api::get(cfg, &path, &[]).await?;
    crate::formatter::output(cfg, &data)
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn tests_search(
    cfg: &Config,
    text: Option<String>,
    count: i64,
    start: i64,
) -> Result<()> {
    let dd_cfg = client::make_dd_config(cfg);
    let api = match client::make_bearer_client(cfg) {
        Some(c) => SyntheticsAPI::with_client_and_config(dd_cfg, c),
        None => SyntheticsAPI::with_config(dd_cfg),
    };

    let mut params = SearchTestsOptionalParams::default();
    if let Some(t) = text {
        params = params.text(t);
    }
    if count != 50 {
        params = params.count(count);
    }
    if start != 0 {
        params = params.start(start);
    }

    let resp = api
        .search_tests(params)
        .await
        .map_err(|e| anyhow::anyhow!("failed to search tests: {e:?}"))?;
    formatter::output(cfg, &resp)
}

#[cfg(target_arch = "wasm32")]
pub async fn tests_search(
    cfg: &Config,
    text: Option<String>,
    count: i64,
    start: i64,
) -> Result<()> {
    let mut query: Vec<(&str, String)> = Vec::new();
    if let Some(t) = text {
        query.push(("text", t));
    }
    if count != 50 {
        query.push(("count", count.to_string()));
    }
    if start != 0 {
        query.push(("start", start.to_string()));
    }
    let data = crate::api::get(cfg, "/api/v1/synthetics/tests/search", &query).await?;
    crate::formatter::output(cfg, &data)
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn locations_list(cfg: &Config) -> Result<()> {
    let dd_cfg = client::make_dd_config(cfg);
    let api = match client::make_bearer_client(cfg) {
        Some(c) => SyntheticsAPI::with_client_and_config(dd_cfg, c),
        None => SyntheticsAPI::with_config(dd_cfg),
    };
    let resp = api
        .list_locations()
        .await
        .map_err(|e| anyhow::anyhow!("failed to list locations: {e:?}"))?;
    formatter::output(cfg, &resp)
}

#[cfg(target_arch = "wasm32")]
pub async fn locations_list(cfg: &Config) -> Result<()> {
    let data = crate::api::get(cfg, "/api/v1/synthetics/locations", &[]).await?;
    crate::formatter::output(cfg, &data)
}

// ---- Suites (V2 API) ----

#[cfg(not(target_arch = "wasm32"))]
pub async fn suites_list(cfg: &Config, query: Option<String>) -> Result<()> {
    let dd_cfg = client::make_dd_config(cfg);
    let api = match client::make_bearer_client(cfg) {
        Some(c) => SyntheticsV2API::with_client_and_config(dd_cfg, c),
        None => SyntheticsV2API::with_config(dd_cfg),
    };
    let mut params = SearchSuitesOptionalParams::default();
    if let Some(q) = query {
        params = params.query(q);
    }
    let resp = api
        .search_suites(params)
        .await
        .map_err(|e| anyhow::anyhow!("failed to list synthetic suites: {e:?}"))?;
    formatter::output(cfg, &resp)
}

#[cfg(target_arch = "wasm32")]
pub async fn suites_list(cfg: &Config, query: Option<String>) -> Result<()> {
    let mut q: Vec<(&str, String)> = Vec::new();
    if let Some(qv) = query {
        q.push(("query", qv));
    }
    let data = crate::api::get(cfg, "/api/v2/synthetics/suites", &q).await?;
    crate::formatter::output(cfg, &data)
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn suites_get(cfg: &Config, suite_id: &str) -> Result<()> {
    let dd_cfg = client::make_dd_config(cfg);
    let api = match client::make_bearer_client(cfg) {
        Some(c) => SyntheticsV2API::with_client_and_config(dd_cfg, c),
        None => SyntheticsV2API::with_config(dd_cfg),
    };
    let resp = api
        .get_synthetics_suite(suite_id.to_string())
        .await
        .map_err(|e| anyhow::anyhow!("failed to get synthetic suite: {e:?}"))?;
    formatter::output(cfg, &resp)
}

#[cfg(target_arch = "wasm32")]
pub async fn suites_get(cfg: &Config, suite_id: &str) -> Result<()> {
    let path = format!("/api/v2/synthetics/suites/{suite_id}");
    let data = crate::api::get(cfg, &path, &[]).await?;
    crate::formatter::output(cfg, &data)
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn suites_create(cfg: &Config, file: &str) -> Result<()> {
    let dd_cfg = client::make_dd_config(cfg);
    let api = match client::make_bearer_client(cfg) {
        Some(c) => SyntheticsV2API::with_client_and_config(dd_cfg, c),
        None => SyntheticsV2API::with_config(dd_cfg),
    };
    let body: SuiteCreateEditRequest = crate::util::read_json_file(file)?;
    let resp = api
        .create_synthetics_suite(body)
        .await
        .map_err(|e| anyhow::anyhow!("failed to create synthetic suite: {e:?}"))?;
    formatter::output(cfg, &resp)
}

#[cfg(target_arch = "wasm32")]
pub async fn suites_create(cfg: &Config, file: &str) -> Result<()> {
    let body: serde_json::Value = crate::util::read_json_file(file)?;
    let data = crate::api::post(cfg, "/api/v2/synthetics/suites", &body).await?;
    crate::formatter::output(cfg, &data)
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn suites_update(cfg: &Config, suite_id: &str, file: &str) -> Result<()> {
    let dd_cfg = client::make_dd_config(cfg);
    let api = match client::make_bearer_client(cfg) {
        Some(c) => SyntheticsV2API::with_client_and_config(dd_cfg, c),
        None => SyntheticsV2API::with_config(dd_cfg),
    };
    let body: SuiteCreateEditRequest = crate::util::read_json_file(file)?;
    let resp = api
        .edit_synthetics_suite(suite_id.to_string(), body)
        .await
        .map_err(|e| anyhow::anyhow!("failed to update synthetic suite: {e:?}"))?;
    formatter::output(cfg, &resp)
}

#[cfg(target_arch = "wasm32")]
pub async fn suites_update(cfg: &Config, suite_id: &str, file: &str) -> Result<()> {
    let body: serde_json::Value = crate::util::read_json_file(file)?;
    let path = format!("/api/v2/synthetics/suites/{suite_id}");
    let data = crate::api::put(cfg, &path, &body).await?;
    crate::formatter::output(cfg, &data)
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn suites_delete(cfg: &Config, suite_ids: Vec<String>) -> Result<()> {
    let dd_cfg = client::make_dd_config(cfg);
    let api = match client::make_bearer_client(cfg) {
        Some(c) => SyntheticsV2API::with_client_and_config(dd_cfg, c),
        None => SyntheticsV2API::with_config(dd_cfg),
    };
    let attrs = DeletedSuitesRequestDeleteAttributes::new(suite_ids);
    let data = DeletedSuitesRequestDelete::new(attrs);
    let body = DeletedSuitesRequestDeleteRequest::new(data);
    let resp = api
        .delete_synthetics_suites(body)
        .await
        .map_err(|e| anyhow::anyhow!("failed to delete synthetic suites: {e:?}"))?;
    formatter::output(cfg, &resp)
}

#[cfg(target_arch = "wasm32")]
pub async fn suites_delete(cfg: &Config, suite_ids: Vec<String>) -> Result<()> {
    let body = serde_json::json!({
        "data": {
            "attributes": {
                "suite_ids": suite_ids
            }
        }
    });
    let data = crate::api::post(cfg, "/api/v2/synthetics/suites/delete", &body).await?;
    crate::formatter::output(cfg, &data)
}
