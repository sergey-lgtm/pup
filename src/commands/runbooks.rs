use anyhow::Result;
use std::collections::HashMap;

use crate::config::Config;
use crate::runbooks::{engine, loader, template};

/// List available runbooks, optionally filtered by tags.
pub fn list(cfg: &Config, tags: Vec<String>) -> Result<()> {
    let runbooks = loader::list_runbooks(cfg, &tags)?;

    if runbooks.is_empty() {
        let dir = loader::runbooks_dir(cfg);
        let dir_str = dir
            .as_ref()
            .map(|d| d.display().to_string())
            .unwrap_or_else(|| "~/.config/pup/runbooks".into());

        let result = serde_json::json!({
            "runbooks": [],
            "message": format!("No runbooks found in {dir_str}. Create a .yaml file there or run 'pup runbooks import <path>'.")
        });
        println!("{}", serde_json::to_string_pretty(&result).unwrap());
        return Ok(());
    }

    let items: Vec<serde_json::Value> = runbooks
        .iter()
        .map(|rb| {
            serde_json::json!({
                "name": rb.name,
                "description": rb.description,
                "tags": rb.tags,
                "steps": rb.steps,
            })
        })
        .collect();

    let result = serde_json::json!({ "runbooks": items });
    println!("{}", serde_json::to_string_pretty(&result).unwrap());
    Ok(())
}

/// Show details of a runbook.
pub fn describe(cfg: &Config, name: &str) -> Result<()> {
    let runbook = loader::load_runbook(cfg, name)?;

    let steps: Vec<serde_json::Value> = runbook
        .steps
        .iter()
        .enumerate()
        .map(|(i, s)| {
            serde_json::json!({
                "step": i + 1,
                "name": s.name,
                "kind": s.kind,
                "run": s.run,
                "message": s.message,
                "on_failure": s.on_failure.as_deref().unwrap_or("fail"),
                "when": s.when.as_deref().unwrap_or("on_success"),
                "optional": s.optional.unwrap_or(false),
                "capture": s.capture,
                "poll": s.poll.as_ref().map(|p| serde_json::json!({
                    "interval": p.interval,
                    "timeout": p.timeout,
                    "until": p.until,
                })),
            })
        })
        .collect();

    let vars: serde_json::Value = runbook
        .vars
        .as_ref()
        .map(|v| {
            let map: serde_json::Map<String, serde_json::Value> = v
                .iter()
                .map(|(k, def)| {
                    (
                        k.clone(),
                        serde_json::json!({
                            "description": def.description,
                            "required": def.required.unwrap_or(false),
                            "default": def.default,
                        }),
                    )
                })
                .collect();
            serde_json::Value::Object(map)
        })
        .unwrap_or(serde_json::Value::Object(Default::default()));

    let result = serde_json::json!({
        "name": runbook.name,
        "description": runbook.description,
        "tags": runbook.tags,
        "vars": vars,
        "steps": steps,
    });

    println!("{}", serde_json::to_string_pretty(&result).unwrap());
    Ok(())
}

/// Execute a runbook with optional variable overrides.
pub async fn run(cfg: &Config, name: &str, set: Vec<String>, _yes: bool) -> Result<()> {
    let runbook = loader::load_runbook(cfg, name)?;

    // Parse --set KEY=VALUE pairs
    let mut vars: HashMap<String, String> = HashMap::new();
    for kv in &set {
        let mut parts = kv.splitn(2, '=');
        let key = parts
            .next()
            .ok_or_else(|| anyhow::anyhow!("invalid --set value '{kv}': expected KEY=VALUE"))?;
        let val = parts
            .next()
            .ok_or_else(|| anyhow::anyhow!("invalid --set value '{kv}': expected KEY=VALUE"))?;
        vars.insert(key.to_string(), val.to_string());
    }

    engine::run(cfg, &runbook, vars).await
}

/// Validate a runbook's structure without executing it.
pub fn validate(cfg: &Config, name: &str) -> Result<()> {
    let runbook = loader::load_runbook(cfg, name)?;
    let mut errors: Vec<String> = vec![];

    const VALID_KINDS: &[&str] = &["pup", "shell", "datadog-workflow", "confirm", "http"];

    for (i, step) in runbook.steps.iter().enumerate() {
        let step_id = format!("step {} '{}'", i + 1, step.name);

        // Check kind is valid
        if !VALID_KINDS.contains(&step.kind.as_str()) {
            errors.push(format!(
                "{step_id}: unknown kind '{}' (valid: {})",
                step.kind,
                VALID_KINDS.join(", ")
            ));
        }

        // Kind-specific required fields
        match step.kind.as_str() {
            "pup" | "shell" => {
                if step.run.is_none() {
                    errors.push(format!(
                        "{step_id}: kind '{kind}' requires 'run'",
                        kind = step.kind
                    ));
                }
            }
            "datadog-workflow" => {
                if step.workflow_id.is_none() {
                    errors.push(format!(
                        "{step_id}: kind 'datadog-workflow' requires 'workflow_id'"
                    ));
                }
            }
            "confirm" => {
                if step.message.is_none() {
                    errors.push(format!("{step_id}: kind 'confirm' requires 'message'"));
                }
            }
            _ => {}
        }

        // Validate poll durations
        if let Some(poll) = &step.poll {
            if let Err(e) = template::parse_duration(&poll.interval) {
                errors.push(format!("{step_id}: invalid poll.interval: {e}"));
            }
            if let Err(e) = template::parse_duration(&poll.timeout) {
                errors.push(format!("{step_id}: invalid poll.timeout: {e}"));
            }
        }
    }

    // Check required vars have defaults or are marked not required
    if let Some(var_defs) = &runbook.vars {
        for (k, def) in var_defs {
            let required = def.required.unwrap_or(true);
            if required && def.default.is_none() {
                errors.push(format!(
                    "var '{k}': required but has no default (provide with --set {k}=value)"
                ));
            }
        }
    }

    if errors.is_empty() {
        let result = serde_json::json!({
            "valid": true,
            "name": runbook.name,
            "steps": runbook.steps.len(),
        });
        println!("{}", serde_json::to_string_pretty(&result).unwrap());
        Ok(())
    } else {
        let result = serde_json::json!({
            "valid": false,
            "name": runbook.name,
            "errors": errors,
        });
        println!("{}", serde_json::to_string_pretty(&result).unwrap());
        anyhow::bail!("runbook validation failed with {} error(s)", errors.len())
    }
}

/// Import a runbook from a file path or URL.
pub async fn import(cfg: &Config, source: &str) -> Result<()> {
    loader::import_runbook(cfg, source).await
}
