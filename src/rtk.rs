//! Token-efficient JSON compression for LLM agent mode.
//!
//! Schema extraction ported from rtk-ai/rtk (MIT License).
//! Source: https://github.com/rtk-ai/rtk/blob/main/src/json_cmd.rs
//! Copyright: Patrick Szymkowiak
//!
//! `compress_value` is a new addition: keeps real values but strips the fat
//! (nulls, long strings truncated, large arrays sampled) so the LLM sees
//! actionable data rather than type descriptors.
//!
//! ## Token-budget field selection
//!
//! Rather than a hardcoded field allowlist, each command declares `FieldWeights`
//! that score how important each field is (0.0–1.0). The algorithm fills a
//! `per_item_token_budget` greedily: fields are sorted by importance ÷ token_cost
//! (value density), highest first. Must-have fields (≥ 0.9) are truncated to fit
//! rather than dropped. This adapts to actual data sizes — a tiny `options` object
//! survives; a 2 KB one is dropped. Unlisted fields get `default_weight`.

use serde_json::Value;

// ---------------------------------------------------------------------------
// Field weights
// ---------------------------------------------------------------------------

/// Importance weights for token-budget field selection.
///
/// Fields are scored by `importance / token_cost` and filled greedily into
/// `CompressConfig::per_item_token_budget`. Fields with weight 0.0 are always
/// dropped; unlisted fields receive `default_weight`.
pub struct FieldWeights {
    /// `(field_name, importance)` pairs. Importance 1.0 = must-have (truncated
    /// to fit), 0.0 = always drop, anything in between is included by density.
    pub weights: &'static [(&'static str, f32)],
    /// Weight assigned to fields not in the list above (default: 0.3).
    pub default_weight: f32,
}

impl FieldWeights {
    fn importance(&self, key: &str) -> f32 {
        self.weights
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, w)| *w)
            .unwrap_or(self.default_weight)
    }
}

// Per-command weight profiles -------------------------------------------------

pub static MONITOR_WEIGHTS: FieldWeights = FieldWeights {
    weights: &[
        ("id", 1.0),
        ("name", 1.0),
        ("overall_state", 1.0),
        ("type", 0.9),
        ("query", 0.85),
        ("tags", 0.75),
        ("message", 0.70),
        ("creator", 0.50),
        ("notifications", 0.45),
        ("modified", 0.40),
        ("created_at", 0.20),
        ("created", 0.15),
        ("overall_state_modified", 0.10),
        ("draft_status", 0.10),
        // expensive objects with low diagnostic value for triage
        ("options", 0.05),
        ("org_id", 0.05),
        ("multi", 0.05),
        ("matching_downtimes", 0.02),
    ],
    default_weight: 0.30,
};

pub static LOG_WEIGHTS: FieldWeights = FieldWeights {
    weights: &[
        ("timestamp", 1.0),
        ("message", 1.0),
        ("service", 1.0),
        ("status", 1.0),
        ("host", 0.80),
        ("id", 0.70),
        ("tags", 0.50),
        ("env", 0.45),
        ("version", 0.35),
    ],
    default_weight: 0.10,
};

pub static SPAN_WEIGHTS: FieldWeights = FieldWeights {
    weights: &[
        ("service", 1.0),
        ("status", 1.0),
        ("resource_name", 1.0),
        ("error_type", 1.0),
        ("trace_id", 0.90),
        ("operation_name", 0.85),
        ("span_id", 0.80),
        ("start_timestamp", 0.70),
        ("end_timestamp", 0.60),
        ("env", 0.60),
        ("host", 0.50),
        ("id", 0.50),
    ],
    default_weight: 0.10,
};

pub static INCIDENT_WEIGHTS: FieldWeights = FieldWeights {
    weights: &[
        ("id", 1.0),
        ("title", 1.0),
        ("severity", 1.0),
        ("state", 1.0),
        ("created", 1.0),
        ("commander", 0.80),
        ("created_by", 0.60),
        ("customer_impacted", 0.60),
        ("resolved", 0.50),
        ("postmortem_id", 0.30),
        // The raw `fields` object contains schema metadata, not values — low value
        ("fields", 0.02),
        ("field_analytics", 0.02),
    ],
    default_weight: 0.25,
};

pub static EVENT_WEIGHTS: FieldWeights = FieldWeights {
    weights: &[
        ("title", 1.0),
        ("timestamp", 1.0),
        ("message", 0.80),
        ("tags", 0.60),
        ("id", 0.50),
        ("source", 0.50),
        // deep internal metadata bags
        ("_dd", 0.02),
        ("evt", 0.10),
    ],
    default_weight: 0.20,
};

// ---------------------------------------------------------------------------
// CompressConfig
// ---------------------------------------------------------------------------

/// Tunable parameters for JSON compression.
#[derive(Clone)]
pub struct CompressConfig {
    /// Truncate strings longer than this many bytes (default: 200).
    pub string_trunc: usize,
    /// Max items to show from a top-level array, e.g. a list response (default: 20).
    pub array_items_top: usize,
    /// Max items to show from nested arrays, e.g. tags (default: 10).
    pub array_items_nested: usize,
    /// Optional structural flatten applied before field selection.
    /// Used for responses that nest relevant fields inside an `attributes` wrapper
    /// (logs, spans). For arrays, called on every element.
    pub flatten: Option<fn(&Value) -> Value>,
    /// Token-budget-aware field selection applied to each top-level item.
    /// Fields are scored by importance ÷ token_cost and filled greedily.
    pub field_weights: Option<&'static FieldWeights>,
    /// Approximate token budget per top-level object item (default: 300 ≈ 1200 chars).
    pub per_item_token_budget: usize,
}

impl Default for CompressConfig {
    fn default() -> Self {
        Self {
            string_trunc: 200,
            array_items_top: 20,
            array_items_nested: 10,
            flatten: None,
            field_weights: None,
            per_item_token_budget: 300,
        }
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Compress a JSON string: optionally flatten structure, apply token-budget
/// field selection, strip nulls, truncate long strings, sample large arrays.
/// Returns compact (non-pretty) JSON so the caller controls formatting.
pub fn compress_json_string(json_str: &str, cfg: &CompressConfig) -> anyhow::Result<String> {
    let value: Value = serde_json::from_str(json_str)?;
    let flattened = apply_flatten(&value, cfg);
    let compressed = compress_value(&flattened, 0, cfg);
    Ok(serde_json::to_string(&compressed)?)
}

/// Return the field weights profile for a given pup command string, if one exists.
pub fn weights_for_command(command: &str) -> Option<&'static FieldWeights> {
    match command {
        "monitors list" | "monitors get" => Some(&MONITOR_WEIGHTS),
        "logs search" => Some(&LOG_WEIGHTS),
        "traces search" | "traces aggregate" => Some(&SPAN_WEIGHTS),
        "incidents list" | "incidents get" => Some(&INCIDENT_WEIGHTS),
        "events search" => Some(&EVENT_WEIGHTS),
        _ => None,
    }
}

/// Return the structural flatten function for a given pup command string, if one exists.
pub fn flatten_for_command(command: &str) -> Option<fn(&Value) -> Value> {
    match command {
        "logs search" => Some(flatten_log),
        "traces search" | "traces aggregate" => Some(flatten_span),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Structural flatteners (lift attributes wrapper → flat object)
// ---------------------------------------------------------------------------

/// Log: lift attributes.{timestamp, message, service, status, host, tags} to top level.
/// Drops attributes.attributes (the verbose custom bag).
pub fn flatten_log(v: &Value) -> Value {
    let mut out = serde_json::Map::new();
    if let Some(id) = v.get("id") {
        out.insert("id".into(), id.clone());
    }
    if let Some(attrs) = v.get("attributes") {
        for &field in &[
            "timestamp",
            "message",
            "service",
            "status",
            "host",
            "tags",
            "env",
            "version",
        ] {
            if let Some(val) = attrs.get(field) {
                out.insert(field.into(), val.clone());
            }
        }
    }
    Value::Object(out)
}

/// Span: lift relevant attributes fields to top level, drop the verbose `custom` bag.
pub fn flatten_span(v: &Value) -> Value {
    let mut out = serde_json::Map::new();
    if let Some(id) = v.get("id") {
        out.insert("id".into(), id.clone());
    }
    if let Some(attrs) = v.get("attributes") {
        for &field in &[
            "trace_id",
            "span_id",
            "service",
            "operation_name",
            "resource_name",
            "status",
            "start_timestamp",
            "end_timestamp",
            "env",
            "host",
        ] {
            if let Some(val) = attrs.get(field) {
                out.insert(field.into(), val.clone());
            }
        }
        // Lift error type without the full stack trace.
        if let Some(err) = attrs.get("error") {
            if let Some(t) = err.get("type") {
                out.insert("error_type".into(), t.clone());
            }
        }
    }
    Value::Object(out)
}

// ---------------------------------------------------------------------------
// Core compression
// ---------------------------------------------------------------------------

fn apply_flatten(value: &Value, cfg: &CompressConfig) -> Value {
    let f = match cfg.flatten {
        Some(f) => f,
        None => return value.clone(),
    };
    match value {
        Value::Array(arr) => Value::Array(arr.iter().map(f).collect()),
        other => f(other),
    }
}

fn compress_value(value: &Value, depth: u8, cfg: &CompressConfig) -> Value {
    match value {
        Value::Null => Value::Null,
        Value::Bool(b) => Value::Bool(*b),
        Value::Number(n) => Value::Number(n.clone()),
        Value::String(s) => {
            if s.len() > cfg.string_trunc {
                Value::String(format!("{}...[{} chars]", &s[..cfg.string_trunc], s.len()))
            } else {
                Value::String(s.clone())
            }
        }
        Value::Array(arr) => {
            let limit = if depth == 0 {
                cfg.array_items_top
            } else {
                cfg.array_items_nested
            };
            let mut items: Vec<Value> = arr
                .iter()
                .take(limit)
                .map(|v| compress_value(v, depth + 1, cfg))
                .filter(|v| !v.is_null())
                .collect();
            if arr.len() > limit {
                items.push(Value::String(format!("... +{} more", arr.len() - limit)));
            }
            Value::Array(items)
        }
        Value::Object(map) => {
            // Apply token-budget field selection at the item level (depth 0 = single object,
            // depth 1 = items inside a top-level array). Deeper objects use plain null-stripping.
            if depth <= 1 {
                if let Some(fw) = cfg.field_weights {
                    return token_budget_compress_object(
                        map,
                        cfg.per_item_token_budget,
                        fw,
                        depth,
                        cfg,
                    );
                }
            }
            // Plain null-stripping for deeper nested objects or when no weights defined.
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                if v.is_null() {
                    continue;
                }
                let c = compress_value(v, depth + 1, cfg);
                if !c.is_null() {
                    out.insert(k.clone(), c);
                }
            }
            Value::Object(out)
        }
    }
}

/// Token-budget greedy field selector.
///
/// Scores each non-null field by `importance / estimated_tokens` (value density),
/// then fills the budget highest-density-first. Must-have fields (importance ≥ 0.9)
/// are truncated to fit remaining budget rather than silently dropped.
fn token_budget_compress_object(
    map: &serde_json::Map<String, Value>,
    budget: usize,
    fw: &FieldWeights,
    depth: u8,
    cfg: &CompressConfig,
) -> Value {
    // Score all non-null fields.
    let mut candidates: Vec<(&str, &Value, f32, usize)> = map
        .iter()
        .filter(|(_, v)| !v.is_null())
        .map(|(k, v)| {
            let importance = fw.importance(k.as_str());
            let tokens = estimate_tokens(v);
            (k.as_str(), v, importance, tokens)
        })
        .filter(|(_, _, imp, _)| *imp > 0.0) // 0.0 = explicit drop
        .collect();

    // Sort: importance desc first, then by value density (importance/tokens) desc.
    candidates.sort_by(|a, b| {
        b.2.partial_cmp(&a.2)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                let da = a.2 / (a.3 as f32 + 1.0);
                let db = b.2 / (b.3 as f32 + 1.0);
                db.partial_cmp(&da).unwrap_or(std::cmp::Ordering::Equal)
            })
    });

    let mut out = serde_json::Map::new();
    let mut used = 0usize;

    for (key, val, importance, cost) in candidates {
        if used >= budget {
            break;
        }
        if used + cost <= budget {
            // Fits: include with standard compression applied.
            out.insert(key.to_owned(), compress_value(val, depth + 1, cfg));
            used += cost;
        } else if importance >= 0.9 {
            // Must-have: truncate string to remaining budget, or include small primitives.
            let remaining_chars = (budget - used).saturating_mul(4);
            match val {
                Value::String(s) if s.len() > remaining_chars => {
                    let safe_end = truncate_to_char_boundary(s, remaining_chars);
                    out.insert(
                        key.to_owned(),
                        Value::String(format!("{}...[{} chars]", &s[..safe_end], s.len())),
                    );
                    used = budget;
                }
                _ if cost <= 20 => {
                    // Tiny value: include even if slightly over budget.
                    out.insert(key.to_owned(), compress_value(val, depth + 1, cfg));
                    used += cost;
                }
                _ => {} // Large must-have that can't be truncated: skip.
            }
        }
        // else: doesn't fit AND not must-have → drop.
    }

    Value::Object(out)
}

/// Estimate token count using the ~4 chars/token heuristic.
fn estimate_tokens(v: &Value) -> usize {
    let chars = serde_json::to_string(v).unwrap_or_default().len();
    chars.div_ceil(4)
}

/// Find the largest byte index ≤ `max_bytes` that falls on a UTF-8 char boundary.
fn truncate_to_char_boundary(s: &str, max_bytes: usize) -> usize {
    if max_bytes >= s.len() {
        return s.len();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    end
}

// ---------------------------------------------------------------------------
// Schema extraction — ported verbatim from rtk-ai/rtk json_cmd.rs.
// Kept for potential future use (e.g. `pup --schema` flag).
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub fn filter_json_string(json_str: &str) -> anyhow::Result<String> {
    let value: Value = serde_json::from_str(json_str)?;
    Ok(extract_schema(&value, 0, 5))
}

#[allow(dead_code)]
fn extract_schema(value: &Value, depth: usize, max_depth: usize) -> String {
    let indent = "  ".repeat(depth);

    if depth > max_depth {
        return format!("{}...", indent);
    }

    match value {
        Value::Null => format!("{}null", indent),
        Value::Bool(_) => format!("{}bool", indent),
        Value::Number(n) => {
            if n.is_i64() {
                format!("{}int", indent)
            } else {
                format!("{}float", indent)
            }
        }
        Value::String(s) => {
            if s.len() > 50 {
                format!("{}string[{}]", indent, s.len())
            } else if s.is_empty() {
                format!("{}string", indent)
            } else if s.starts_with("http") {
                format!("{}url", indent)
            } else if s.contains('-') && s.len() == 10 {
                format!("{}date?", indent)
            } else {
                format!("{}string", indent)
            }
        }
        Value::Array(arr) => {
            if arr.is_empty() {
                format!("{}[]", indent)
            } else {
                let first_schema = extract_schema(&arr[0], depth + 1, max_depth);
                let trimmed = first_schema.trim();
                if arr.len() == 1 {
                    format!("{}[\n{}\n{}]", indent, first_schema, indent)
                } else {
                    format!("{}[{}] ({})", indent, trimmed, arr.len())
                }
            }
        }
        Value::Object(map) => {
            if map.is_empty() {
                return format!("{}{{}}", indent);
            }
            let mut lines = vec![format!("{}{{", indent)];
            let mut keys: Vec<_> = map.keys().collect();
            keys.sort();

            for (i, key) in keys.iter().enumerate() {
                let val = &map[*key];
                let val_schema = extract_schema(val, depth + 1, max_depth);
                let val_trimmed = val_schema.trim();

                let is_simple = matches!(
                    val,
                    Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_)
                );

                if is_simple {
                    if i < keys.len() - 1 {
                        lines.push(format!("{}  {}: {},", indent, key, val_trimmed));
                    } else {
                        lines.push(format!("{}  {}: {}", indent, key, val_trimmed));
                    }
                } else {
                    lines.push(format!("{}  {}:", indent, key));
                    lines.push(val_schema);
                }

                if i >= 15 {
                    lines.push(format!("{}  ... +{} more keys", indent, keys.len() - i - 1));
                    break;
                }
            }
            lines.push(format!("{}}}", indent));
            lines.join("\n")
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn default_cfg() -> CompressConfig {
        CompressConfig::default()
    }

    // --- FieldWeights tests ---

    #[test]
    fn test_field_weights_importance_known() {
        assert_eq!(MONITOR_WEIGHTS.importance("id"), 1.0);
        assert_eq!(MONITOR_WEIGHTS.importance("overall_state"), 1.0);
        assert_eq!(MONITOR_WEIGHTS.importance("options"), 0.05);
        assert_eq!(MONITOR_WEIGHTS.importance("matching_downtimes"), 0.02);
    }

    #[test]
    fn test_field_weights_importance_unknown_uses_default() {
        assert_eq!(MONITOR_WEIGHTS.importance("some_new_api_field"), 0.30);
        assert_eq!(LOG_WEIGHTS.importance("some_new_api_field"), 0.10);
    }

    #[test]
    fn test_weights_for_command_routing() {
        assert!(weights_for_command("monitors list").is_some());
        assert!(weights_for_command("monitors get").is_some());
        assert!(weights_for_command("logs search").is_some());
        assert!(weights_for_command("traces search").is_some());
        assert!(weights_for_command("traces aggregate").is_some());
        assert!(weights_for_command("incidents list").is_some());
        assert!(weights_for_command("events search").is_some());
        assert!(weights_for_command("dashboards list").is_none());
    }

    #[test]
    fn test_flatten_for_command_routing() {
        assert!(flatten_for_command("logs search").is_some());
        assert!(flatten_for_command("traces search").is_some());
        assert!(flatten_for_command("monitors list").is_none());
    }

    // --- token_budget_compress_object ---

    #[test]
    fn test_budget_drops_low_importance_fields_when_tight() {
        let cfg = CompressConfig {
            per_item_token_budget: 20, // very tight
            field_weights: Some(&MONITOR_WEIGHTS),
            ..default_cfg()
        };
        let monitor = serde_json::json!({
            "id": 123,
            "name": "High CPU",
            "overall_state": "Alert",
            "options": {"avalanche_window": 20, "include_tags": true, "thresholds": {"critical": 90.0}},
            "org_id": 456,
            "matching_downtimes": [],
        });
        let compressed = compress_json_string(
            &serde_json::to_string(&serde_json::json!([monitor])).unwrap(),
            &cfg,
        )
        .unwrap();
        let result: Value = serde_json::from_str(&compressed).unwrap();
        let item = &result[0];
        // High-importance fields should be kept
        assert!(item.get("id").is_some(), "id must be kept");
        assert!(item.get("name").is_some(), "name must be kept");
        assert!(
            item.get("overall_state").is_some(),
            "overall_state must be kept"
        );
        // The large options object (~17 tokens) should be dropped when budget is tight
        assert!(
            item.get("options").is_none(),
            "options should be dropped under tight budget"
        );
    }

    #[test]
    fn test_budget_keeps_small_low_importance_fields_when_room() {
        let cfg = CompressConfig {
            per_item_token_budget: 500, // generous
            field_weights: Some(&MONITOR_WEIGHTS),
            ..default_cfg()
        };
        // With a generous budget, small low-importance fields should survive
        let monitor = serde_json::json!({
            "id": 123,
            "name": "test",
            "overall_state": "OK",
            "org_id": 456,   // small, low importance
        });
        let compressed = compress_json_string(
            &serde_json::to_string(&serde_json::json!([monitor])).unwrap(),
            &cfg,
        )
        .unwrap();
        let result: Value = serde_json::from_str(&compressed).unwrap();
        let item = &result[0];
        assert!(item.get("id").is_some());
        // org_id is small (fits easily) even at low importance
        assert!(
            item.get("org_id").is_some(),
            "small low-importance field fits in generous budget"
        );
    }

    #[test]
    fn test_budget_must_have_field_truncated_not_dropped() {
        let cfg = CompressConfig {
            per_item_token_budget: 15, // extremely tight — only ~60 chars
            field_weights: Some(&MONITOR_WEIGHTS),
            ..default_cfg()
        };
        let long_name = "A".repeat(200);
        let monitor = serde_json::json!({
            "id": 1,
            "name": long_name,
            "overall_state": "Alert",
        });
        let compressed = compress_json_string(
            &serde_json::to_string(&serde_json::json!([monitor])).unwrap(),
            &cfg,
        )
        .unwrap();
        let result: Value = serde_json::from_str(&compressed).unwrap();
        let item = &result[0];
        // name is must-have (1.0) — it should appear truncated, not dropped
        let name_val = item
            .get("name")
            .expect("name must be present even under tight budget");
        let name_str = name_val.as_str().unwrap();
        assert!(
            name_str.contains("...[200 chars]"),
            "name should be truncated: {name_str}"
        );
    }

    #[test]
    fn test_budget_drops_zero_weight_fields() {
        let cfg = CompressConfig {
            per_item_token_budget: 1000, // very generous
            field_weights: Some(&MONITOR_WEIGHTS),
            ..default_cfg()
        };
        let monitor = serde_json::json!({
            "id": 1,
            "name": "test",
            "overall_state": "OK",
            "matching_downtimes": [1, 2, 3], // weight 0.02 but non-null — should drop at budget
        });
        // matching_downtimes has weight 0.02 — with a very generous budget it might survive,
        // but more importantly confirm 0.0 fields (if any) are always dropped.
        // Let's test with a custom weight profile that has a 0.0 field.
        static WEIGHTS_WITH_ZERO: FieldWeights = FieldWeights {
            weights: &[("id", 1.0), ("secret_field", 0.0), ("name", 0.9)],
            default_weight: 0.5,
        };
        let cfg2 = CompressConfig {
            per_item_token_budget: 1000,
            field_weights: Some(&WEIGHTS_WITH_ZERO),
            ..default_cfg()
        };
        let obj = serde_json::json!({
            "id": 1,
            "name": "test",
            "secret_field": "should never appear",
        });
        let compressed = compress_json_string(
            &serde_json::to_string(&serde_json::json!([obj])).unwrap(),
            &cfg2,
        )
        .unwrap();
        let result: Value = serde_json::from_str(&compressed).unwrap();
        assert!(
            result[0].get("secret_field").is_none(),
            "weight 0.0 field should always be dropped"
        );
        assert!(result[0].get("id").is_some());
        assert!(result[0].get("name").is_some());
        drop(cfg); // suppress unused warning
    }

    // --- flatten_log / flatten_span ---

    #[test]
    fn test_flatten_log_lifts_attributes() {
        let log = serde_json::json!({
            "id": "abc",
            "type": "log",
            "attributes": {
                "timestamp": "2026-03-11T18:00:00Z",
                "message": "something failed",
                "service": "my-svc",
                "status": "error",
                "host": "host-1",
                "tags": ["env:prod"],
                "attributes": {"caller": "main.go:42", "level": "ERROR"}
            }
        });
        let flat = flatten_log(&log);
        let m = flat.as_object().unwrap();
        assert_eq!(m["id"].as_str().unwrap(), "abc");
        assert_eq!(m["message"].as_str().unwrap(), "something failed");
        assert_eq!(m["service"].as_str().unwrap(), "my-svc");
        assert!(
            !m.contains_key("attributes"),
            "nested custom bag should not appear at top level"
        );
        assert!(!m.contains_key("type"), "type wrapper dropped");
    }

    #[test]
    fn test_flatten_span_lifts_attributes() {
        let span = serde_json::json!({
            "id": "span1",
            "attributes": {
                "service": "api",
                "status": "error",
                "error": {"type": "RuntimeError", "message": "oops", "stack": "long..."},
                "custom": {"lots": "of", "noise": "here"}
            }
        });
        let flat = flatten_span(&span);
        let m = flat.as_object().unwrap();
        assert_eq!(m["service"].as_str().unwrap(), "api");
        assert_eq!(m["error_type"].as_str().unwrap(), "RuntimeError");
        assert!(!m.contains_key("custom"), "custom bag dropped");
    }

    // --- compress_value primitives / arrays (unchanged behaviour) ---

    #[test]
    fn test_compress_drops_nulls() {
        let obj = serde_json::json!({"id": 1, "deleted": null, "name": "foo"});
        let c = compress_value(&obj, 2, &default_cfg()); // depth=2 → plain null-strip
        let m = c.as_object().unwrap();
        assert!(m.contains_key("id"));
        assert!(m.contains_key("name"));
        assert!(!m.contains_key("deleted"));
    }

    #[test]
    fn test_compress_truncates_long_string() {
        let long = "x".repeat(300);
        let c = compress_value(&Value::String(long), 0, &default_cfg());
        let s = c.as_str().unwrap();
        assert!(s.contains("...[300 chars]"));
        assert!(s.len() < 300);
    }

    #[test]
    fn test_compress_array_top_level_sampled() {
        let arr: Vec<Value> = (0..30).map(|i| serde_json::json!(i)).collect();
        let c = compress_value(&Value::Array(arr), 0, &default_cfg());
        let items = c.as_array().unwrap();
        assert_eq!(items.len(), 21); // 20 + sentinel
        assert_eq!(items.last().unwrap().as_str().unwrap(), "... +10 more");
    }

    #[test]
    fn test_compress_array_nested_sampled() {
        let arr: Vec<Value> = (0..15).map(|i| serde_json::json!(i)).collect();
        let c = compress_value(&Value::Array(arr), 1, &default_cfg());
        let items = c.as_array().unwrap();
        assert_eq!(items.len(), 11); // 10 + sentinel
    }

    #[test]
    fn test_compress_smaller_than_original() {
        let json = r#"[{"id":1,"name":"High CPU","message":"CPU above 90% on host for 5 minutes. Check runaway processes or traffic spikes immediately. Alert will resolve when CPU drops below threshold for 3 consecutive minutes.","tags":["env:prod","team:api"],"status":"Alert","deleted":null,"draft":null,"restricted_roles":null,"creator":null,"priority":null,"org_id":null}]"#;
        let compressed = compress_json_string(json, &default_cfg()).unwrap();
        assert!(
            compressed.len() < json.len(),
            "compressed ({}) should be smaller than original ({})",
            compressed.len(),
            json.len()
        );
    }

    // --- estimate_tokens ---

    #[test]
    fn test_estimate_tokens_reasonable() {
        // ~4 chars per token
        assert_eq!(estimate_tokens(&serde_json::json!("hello")), 2); // 7 chars → 2 tokens
        assert_eq!(estimate_tokens(&serde_json::json!(42)), 1);
        let long = serde_json::json!("a".repeat(400));
        assert!(estimate_tokens(&long) >= 100);
    }

    // --- extract_schema tests (RTK port) ---

    #[test]
    fn test_extract_schema_primitives() {
        assert_eq!(extract_schema(&serde_json::json!(null), 0, 5), "null");
        assert_eq!(extract_schema(&serde_json::json!(true), 0, 5), "bool");
        assert_eq!(extract_schema(&serde_json::json!(42), 0, 5), "int");
        assert_eq!(extract_schema(&serde_json::json!(3.7), 0, 5), "float");
    }

    #[test]
    fn test_extract_schema_string_short() {
        assert_eq!(extract_schema(&serde_json::json!("hello"), 0, 5), "string");
    }

    #[test]
    fn test_extract_schema_string_long() {
        let long = "a".repeat(60);
        assert_eq!(extract_schema(&serde_json::json!(long), 0, 5), "string[60]");
    }

    #[test]
    fn test_extract_schema_string_url() {
        assert_eq!(
            extract_schema(&serde_json::json!("https://app.datadoghq.com"), 0, 5),
            "url"
        );
    }

    #[test]
    fn test_extract_schema_string_date() {
        assert_eq!(
            extract_schema(&serde_json::json!("2024-03-11"), 0, 5),
            "date?"
        );
    }

    #[test]
    fn test_extract_schema_empty_array() {
        assert_eq!(extract_schema(&serde_json::json!([]), 0, 5), "[]");
    }

    #[test]
    fn test_extract_schema_array_single() {
        let arr = serde_json::json!([42]);
        let result = extract_schema(&arr, 0, 5);
        assert!(result.contains("int"));
        assert!(result.starts_with('['));
    }

    #[test]
    fn test_extract_schema_array_multi() {
        let arr = serde_json::json!(["env:prod", "team:api", "service:web"]);
        assert_eq!(extract_schema(&arr, 0, 5), "[string] (3)");
    }

    #[test]
    fn test_extract_schema_object() {
        let obj = serde_json::json!({"id": 42, "name": "monitor"});
        let result = extract_schema(&obj, 0, 5);
        assert!(result.contains("id: int"));
        assert!(result.contains("name: string"));
    }

    #[test]
    fn test_extract_schema_depth_limit() {
        let deep = serde_json::json!({"a": 1});
        let result = extract_schema(&deep, 0, 0);
        assert!(result.contains("a: ..."), "got: {result}");
    }

    #[test]
    fn test_filter_json_string_roundtrip() {
        let json = r#"{"name": "test", "count": 42, "tags": ["a", "b"]}"#;
        let result = filter_json_string(json).unwrap();
        assert!(result.contains("name: string"));
        assert!(result.contains("count: int"));
        assert!(result.contains("[string] (2)"));
    }

    #[test]
    fn test_filter_json_string_invalid() {
        assert!(filter_json_string("not json").is_err());
    }
}
