use anyhow::Result;
#[cfg(not(target_arch = "wasm32"))]
use datadog_api_client::datadogV2::api_app_builder::{AppBuilderAPI, ListAppsOptionalParams};
#[cfg(not(target_arch = "wasm32"))]
use datadog_api_client::datadogV2::model::{
    AppDefinitionType, CreateAppRequest, DeleteAppsRequest, DeleteAppsRequestDataItems,
    UpdateAppRequest,
};

#[cfg(not(target_arch = "wasm32"))]
use crate::client;
use crate::config::Config;
use crate::formatter;
use crate::util;

#[cfg(not(target_arch = "wasm32"))]
pub async fn list(cfg: &Config, query: Option<&str>) -> Result<()> {
    let dd_cfg = client::make_dd_config(cfg);
    let api = match client::make_bearer_client(cfg) {
        Some(c) => AppBuilderAPI::with_client_and_config(dd_cfg, c),
        None => AppBuilderAPI::with_config(dd_cfg),
    };
    let mut params = ListAppsOptionalParams::default();
    if let Some(q) = query {
        params = params.filter_query(q.to_string());
    }
    let resp = api
        .list_apps(params)
        .await
        .map_err(|e| anyhow::anyhow!("failed to list apps: {e:?}"))?;
    formatter::output(cfg, &resp)
}

#[cfg(target_arch = "wasm32")]
pub async fn list(cfg: &Config, query: Option<&str>) -> Result<()> {
    let mut qs: Vec<(&str, String)> = vec![];
    if let Some(q) = query {
        qs.push(("filter[query]", q.to_string()));
    }
    let data = crate::api::get(cfg, "/api/v2/app-builder/apps", &qs).await?;
    crate::formatter::output(cfg, &data)
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn get(cfg: &Config, app_id: &str) -> Result<()> {
    let dd_cfg = client::make_dd_config(cfg);
    let api = match client::make_bearer_client(cfg) {
        Some(c) => AppBuilderAPI::with_client_and_config(dd_cfg, c),
        None => AppBuilderAPI::with_config(dd_cfg),
    };
    let uuid = util::parse_uuid(app_id, "app")?;
    let resp = api
        .get_app(uuid, Default::default())
        .await
        .map_err(|e| anyhow::anyhow!("failed to get app: {e:?}"))?;
    formatter::output(cfg, &resp)
}

#[cfg(target_arch = "wasm32")]
pub async fn get(cfg: &Config, app_id: &str) -> Result<()> {
    util::parse_uuid(app_id, "app")?;
    let data = crate::api::get(cfg, &format!("/api/v2/app-builder/apps/{app_id}"), &[]).await?;
    crate::formatter::output(cfg, &data)
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn create(cfg: &Config, file: &str) -> Result<()> {
    let dd_cfg = client::make_dd_config(cfg);
    let api = match client::make_bearer_client(cfg) {
        Some(c) => AppBuilderAPI::with_client_and_config(dd_cfg, c),
        None => AppBuilderAPI::with_config(dd_cfg),
    };
    let body: CreateAppRequest = util::read_json_file(file)?;
    let resp = api
        .create_app(body)
        .await
        .map_err(|e| anyhow::anyhow!("failed to create app: {e:?}"))?;
    formatter::output(cfg, &resp)
}

#[cfg(target_arch = "wasm32")]
pub async fn create(cfg: &Config, file: &str) -> Result<()> {
    let body: serde_json::Value = util::read_json_file(file)?;
    let data = crate::api::post(cfg, "/api/v2/app-builder/apps", &body).await?;
    crate::formatter::output(cfg, &data)
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn update(cfg: &Config, app_id: &str, file: &str) -> Result<()> {
    let dd_cfg = client::make_dd_config(cfg);
    let api = match client::make_bearer_client(cfg) {
        Some(c) => AppBuilderAPI::with_client_and_config(dd_cfg, c),
        None => AppBuilderAPI::with_config(dd_cfg),
    };
    let uuid = util::parse_uuid(app_id, "app")?;
    let body: UpdateAppRequest = util::read_json_file(file)?;
    let resp = api
        .update_app(uuid, body)
        .await
        .map_err(|e| anyhow::anyhow!("failed to update app: {e:?}"))?;
    formatter::output(cfg, &resp)
}

#[cfg(target_arch = "wasm32")]
pub async fn update(cfg: &Config, app_id: &str, file: &str) -> Result<()> {
    util::parse_uuid(app_id, "app")?;
    let body: serde_json::Value = util::read_json_file(file)?;
    let data = crate::api::put(cfg, &format!("/api/v2/app-builder/apps/{app_id}"), &body).await?;
    crate::formatter::output(cfg, &data)
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn delete(cfg: &Config, app_id: &str) -> Result<()> {
    let dd_cfg = client::make_dd_config(cfg);
    let api = match client::make_bearer_client(cfg) {
        Some(c) => AppBuilderAPI::with_client_and_config(dd_cfg, c),
        None => AppBuilderAPI::with_config(dd_cfg),
    };
    let uuid = util::parse_uuid(app_id, "app")?;
    api.delete_app(uuid)
        .await
        .map_err(|e| anyhow::anyhow!("failed to delete app: {e:?}"))?;
    println!("Successfully deleted app {app_id}");
    Ok(())
}

#[cfg(target_arch = "wasm32")]
pub async fn delete(cfg: &Config, app_id: &str) -> Result<()> {
    util::parse_uuid(app_id, "app")?;
    crate::api::delete(cfg, &format!("/api/v2/app-builder/apps/{app_id}")).await?;
    println!("Successfully deleted app {app_id}");
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn delete_batch(cfg: &Config, app_ids: &[String]) -> Result<()> {
    let dd_cfg = client::make_dd_config(cfg);
    let api = match client::make_bearer_client(cfg) {
        Some(c) => AppBuilderAPI::with_client_and_config(dd_cfg, c),
        None => AppBuilderAPI::with_config(dd_cfg),
    };
    let items: Result<Vec<_>> = app_ids
        .iter()
        .map(|id| {
            let uuid = util::parse_uuid(id, "app")?;
            Ok(DeleteAppsRequestDataItems::new(
                uuid,
                AppDefinitionType::APPDEFINITIONS,
            ))
        })
        .collect();
    let body = DeleteAppsRequest::new().data(items?);
    let resp = api
        .delete_apps(body)
        .await
        .map_err(|e| anyhow::anyhow!("failed to delete apps: {e:?}"))?;
    formatter::output(cfg, &resp)
}

#[cfg(target_arch = "wasm32")]
pub async fn delete_batch(cfg: &Config, app_ids: &[String]) -> Result<()> {
    for id in app_ids {
        util::parse_uuid(id, "app")?;
    }
    let items: Vec<_> = app_ids
        .iter()
        .map(|id| serde_json::json!({"id": id, "type": "appDefinitions"}))
        .collect();
    let body = serde_json::json!({"data": items});
    let data = crate::api::delete_with_body(cfg, "/api/v2/app-builder/apps", &body).await?;
    crate::formatter::output(cfg, &data)
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn publish(cfg: &Config, app_id: &str) -> Result<()> {
    let dd_cfg = client::make_dd_config(cfg);
    let api = match client::make_bearer_client(cfg) {
        Some(c) => AppBuilderAPI::with_client_and_config(dd_cfg, c),
        None => AppBuilderAPI::with_config(dd_cfg),
    };
    let uuid = util::parse_uuid(app_id, "app")?;
    let resp = api
        .publish_app(uuid)
        .await
        .map_err(|e| anyhow::anyhow!("failed to publish app: {e:?}"))?;
    formatter::output(cfg, &resp)
}

#[cfg(target_arch = "wasm32")]
pub async fn publish(cfg: &Config, app_id: &str) -> Result<()> {
    util::parse_uuid(app_id, "app")?;
    let data = crate::api::post(
        cfg,
        &format!("/api/v2/app-builder/apps/{app_id}/deployment"),
        &serde_json::Value::Null,
    )
    .await?;
    crate::formatter::output(cfg, &data)
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn unpublish(cfg: &Config, app_id: &str) -> Result<()> {
    let dd_cfg = client::make_dd_config(cfg);
    let api = match client::make_bearer_client(cfg) {
        Some(c) => AppBuilderAPI::with_client_and_config(dd_cfg, c),
        None => AppBuilderAPI::with_config(dd_cfg),
    };
    let uuid = util::parse_uuid(app_id, "app")?;
    api.unpublish_app(uuid)
        .await
        .map_err(|e| anyhow::anyhow!("failed to unpublish app: {e:?}"))?;
    println!("Successfully unpublished app {app_id}");
    Ok(())
}

#[cfg(target_arch = "wasm32")]
pub async fn unpublish(cfg: &Config, app_id: &str) -> Result<()> {
    util::parse_uuid(app_id, "app")?;
    crate::api::delete(
        cfg,
        &format!("/api/v2/app-builder/apps/{app_id}/deployment"),
    )
    .await?;
    println!("Successfully unpublished app {app_id}");
    Ok(())
}
