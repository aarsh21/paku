//! Shared model-provider parser regressions retained for Pi's supported logins.
use super::*;

#[test]
fn plan_labels() {
    assert_eq!(
        claude_plan(Some("claude_max"), Some("default_claude_max_20x")).as_deref(),
        Some("Max 20×")
    );
    assert_eq!(
        claude_plan(Some("claude_pro"), None).as_deref(),
        Some("Pro")
    );
    assert_eq!(
        claude_plan(Some("claude_team"), Some("weird")).as_deref(),
        Some("Team")
    );
    assert_eq!(claude_plan(Some("free"), None), None);
    assert_eq!(chatgpt_plan(Some("plus")).as_deref(), Some("ChatGPT Plus"));
    assert_eq!(chatgpt_plan(Some("free")).as_deref(), Some("ChatGPT Free"));
    assert_eq!(chatgpt_plan(None), None);
}

#[test]
fn codex_window_labels_track_the_window_span() {
    // Pi's ChatGPT provider shares these OpenAI rate-limit windows.
    assert_eq!(chatgpt_window_label(2_592_000), "Month");
    assert_eq!(chatgpt_window_label(18_000), "Session");
    assert_eq!(chatgpt_window_label(604_800), "Week");
    assert_eq!(chatgpt_window_label(0), "Session");
}

#[test]
fn claude_usage_windows_map_buckets_and_percent() {
    let body = serde_json::json!({
        "five_hour": { "utilization": 6.0, "resets_at": "2026-04-08T18:59:59Z" },
        "seven_day": { "utilization": 35.0, "resets_at": "2026-04-14T16:59:59Z" },
        "extra_usage": { "is_enabled": true },
    });
    let snapshot = anthropic_usage_windows(&body).expect("windows");
    assert_eq!(snapshot.plan_label, None);
    let labels: Vec<_> = snapshot.windows.iter().map(|w| w.label.as_str()).collect();
    assert_eq!(labels, ["Session", "Week"]);
    assert!((snapshot.windows[0].used_fraction - 0.06).abs() < 1e-6);
    assert!((snapshot.windows[1].used_fraction - 0.35).abs() < 1e-6);
    assert_eq!(
        snapshot.windows[1].resets_at,
        Some("2026-04-14T16:59:59Z".parse::<DateTime<Utc>>().unwrap())
    );
}

#[test]
fn claude_usage_windows_none_without_any_bucket() {
    assert!(anthropic_usage_windows(&serde_json::json!({ "extra_usage": {} })).is_none());
    assert!(anthropic_usage_windows(&serde_json::json!({ "five_hour": {} })).is_none());
}
