//! Regression coverage at the mobile FFI boundary, not just the core catalog.
use super::*;
use std::collections::HashMap;

const REMOVED: &[&str] = &[
    "claude-code",
    "codex",
    "cursor",
    "devin",
    "grok",
    "hermes",
    "opencode",
    "antigravity",
];

fn config(harness: &str, model: &str) -> ChatConfig {
    ChatConfig {
        harness: harness.into(),
        model: Some(model.into()),
        reasoning: Some("xhigh".into()),
        model_options: HashMap::from([("verbosity".into(), "high".into())]),
        sandbox: SandboxLevel::WorkspaceWrite,
    }
}

#[test]
fn mobile_picker_only_offers_pi_and_never_mock() {
    let catalog = fallback_harnesses();
    assert_eq!(catalog.len(), 1);
    assert_eq!(catalog[0].id, "pi");
    assert_eq!(catalog[0].label, "Pi");
    assert!(catalog[0].offered);
    assert!(!fallback_models("pi".into()).is_empty());
    // Mock remains usable for explicit live-stack tests, not selectable.
    assert!(!fallback_models("mock".into()).is_empty());
    for harness in REMOVED.iter().copied().chain(["unknown", ""]) {
        assert!(fallback_models(harness.into()).is_empty(), "{harness}");
    }
}

#[test]
fn mobile_run_config_rejects_removed_harnesses() {
    for harness in REMOVED.iter().copied().chain(["unknown", "", "Pi"]) {
        let err = zc::ChatConfig::try_from(config(harness, "default")).unwrap_err();
        assert_eq!(
            err,
            CoreError::InvalidArgument {
                message: format!("unknown harness `{harness}`")
            },
        );
    }
}

#[test]
fn mobile_run_config_keeps_pi_provider_ids_options_and_mock() {
    for model in [
        "anthropic/claude-opus-4-5",
        "openai/gpt-5.4",
        "google/gemini-2.5-pro",
        "openrouter/anthropic/claude-sonnet-4.5[1m]",
    ] {
        let ffi = config("pi", model);
        let core = zc::ChatConfig::try_from(ffi.clone()).unwrap();
        assert_eq!(core.harness, paku_proto::HarnessId::Pi);
        assert_eq!(ChatConfig::from(&core), ffi);
    }
    let core = zc::ChatConfig::try_from(config("mock", "default")).unwrap();
    assert_eq!(core.harness, paku_proto::HarnessId::Mock);
}

#[test]
fn mobile_model_catalog_preserves_provider_metadata() {
    let models = serde_json::from_value(serde_json::json!([
        {"id":"default", "label":"Pi default"},
        {
            "id":"anthropic/claude-opus-4-5", "label":"Opus 4.5",
            "description":"From Pi's Anthropic provider", "reasoningLevels":["low", "high"],
            "options":[{"id":"verbosity", "label":"Verbosity", "defaultChoice":"high",
                "choices":[{"id":"high", "label":"High"}]}]
        },
        {"id":"openai/gpt-5.4", "label":"GPT-5.4"},
        {"id":"google/gemini-2.5-pro", "label":"Gemini 2.5 Pro"}
    ]))
    .unwrap();
    let core = zc::catalog::normalize_models("pi", models);
    let ffi: Vec<ModelInfo> = core.into_iter().map(Into::into).collect();
    assert_eq!(ffi.len(), 3);
    assert_eq!(ffi[0].id, "anthropic/claude-opus-4-5");
    assert_eq!(ffi[0].label, "Opus 4.5");
    assert_eq!(ffi[0].reasoning_levels, ["low", "high"]);
    assert_eq!(ffi[0].options[0].id, "verbosity");
    assert_eq!(ffi[0].options[0].default_choice, "high");
    assert_eq!(ffi[1].id, "openai/gpt-5.4");
    assert_eq!(ffi[2].id, "google/gemini-2.5-pro");
}
