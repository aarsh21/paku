//! Harness + model catalogs. The phone mirrors its run device's live Pi
//! catalog (`ListHarnesses` / `ListModels` over the relay), falling back to
//! Pi's configured default when the execution device is unreachable.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessInfo {
    /// Wire id (`pi`; `mock` is reserved for tests).
    #[serde(deserialize_with = "deserialize_harness_id")]
    pub id: String,
    #[serde(alias = "name")]
    pub label: String,
    #[serde(default)]
    pub supports_steering: Option<bool>,
    /// `step-boundary` / `turn-boundary`.
    #[serde(default)]
    pub steering_mode: Option<String>,
    #[serde(default)]
    pub reasoning_levels: Vec<String>,
    #[serde(default = "default_true")]
    pub installed: bool,
    #[serde(default)]
    pub enabled: Option<bool>,
}

fn deserialize_harness_id<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let id = String::deserialize(d)?;
    match id.as_str() {
        "pi" | "mock" => Ok(id),
        _ => Err(serde::de::Error::custom(format!(
            "unsupported harness: {id}"
        ))),
    }
}

fn default_true() -> bool {
    true
}

impl HarnessInfo {
    fn fallback(id: &str, label: &str) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            supports_steering: None,
            steering_mode: None,
            reasoning_levels: Vec::new(),
            installed: true,
            enabled: None,
        }
    }

    /// Mid-turn steering is available (known only from a live catalog).
    pub fn mid_turn_steering(&self) -> Option<bool> {
        Some(self.supports_steering? && self.steering_mode.as_deref()? == "step-boundary")
    }

    pub fn offered(&self) -> bool {
        matches!(self.id.as_str(), "pi" | "mock") && self.installed && self.enabled != Some(false)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelOptionChoice {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelOption {
    pub id: String,
    pub label: String,
    pub choices: Vec<ModelOptionChoice>,
    pub default_choice: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Pi's model-specific effort ladder; empty = no reasoning support.
    #[serde(default)]
    pub reasoning_levels: Vec<String>,
    #[serde(default)]
    pub options: Vec<ModelOption>,
}

fn model(
    id: &str,
    label: &str,
    description: &str,
    ladder: &[&str],
    options: Vec<ModelOption>,
) -> ModelInfo {
    ModelInfo {
        id: id.into(),
        label: label.into(),
        description: Some(description.into()),
        reasoning_levels: ladder.iter().map(|s| (*s).to_owned()).collect(),
        options,
    }
}

/// Pi is the only production harness. Mock is never shown in the picker.
pub fn fallback_harnesses() -> Vec<HarnessInfo> {
    vec![HarnessInfo::fallback("pi", "Pi")]
}

pub fn harness_label(id: &str) -> String {
    match id {
        "pi" => "Pi".into(),
        "mock" => "Mock".into(),
        other => other.to_owned(),
    }
}

/// Real provider/model ids must come from Pi's live device-local catalog.
/// In particular, an unsupported legacy harness never gets Pi's models.
pub fn fallback_models(harness: &str) -> Vec<ModelInfo> {
    match harness {
        "pi" | "mock" => vec![model(
            "default",
            "Pi default",
            "Runs the model configured in Pi settings",
            &["minimal", "low", "medium", "high", "xhigh", "max"],
            vec![],
        )],
        _ => vec![],
    }
}

pub fn default_reasoning(model: &ModelInfo) -> Option<String> {
    let levels = &model.reasoning_levels;
    for preferred in ["high", "medium"] {
        if levels.iter().any(|l| l == preferred) {
            return Some(preferred.into());
        }
    }
    levels.first().cloned()
}

pub fn reasoning_label(level: &str) -> String {
    match level {
        "xhigh" => "X-High".into(),
        other => {
            let mut chars = other.chars();
            chars
                .next()
                .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default()
        }
    }
}

/// Remove a default placeholder only when the live catalog has real models.
/// Preserve Pi's exact provider-qualified ids, labels and options; suffixes
/// such as `[1m]` are model identity, not a client-created context option.
pub fn normalize_models(harness: &str, models: Vec<ModelInfo>) -> Vec<ModelInfo> {
    if !matches!(harness, "pi" | "mock") {
        return vec![];
    }
    let has_real = models.iter().any(|m| m.id != "default");
    models
        .into_iter()
        .filter(|m| !has_real || m.id != "default")
        .collect()
}

/// Display labels learned from live Pi catalogs.
static LEARNED_LABELS: std::sync::LazyLock<
    std::sync::RwLock<std::collections::HashMap<(String, String), String>>,
> = std::sync::LazyLock::new(Default::default);

pub(crate) fn learn_labels(harness: &str, models: &[ModelInfo]) {
    if !matches!(harness, "pi" | "mock") {
        return;
    }
    let Ok(mut map) = LEARNED_LABELS.write() else {
        return;
    };
    for m in models {
        if !m.label.trim().is_empty() && m.label != m.id {
            map.insert((harness.to_owned(), m.id.clone()), m.label.clone());
        }
    }
}

pub fn model_label(harness: &str, model_id: &str) -> String {
    if let Some(found) = fallback_models(harness).iter().find(|m| m.id == model_id) {
        return found.label.clone();
    }
    LEARNED_LABELS
        .read()
        .ok()
        .and_then(|map| map.get(&(harness.to_owned(), model_id.to_owned())).cloned())
        .unwrap_or_else(|| model_id.to_owned())
}

/// Last-known live catalogs on disk (`{data_dir}/catalogs/{device}.json`).
#[derive(Debug, Clone)]
pub(crate) struct DiskCatalog {
    dir: std::path::PathBuf,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct DeviceCatalog {
    #[serde(default)]
    harnesses: Option<Vec<HarnessInfo>>,
    #[serde(default)]
    models: std::collections::BTreeMap<String, Vec<ModelInfo>>,
}

fn file_safe(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

impl DiskCatalog {
    pub(crate) fn new(data_dir: &std::path::Path) -> Self {
        Self {
            dir: data_dir.join("catalogs"),
        }
    }

    fn path(&self, device_id: &str) -> std::path::PathBuf {
        self.dir.join(format!("{}.json", file_safe(device_id)))
    }

    fn load(&self, device_id: &str) -> DeviceCatalog {
        std::fs::read(self.path(device_id))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn store(&self, device_id: &str, catalog: &DeviceCatalog) {
        let write = || -> std::io::Result<()> {
            std::fs::create_dir_all(&self.dir)?;
            let path = self.path(device_id);
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, serde_json::to_vec(catalog).unwrap_or_default())?;
            std::fs::rename(tmp, path)
        };
        if let Err(err) = write() {
            tracing::debug!(error = %err, "catalog cache write failed");
        }
    }

    pub(crate) fn harnesses(&self, device_id: &str) -> Option<Vec<HarnessInfo>> {
        self.load(device_id).harnesses
    }

    pub(crate) fn put_harnesses(&self, device_id: &str, list: &[HarnessInfo]) {
        let mut catalog = self.load(device_id);
        catalog.harnesses = Some(list.to_vec());
        self.store(device_id, &catalog);
    }

    pub(crate) fn models(&self, device_id: &str, harness: &str) -> Option<Vec<ModelInfo>> {
        if !matches!(harness, "pi" | "mock") {
            return None;
        }
        self.load(device_id).models.remove(harness)
    }

    pub(crate) fn warm_labels(&self) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(bytes) = std::fs::read(entry.path()) else {
                continue;
            };
            let Ok(catalog) = serde_json::from_slice::<DeviceCatalog>(&bytes) else {
                continue;
            };
            for (harness, models) in &catalog.models {
                learn_labels(harness, models);
            }
        }
    }

    pub(crate) fn put_models(&self, device_id: &str, harness: &str, list: &[ModelInfo]) {
        if !matches!(harness, "pi" | "mock") {
            return;
        }
        learn_labels(harness, list);
        let mut catalog = self.load(device_id);
        catalog.models.insert(harness.to_owned(), list.to_vec());
        self.store(device_id, &catalog);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_pi_is_offered_as_a_production_fallback() {
        assert_eq!(
            fallback_harnesses()
                .iter()
                .map(|h| h.id.as_str())
                .collect::<Vec<_>>(),
            ["pi"]
        );
        assert_eq!(harness_label("pi"), "Pi");
        assert_eq!(model_label("pi", "default"), "Pi default");
        assert_eq!(model_label("pi", "some-new-model"), "some-new-model");
    }

    #[test]
    fn unsupported_harnesses_are_not_decoded_or_given_models() {
        for id in [
            "claude-code",
            "codex",
            "cursor",
            "opencode",
            "devin",
            "grok",
            "hermes",
            "antigravity",
            "future-harness",
        ] {
            assert!(
                serde_json::from_value::<HarnessInfo>(serde_json::json!({"id":id,"name":id}))
                    .is_err(),
                "{id}"
            );
            assert!(fallback_models(id).is_empty(), "{id}");
            assert!(
                normalize_models(id, fallback_models("pi")).is_empty(),
                "{id}"
            );
        }
    }

    #[test]
    fn live_provider_catalogs_keep_exact_identity_and_options() {
        let live = vec![
            model(
                "anthropic/claude-opus-5[1m]",
                "Opus 5 (1M)",
                "Anthropic",
                &["high"],
                vec![],
            ),
            model("openai/gpt-5.4", "GPT-5.4", "OpenAI", &[], vec![]),
        ];
        let mut with_default = live.clone();
        with_default.push(fallback_models("pi").remove(0));
        assert_eq!(normalize_models("pi", with_default), live);
        learn_labels("pi", &live);
        assert_eq!(
            model_label("pi", "anthropic/claude-opus-5[1m]"),
            "Opus 5 (1M)"
        );
    }

    #[test]
    fn disk_catalog_round_trips_per_device_pi_models() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCatalog::new(dir.path());
        assert!(cache.harnesses("dev/mac").is_none());
        cache.put_harnesses("dev/mac", &fallback_harnesses());
        let local = vec![model("anthropic/opus", "Opus", "Anthropic", &[], vec![])];
        let remote = vec![model("openai/gpt-5.4", "GPT-5.4", "OpenAI", &[], vec![])];
        cache.put_models("dev/mac", "pi", &local);
        cache.put_models("dev/worker", "pi", &remote);
        assert_eq!(cache.harnesses("dev/mac").unwrap().len(), 1);
        assert_eq!(cache.models("dev/mac", "pi").unwrap(), local);
        assert_eq!(cache.models("dev/worker", "pi").unwrap(), remote);
        assert!(cache.models("dev/mac", "codex").is_none());
    }
}
