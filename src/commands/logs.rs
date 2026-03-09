use anyhow::{bail, Result};
#[cfg(not(target_arch = "wasm32"))]
use datadog_api_client::datadogV2::api_logs::{ListLogsOptionalParams, LogsAPI};
#[cfg(not(target_arch = "wasm32"))]
use datadog_api_client::datadogV2::api_logs_archives::LogsArchivesAPI;
#[cfg(not(target_arch = "wasm32"))]
use datadog_api_client::datadogV2::api_logs_custom_destinations::LogsCustomDestinationsAPI;
#[cfg(not(target_arch = "wasm32"))]
use datadog_api_client::datadogV2::api_logs_metrics::LogsMetricsAPI;
#[cfg(not(target_arch = "wasm32"))]
use datadog_api_client::datadogV2::model::{
    LogsListRequest, LogsListRequestPage, LogsQueryFilter, LogsSort, LogsStorageTier,
};

#[cfg(not(target_arch = "wasm32"))]
use crate::client;
use crate::config::Config;
use crate::formatter;
use crate::util;

pub struct AggregateArgs {
    pub query: String,
    pub from: String,
    pub to: String,
    pub compute: String,
    pub group_by: Option<String>,
    pub limit: i32,
    pub storage: Option<String>,
}

fn normalize_storage_tier(storage: Option<String>) -> Result<Option<String>> {
    match storage {
        None => Ok(None),
        Some(s) => match s.to_lowercase().as_str() {
            "indexes" => Ok(Some("indexes".into())),
            "online-archives" | "online_archives" => Ok(Some("online-archives".into())),
            "flex" => Ok(Some("flex".into())),
            other => anyhow::bail!(
                "unknown storage tier {:?}; valid values are: indexes, online-archives, flex",
                other
            ),
        },
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn parse_storage_tier(storage: Option<String>) -> Result<Option<LogsStorageTier>> {
    match normalize_storage_tier(storage)? {
        None => Ok(None),
        Some(tier) => match tier.as_str() {
            "indexes" => Ok(Some(LogsStorageTier::INDEXES)),
            "online-archives" => Ok(Some(LogsStorageTier::ONLINE_ARCHIVES)),
            "flex" => Ok(Some(LogsStorageTier::FLEX)),
            _ => unreachable!("storage tier is normalized"),
        },
    }
}

fn parse_compute_raw(input: &str) -> Result<(String, Option<String>)> {
    let input = input.trim();
    if input.is_empty() {
        bail!("--compute is required");
    }

    if input == "count" {
        return Ok(("count".into(), None));
    }

    if let Some(paren) = input.find('(') {
        let func = &input[..paren];
        let rest = input[paren + 1..].trim_end_matches(')').trim();

        if func == "percentile" {
            let parts: Vec<&str> = rest.splitn(2, ',').collect();
            if parts.len() != 2 {
                bail!("percentile requires field and value: percentile(@duration, 99)");
            }
            let metric = parts[0].trim().to_string();
            let pct: u32 = parts[1]
                .trim()
                .parse()
                .map_err(|_| anyhow::anyhow!("invalid percentile value: {}", parts[1].trim()))?;
            let agg_name = match pct {
                75 => "pc75",
                90 => "pc90",
                95 => "pc95",
                98 => "pc98",
                99 => "pc99",
                _ => bail!("unsupported percentile: {pct} (supported: 75, 90, 95, 98, 99)"),
            };
            return Ok((agg_name.into(), Some(metric)));
        }

        let metric = rest.to_string();
        let agg_name = match func {
            "avg" | "sum" | "min" | "max" | "median" | "cardinality" => func.to_string(),
            "count" => bail!("count does not accept a field argument; use just 'count'"),
            _ => bail!("unknown aggregation function: {func}"),
        };
        return Ok((agg_name, Some(metric)));
    }

    bail!(
        "invalid --compute format: {input:?}\n\
         Expected: count, avg(@duration), sum(@duration), percentile(@duration, 99), etc."
    )
}

fn build_aggregate_body(
    query: String,
    from_ms: i64,
    to_ms: i64,
    compute: String,
    group_by: Option<String>,
    limit: i32,
    storage: Option<String>,
) -> Result<serde_json::Value> {
    let (aggregation, metric) = parse_compute_raw(&compute)?;
    let storage_tier = normalize_storage_tier(storage)?;

    let mut filter = serde_json::json!({
        "query": query,
        "from": from_ms.to_string(),
        "to": to_ms.to_string()
    });
    if let Some(tier) = storage_tier {
        filter["storage_tier"] = serde_json::Value::String(tier);
    }

    let mut compute_obj = serde_json::json!({ "aggregation": aggregation });
    if let Some(metric) = metric {
        compute_obj["metric"] = serde_json::Value::String(metric);
    }

    let mut body = serde_json::json!({
        "filter": filter,
        "compute": [compute_obj]
    });

    if let Some(facet) = group_by {
        let mut group_by_obj = serde_json::json!({ "facet": facet });
        if limit > 0 {
            group_by_obj["limit"] = serde_json::json!(limit);
        }
        body["group_by"] = serde_json::json!([group_by_obj]);
    }

    Ok(body)
}

#[cfg(not(target_arch = "wasm32"))]
fn parse_logs_sort(sort: &str) -> LogsSort {
    match sort {
        "timestamp" | "asc" | "+timestamp" => LogsSort::TIMESTAMP_ASCENDING,
        _ => LogsSort::TIMESTAMP_DESCENDING,
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn search(
    cfg: &Config,
    query: String,
    from: String,
    to: String,
    limit: i32,
    sort: String,
    storage: Option<String>,
) -> Result<()> {
    let dd_cfg = client::make_dd_config(cfg);
    let api = match client::make_bearer_client(cfg) {
        Some(c) => LogsAPI::with_client_and_config(dd_cfg, c),
        None => LogsAPI::with_config(dd_cfg),
    };

    let from_ms = util::parse_time_to_unix_millis(&from)?;
    let to_ms = util::parse_time_to_unix_millis(&to)?;

    let storage_tier = parse_storage_tier(storage)?;

    let mut filter = LogsQueryFilter::new()
        .query(query)
        .from(from_ms.to_string())
        .to(to_ms.to_string());
    if let Some(tier) = storage_tier {
        filter = filter.storage_tier(tier);
    }

    let body = LogsListRequest::new()
        .filter(filter)
        .page(LogsListRequestPage::new().limit(limit))
        .sort(parse_logs_sort(&sort));

    let params = ListLogsOptionalParams::default().body(body);

    let resp = api
        .list_logs(params)
        .await
        .map_err(|e| anyhow::anyhow!("failed to search logs: {:?}", e))?;

    let meta = if cfg.agent_mode {
        let count = resp.data.as_ref().map(|d| d.len());
        let truncated = count.is_some_and(|c| c as i32 >= limit);
        Some(formatter::Metadata {
            count,
            truncated,
            command: Some("logs search".into()),
            next_action: if truncated {
                Some(format!(
                    "Results may be truncated at {limit}. Use --limit={} or narrow the --query",
                    limit + 1
                ))
            } else {
                None
            },
        })
    } else {
        None
    };
    formatter::format_and_print(&resp, &cfg.output_format, cfg.agent_mode, meta.as_ref())?;
    Ok(())
}

#[cfg(target_arch = "wasm32")]
pub async fn search(
    cfg: &Config,
    query: String,
    from: String,
    to: String,
    limit: i32,
    sort: String,
    storage: Option<String>,
) -> Result<()> {
    let from_ms = util::parse_time_to_unix_millis(&from)?;
    let to_ms = util::parse_time_to_unix_millis(&to)?;
    let mut filter = serde_json::json!({
        "query": query,
        "from": from_ms.to_string(),
        "to": to_ms.to_string()
    });
    if let Some(tier) = storage {
        filter["storage_tier"] = serde_json::Value::String(tier);
    }
    let sort_value = match sort.as_str() {
        "timestamp" | "asc" | "+timestamp" => "timestamp",
        _ => "-timestamp",
    };
    let body = serde_json::json!({
        "filter": filter,
        "page": { "limit": limit },
        "sort": sort_value
    });
    let data = crate::api::post(cfg, "/api/v2/logs/events/search", &body).await?;
    crate::formatter::output(cfg, &data)
}

/// Alias for `search` with the same interface.
pub async fn list(
    cfg: &Config,
    query: String,
    from: String,
    to: String,
    limit: i32,
    sort: String,
    storage: Option<String>,
) -> Result<()> {
    search(cfg, query, from, to, limit, sort, storage).await
}

/// Alias for `search` with the same interface.
pub async fn query(
    cfg: &Config,
    query: String,
    from: String,
    to: String,
    limit: i32,
    sort: String,
    storage: Option<String>,
) -> Result<()> {
    search(cfg, query, from, to, limit, sort, storage).await
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn aggregate(cfg: &Config, args: AggregateArgs) -> Result<()> {
    let AggregateArgs {
        query,
        from,
        to,
        compute,
        group_by,
        limit,
        storage,
    } = args;
    let from_ms = util::parse_time_to_unix_millis(&from)?;
    let to_ms = util::parse_time_to_unix_millis(&to)?;
    let body = build_aggregate_body(query, from_ms, to_ms, compute, group_by, limit, storage)?;
    let data = client::raw_post(cfg, "/api/v2/logs/analytics/aggregate", body).await?;
    formatter::output(cfg, &data)?;
    Ok(())
}

#[cfg(target_arch = "wasm32")]
pub async fn aggregate(cfg: &Config, args: AggregateArgs) -> Result<()> {
    let AggregateArgs {
        query,
        from,
        to,
        compute,
        group_by,
        limit,
        storage,
    } = args;
    let from_ms = util::parse_time_to_unix_millis(&from)?;
    let to_ms = util::parse_time_to_unix_millis(&to)?;
    let body = build_aggregate_body(query, from_ms, to_ms, compute, group_by, limit, storage)?;
    let data = crate::api::post(cfg, "/api/v2/logs/analytics/aggregate", &body).await?;
    crate::formatter::output(cfg, &data)
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn archives_list(cfg: &Config) -> Result<()> {
    let dd_cfg = client::make_dd_config(cfg);
    let api = match client::make_bearer_client(cfg) {
        Some(c) => LogsArchivesAPI::with_client_and_config(dd_cfg, c),
        None => LogsArchivesAPI::with_config(dd_cfg),
    };

    let resp = api
        .list_logs_archives()
        .await
        .map_err(|e| anyhow::anyhow!("failed to list log archives: {:?}", e))?;

    formatter::output(cfg, &resp)?;
    Ok(())
}

#[cfg(target_arch = "wasm32")]
pub async fn archives_list(cfg: &Config) -> Result<()> {
    let data = crate::api::get(cfg, "/api/v2/logs/config/archives", &[]).await?;
    crate::formatter::output(cfg, &data)
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn archives_get(cfg: &Config, archive_id: &str) -> Result<()> {
    let dd_cfg = client::make_dd_config(cfg);
    let api = match client::make_bearer_client(cfg) {
        Some(c) => LogsArchivesAPI::with_client_and_config(dd_cfg, c),
        None => LogsArchivesAPI::with_config(dd_cfg),
    };

    let resp = api
        .get_logs_archive(archive_id.to_string())
        .await
        .map_err(|e| anyhow::anyhow!("failed to get log archive: {:?}", e))?;

    formatter::output(cfg, &resp)?;
    Ok(())
}

#[cfg(target_arch = "wasm32")]
pub async fn archives_get(cfg: &Config, archive_id: &str) -> Result<()> {
    let path = format!("/api/v2/logs/config/archives/{archive_id}");
    let data = crate::api::get(cfg, &path, &[]).await?;
    crate::formatter::output(cfg, &data)
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn archives_delete(cfg: &Config, archive_id: &str) -> Result<()> {
    let dd_cfg = client::make_dd_config(cfg);
    let api = match client::make_bearer_client(cfg) {
        Some(c) => LogsArchivesAPI::with_client_and_config(dd_cfg, c),
        None => LogsArchivesAPI::with_config(dd_cfg),
    };

    api.delete_logs_archive(archive_id.to_string())
        .await
        .map_err(|e| anyhow::anyhow!("failed to delete log archive: {:?}", e))?;

    println!("Log archive {archive_id} deleted.");
    Ok(())
}

#[cfg(target_arch = "wasm32")]
pub async fn archives_delete(cfg: &Config, archive_id: &str) -> Result<()> {
    let path = format!("/api/v2/logs/config/archives/{archive_id}");
    crate::api::delete(cfg, &path).await?;
    println!("Log archive {archive_id} deleted.");
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn custom_destinations_list(cfg: &Config) -> Result<()> {
    let dd_cfg = client::make_dd_config(cfg);
    let api = match client::make_bearer_client(cfg) {
        Some(c) => LogsCustomDestinationsAPI::with_client_and_config(dd_cfg, c),
        None => LogsCustomDestinationsAPI::with_config(dd_cfg),
    };

    let resp = api
        .list_logs_custom_destinations()
        .await
        .map_err(|e| anyhow::anyhow!("failed to list custom destinations: {:?}", e))?;

    formatter::output(cfg, &resp)?;
    Ok(())
}

#[cfg(target_arch = "wasm32")]
pub async fn custom_destinations_list(cfg: &Config) -> Result<()> {
    let data = crate::api::get(cfg, "/api/v2/logs/config/custom_destinations", &[]).await?;
    crate::formatter::output(cfg, &data)
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn custom_destinations_get(cfg: &Config, destination_id: &str) -> Result<()> {
    let dd_cfg = client::make_dd_config(cfg);
    let api = match client::make_bearer_client(cfg) {
        Some(c) => LogsCustomDestinationsAPI::with_client_and_config(dd_cfg, c),
        None => LogsCustomDestinationsAPI::with_config(dd_cfg),
    };

    let resp = api
        .get_logs_custom_destination(destination_id.to_string())
        .await
        .map_err(|e| anyhow::anyhow!("failed to get custom destination: {:?}", e))?;

    formatter::output(cfg, &resp)?;
    Ok(())
}

#[cfg(target_arch = "wasm32")]
pub async fn custom_destinations_get(cfg: &Config, destination_id: &str) -> Result<()> {
    let path = format!("/api/v2/logs/config/custom_destinations/{destination_id}");
    let data = crate::api::get(cfg, &path, &[]).await?;
    crate::formatter::output(cfg, &data)
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn metrics_list(cfg: &Config) -> Result<()> {
    let dd_cfg = client::make_dd_config(cfg);
    let api = match client::make_bearer_client(cfg) {
        Some(c) => LogsMetricsAPI::with_client_and_config(dd_cfg, c),
        None => LogsMetricsAPI::with_config(dd_cfg),
    };

    let resp = api
        .list_logs_metrics()
        .await
        .map_err(|e| anyhow::anyhow!("failed to list log-based metrics: {:?}", e))?;

    formatter::output(cfg, &resp)?;
    Ok(())
}

#[cfg(target_arch = "wasm32")]
pub async fn metrics_list(cfg: &Config) -> Result<()> {
    let data = crate::api::get(cfg, "/api/v2/logs/config/metrics", &[]).await?;
    crate::formatter::output(cfg, &data)
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn metrics_get(cfg: &Config, metric_id: &str) -> Result<()> {
    let dd_cfg = client::make_dd_config(cfg);
    let api = match client::make_bearer_client(cfg) {
        Some(c) => LogsMetricsAPI::with_client_and_config(dd_cfg, c),
        None => LogsMetricsAPI::with_config(dd_cfg),
    };

    let resp = api
        .get_logs_metric(metric_id.to_string())
        .await
        .map_err(|e| anyhow::anyhow!("failed to get log-based metric: {:?}", e))?;

    formatter::output(cfg, &resp)?;
    Ok(())
}

#[cfg(target_arch = "wasm32")]
pub async fn metrics_get(cfg: &Config, metric_id: &str) -> Result<()> {
    let path = format!("/api/v2/logs/config/metrics/{metric_id}");
    let data = crate::api::get(cfg, &path, &[]).await?;
    crate::formatter::output(cfg, &data)
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn metrics_delete(cfg: &Config, metric_id: &str) -> Result<()> {
    let dd_cfg = client::make_dd_config(cfg);
    let api = match client::make_bearer_client(cfg) {
        Some(c) => LogsMetricsAPI::with_client_and_config(dd_cfg, c),
        None => LogsMetricsAPI::with_config(dd_cfg),
    };

    api.delete_logs_metric(metric_id.to_string())
        .await
        .map_err(|e| anyhow::anyhow!("failed to delete log-based metric: {:?}", e))?;

    println!("Log-based metric {metric_id} deleted.");
    Ok(())
}

#[cfg(target_arch = "wasm32")]
pub async fn metrics_delete(cfg: &Config, metric_id: &str) -> Result<()> {
    let path = format!("/api/v2/logs/config/metrics/{metric_id}");
    crate::api::delete(cfg, &path).await?;
    println!("Log-based metric {metric_id} deleted.");
    Ok(())
}

// ---------------------------------------------------------------------------
// Restriction Queries (raw HTTP - not available in typed client)
// ---------------------------------------------------------------------------

#[cfg(not(target_arch = "wasm32"))]
pub async fn restriction_queries_list(cfg: &Config) -> Result<()> {
    let data = client::raw_get(cfg, "/api/v2/logs/config/restriction_queries").await?;
    formatter::output(cfg, &data)
}

#[cfg(target_arch = "wasm32")]
pub async fn restriction_queries_list(cfg: &Config) -> Result<()> {
    let data = crate::api::get(cfg, "/api/v2/logs/config/restriction_queries", &[]).await?;
    crate::formatter::output(cfg, &data)
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn restriction_queries_get(cfg: &Config, query_id: &str) -> Result<()> {
    let path = format!("/api/v2/logs/config/restriction_queries/{query_id}");
    let data = client::raw_get(cfg, &path).await?;
    formatter::output(cfg, &data)
}

#[cfg(target_arch = "wasm32")]
pub async fn restriction_queries_get(cfg: &Config, query_id: &str) -> Result<()> {
    let path = format!("/api/v2/logs/config/restriction_queries/{query_id}");
    let data = crate::api::get(cfg, &path, &[]).await?;
    crate::formatter::output(cfg, &data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_compute_count() {
        let (aggregation, metric) = parse_compute_raw("count").unwrap();
        assert_eq!(aggregation, "count");
        assert!(metric.is_none());
    }

    #[test]
    fn test_parse_compute_avg() {
        let (aggregation, metric) = parse_compute_raw("avg(@duration)").unwrap();
        assert_eq!(aggregation, "avg");
        assert_eq!(metric.unwrap(), "@duration");
    }

    #[test]
    fn test_parse_compute_sum() {
        let (aggregation, metric) = parse_compute_raw("sum(@duration)").unwrap();
        assert_eq!(aggregation, "sum");
        assert_eq!(metric.unwrap(), "@duration");
    }

    #[test]
    fn test_parse_compute_min() {
        let (aggregation, metric) = parse_compute_raw("min(@duration)").unwrap();
        assert_eq!(aggregation, "min");
        assert_eq!(metric.unwrap(), "@duration");
    }

    #[test]
    fn test_parse_compute_max() {
        let (aggregation, metric) = parse_compute_raw("max(@duration)").unwrap();
        assert_eq!(aggregation, "max");
        assert_eq!(metric.unwrap(), "@duration");
    }

    #[test]
    fn test_parse_compute_median() {
        let (aggregation, metric) = parse_compute_raw("median(@duration)").unwrap();
        assert_eq!(aggregation, "median");
        assert_eq!(metric.unwrap(), "@duration");
    }

    #[test]
    fn test_parse_compute_cardinality() {
        let (aggregation, metric) = parse_compute_raw("cardinality(@usr.id)").unwrap();
        assert_eq!(aggregation, "cardinality");
        assert_eq!(metric.unwrap(), "@usr.id");
    }

    #[test]
    fn test_parse_compute_percentile_99() {
        let (aggregation, metric) = parse_compute_raw("percentile(@duration, 99)").unwrap();
        assert_eq!(aggregation, "pc99");
        assert_eq!(metric.unwrap(), "@duration");
    }

    #[test]
    fn test_parse_compute_percentile_95() {
        let (aggregation, metric) = parse_compute_raw("percentile(@duration, 95)").unwrap();
        assert_eq!(aggregation, "pc95");
        assert_eq!(metric.unwrap(), "@duration");
    }

    #[test]
    fn test_parse_compute_percentile_90() {
        let (aggregation, metric) = parse_compute_raw("percentile(@duration, 90)").unwrap();
        assert_eq!(aggregation, "pc90");
        assert_eq!(metric.unwrap(), "@duration");
    }

    #[test]
    fn test_parse_compute_empty() {
        assert!(parse_compute_raw("").is_err());
    }

    #[test]
    fn test_parse_compute_invalid() {
        assert!(parse_compute_raw("invalid").is_err());
    }

    #[test]
    fn test_parse_compute_unknown_function() {
        assert!(parse_compute_raw("foo(@bar)").is_err());
    }

    #[test]
    fn test_parse_compute_unsupported_percentile() {
        assert!(parse_compute_raw("percentile(@duration, 50)").is_err());
    }

    #[test]
    fn test_parse_compute_percentile_missing_value() {
        assert!(parse_compute_raw("percentile(@duration)").is_err());
    }

    #[test]
    fn test_parse_compute_rejects_invalid_count_metric() {
        let err = parse_compute_raw("count(@duration)").unwrap_err();
        assert!(err.to_string().contains("does not accept a field"));
    }

    #[test]
    fn test_normalize_storage_tier_alias() {
        let tier = normalize_storage_tier(Some("online_archives".into())).unwrap();
        assert_eq!(tier.unwrap(), "online-archives");
    }

    #[test]
    fn test_build_aggregate_body_includes_compute_group_by_limit_and_storage() {
        let body = build_aggregate_body(
            "service:web".into(),
            1,
            2,
            "avg(@duration)".into(),
            Some("service".into()),
            3,
            Some("flex".into()),
        )
        .unwrap();

        assert_eq!(
            body,
            serde_json::json!({
                "filter": {
                    "query": "service:web",
                    "from": "1",
                    "to": "2",
                    "storage_tier": "flex"
                },
                "compute": [{
                    "aggregation": "avg",
                    "metric": "@duration"
                }],
                "group_by": [{
                    "facet": "service",
                    "limit": 3
                }]
            })
        );
    }

    #[test]
    fn test_build_aggregate_body_omits_group_by_for_plain_count() {
        let body = build_aggregate_body("*".into(), 1, 2, "count".into(), None, 10, None).unwrap();

        assert_eq!(
            body,
            serde_json::json!({
                "filter": {
                    "query": "*",
                    "from": "1",
                    "to": "2"
                },
                "compute": [{
                    "aggregation": "count"
                }]
            })
        );
    }
}
