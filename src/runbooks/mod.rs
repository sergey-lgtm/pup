use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub mod engine;
pub mod loader;
pub mod template;

/// A runbook definition loaded from ~/.config/pup/runbooks/<name>.yaml.
#[derive(Deserialize, Serialize, Clone)]
pub struct Runbook {
    pub name: String,
    pub description: Option<String>,
    pub tags: Option<HashMap<String, String>>,
    pub import: Option<Vec<String>>,
    pub vars: Option<HashMap<String, VarDef>>,
    pub steps: Vec<Step>,
}

/// Definition of a runbook variable.
#[derive(Deserialize, Serialize, Clone)]
pub struct VarDef {
    pub description: Option<String>,
    pub required: Option<bool>,
    pub default: Option<String>,
}

/// A single step in a runbook.
#[derive(Deserialize, Serialize, Clone)]
pub struct Step {
    pub name: String,
    /// "pup" | "shell" | "datadog-workflow" | "confirm" | "http"
    pub kind: String,
    /// pup or shell command to run
    pub run: Option<String>,
    /// Datadog Workflow ID (for kind: datadog-workflow)
    pub workflow_id: Option<String>,
    /// Inputs for the workflow
    pub inputs: Option<HashMap<String, String>>,
    /// URL for http steps
    pub url: Option<String>,
    /// HTTP method (GET, POST, etc.)
    pub method: Option<String>,
    /// Message to display for confirm steps
    pub message: Option<String>,
    /// "warn" | "confirm" | "fail" (default)
    pub on_failure: Option<String>,
    /// "always" | "on_success" (default)
    pub when: Option<String>,
    /// If true, failures are silently ignored
    pub optional: Option<bool>,
    /// Capture stdout into this variable name
    pub capture: Option<String>,
    pub poll: Option<PollConfig>,
    pub assert: Option<AssertConfig>,
}

/// Polling configuration for a step.
#[derive(Deserialize, Serialize, Clone)]
pub struct PollConfig {
    /// Poll interval: "30s" | "1m" | "5m"
    pub interval: String,
    /// Total timeout: "5m" | "1h"
    pub timeout: String,
    /// Condition to stop polling: "status == OK" | "value < 5" | "decreasing" | "empty"
    pub until: String,
}

/// Assertion configuration for a step.
#[derive(Deserialize, Serialize, Clone)]
pub struct AssertConfig {
    /// Assert that the output is empty
    pub empty: Option<bool>,
    /// Custom error message on assertion failure
    pub message: Option<String>,
}

/// Lightweight runbook metadata for `pup runbooks list`.
pub struct RunbookMeta {
    pub name: String,
    pub description: Option<String>,
    pub tags: HashMap<String, String>,
    pub steps: usize,
}
