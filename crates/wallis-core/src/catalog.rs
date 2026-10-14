//! Model catalog synced from the ChatGPT Codex backend (same shape Codex caches in `models_cache.json`).
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize)]
struct RawLevel { effort: String }

#[derive(Clone, Debug, Deserialize)]
struct RawTier { id: String }

#[derive(Clone, Debug, Deserialize)]
struct RawModel {
    slug: String,
    display_name: String,
    #[serde(default)] description: String,
    #[serde(default)] default_reasoning_level: Option<String>,
    #[serde(default)] supported_reasoning_levels: Vec<RawLevel>,
    #[serde(default)] visibility: String,
    #[serde(default)] priority: i64,
    #[serde(default)] service_tiers: Vec<RawTier>,
}

#[derive(Deserialize)]
struct RawCatalog { models: Vec<RawModel> }

/// One selectable model: its reasoning levels and whether Fast mode (`priority` service tier) exists.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexModel {
    pub id: String,
    pub label: String,
    pub description: String,
    pub efforts: Vec<String>,
    pub default_effort: String,
    pub supports_fast: bool,
}

/// Visible models only, best priority first. Hidden entries (internal review models) never reach the picker.
pub fn parse_codex_catalog(json: &str) -> Result<Vec<CodexModel>, String> {
    let mut raw: RawCatalog = serde_json::from_str(json).map_err(|error| format!("Codex returned an invalid model catalog: {error}"))?;
    raw.models.retain(|model| model.visibility == "list");
    raw.models.sort_by_key(|model| model.priority);
    if raw.models.is_empty() { return Err("Codex listed no models for this account.".into()); }
    Ok(raw.models.into_iter().map(|model| {
        let efforts: Vec<String> = model.supported_reasoning_levels.into_iter().map(|level| level.effort).collect();
        let default_effort = model.default_reasoning_level.filter(|level| efforts.contains(level)).or_else(|| efforts.first().cloned()).unwrap_or_default();
        CodexModel {
            supports_fast: model.service_tiers.iter().any(|tier| tier.id == "priority"),
            id: model.slug, label: model.display_name, description: model.description, efforts, default_effort,
        }
    }).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_visible_models_in_priority_order_with_their_levels() {
        let models = parse_codex_catalog(include_str!("../tests/fixtures/codex-models.json")).unwrap();
        assert_eq!(models.first().map(|model| model.id.as_str()), Some("gpt-6.1-sol"));
        assert!(models.iter().all(|model| model.id != "gpt-reserve" && model.id != "codex-auto-review"), "hidden models must not be offered");
        let luna = models.iter().find(|model| model.id == "gpt-6-luna").unwrap();
        assert!(!luna.efforts.contains(&"ultra".to_string()), "levels come from the catalog, not a fixed list");
        assert!(luna.supports_fast);
        assert!(luna.efforts.contains(&luna.default_effort));
    }

    #[test]
    fn reads_the_upstream_codex_models_file() {
        // openai/codex codex-rs/models-manager/models.json, the same shape the backend serves.
        let models = parse_codex_catalog(include_str!("../tests/fixtures/codex-models-upstream.json")).unwrap();
        assert!(!models.is_empty() && models.iter().all(|model| !model.efforts.is_empty() && model.efforts.contains(&model.default_effort)));
        assert!(models.iter().any(|model| model.supports_fast));
    }

    #[test]
    fn rejects_garbage_and_empty_catalogs() {
        assert!(parse_codex_catalog("{}").is_err());
        assert!(parse_codex_catalog(r#"{"models":[]}"#).is_err());
    }
}
