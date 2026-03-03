use anyhow::Result;
use std::path::PathBuf;

use super::{Runbook, RunbookMeta};
use crate::config::Config;

/// Returns the runbooks directory: ~/.config/pup/runbooks/
pub fn runbooks_dir(_cfg: &Config) -> Option<PathBuf> {
    crate::config::config_dir().map(|d| d.join("runbooks"))
}

/// List all runbooks, optionally filtered by tags (format: "key:value").
pub fn list_runbooks(cfg: &Config, tags: &[String]) -> Result<Vec<RunbookMeta>> {
    let dir = match runbooks_dir(cfg) {
        Some(d) => d,
        None => return Ok(vec![]),
    };
    if !dir.exists() {
        return Ok(vec![]);
    }

    let mut results = vec![];
    let mut entries: Vec<_> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|x| x.to_str())
                .map(|x| x == "yaml" || x == "yml")
                .unwrap_or(false)
        })
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        let contents = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("failed to read {:?}: {e}", path))?;
        let runbook: Runbook = serde_yaml::from_str(&contents)
            .map_err(|e| anyhow::anyhow!("failed to parse {:?}: {e}", path))?;

        if !tags.is_empty() {
            let runbook_tags = runbook.tags.clone().unwrap_or_default();
            let matches = tags.iter().all(|tag| {
                let mut parts = tag.splitn(2, ':');
                let key = parts.next().unwrap_or("");
                if let Some(val) = parts.next() {
                    runbook_tags.get(key).map(|v| v == val).unwrap_or(false)
                } else {
                    runbook_tags.contains_key(key)
                }
            });
            if !matches {
                continue;
            }
        }

        results.push(RunbookMeta {
            name: runbook.name,
            description: runbook.description,
            tags: runbook.tags.unwrap_or_default(),
            steps: runbook.steps.len(),
        });
    }

    Ok(results)
}

/// Load a single runbook by name from the runbooks directory.
pub fn load_runbook(cfg: &Config, name: &str) -> Result<Runbook> {
    let dir = runbooks_dir(cfg)
        .ok_or_else(|| anyhow::anyhow!("could not determine runbooks directory"))?;

    // Try <name>.yaml then <name>.yml
    for ext in &["yaml", "yml"] {
        let path = dir.join(format!("{name}.{ext}"));
        if path.exists() {
            let contents = std::fs::read_to_string(&path)
                .map_err(|e| anyhow::anyhow!("failed to read {:?}: {e}", path))?;
            return serde_yaml::from_str(&contents)
                .map_err(|e| anyhow::anyhow!("failed to parse runbook '{name}': {e}"));
        }
    }

    anyhow::bail!(
        "runbook '{}' not found in {:?} (expected {}.yaml)",
        name,
        dir,
        name
    )
}

/// Import a runbook from a file path or HTTP(S) URL.
pub async fn import_runbook(cfg: &Config, source: &str) -> Result<()> {
    let dir = runbooks_dir(cfg)
        .ok_or_else(|| anyhow::anyhow!("could not determine runbooks directory"))?;
    std::fs::create_dir_all(&dir)?;

    if source.starts_with("http://") || source.starts_with("https://") {
        let client = reqwest::Client::new();
        let body = client
            .get(source)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("failed to fetch {source}: {e}"))?
            .text()
            .await
            .map_err(|e| anyhow::anyhow!("failed to read response body: {e}"))?;

        let filename = source
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or("imported");
        let filename = if filename.ends_with(".yaml") || filename.ends_with(".yml") {
            filename.to_string()
        } else {
            format!("{filename}.yaml")
        };
        let dest = dir.join(&filename);
        std::fs::write(&dest, &body)
            .map_err(|e| anyhow::anyhow!("failed to write {:?}: {e}", dest))?;
        println!("Imported runbook to {}", dest.display());
    } else {
        let src_path = std::path::Path::new(source);
        if !src_path.exists() {
            anyhow::bail!("source file not found: {}", source);
        }
        let filename = src_path
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("invalid source path: {}", source))?;
        let dest = dir.join(filename);
        std::fs::copy(src_path, &dest)
            .map_err(|e| anyhow::anyhow!("failed to copy to {:?}: {e}", dest))?;
        println!("Imported runbook to {}", dest.display());
    }

    Ok(())
}
