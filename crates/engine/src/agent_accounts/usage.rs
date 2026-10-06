//! Read-only usage probes for Pi's ChatGPT, Anthropic, and Copilot logins.
//! Pi owns token refresh; the engine never rotates these credentials.

use super::stores::{Upstream, copilot_api_base, upstream_of};
use super::*;

impl AgentAccounts {
    /// the vendor behind it with its own token.
    pub(super) async fn keyed_usage(
        &self,
        harness: HarnessId,
        slot: &Slot,
    ) -> Result<UsageSnapshot, ProbeError> {
        let missing = ProbeError::NoCredentials {
            why: NoCredentials::Missing,
        };
        let key = slot.store_key.as_deref().ok_or(missing.clone())?;
        let creds = &slot.credentials;
        match upstream_of(harness, key) {
            Some(Upstream::OpenAi) => {
                let access = str_field(creds, "access").ok_or(missing)?;
                let account = str_field(creds, "accountId").unwrap_or_default();
                self.openai_usage(stores::cli_name(harness), &access, &account)
                    .await
            }
            Some(Upstream::Anthropic) => {
                let access = str_field(creds, "access").ok_or(missing)?;
                self.anthropic_usage_request(&access).await
            }
            Some(Upstream::Copilot) => {
                let token = str_field(creds, "refresh").ok_or(missing)?;
                // GitHub Enterprise: a validated plain host, or nothing is sent.
                let api = copilot_api_base(
                    creds,
                    &self.inner.endpoints.github_api,
                    self.inner.endpoints.allow_loopback_http,
                )
                .ok_or(ProbeError::UntrustedEndpoint)?;
                self.copilot_usage(&api, &token).await
            }
            None => Err(ProbeError::NoCredentials {
                why: NoCredentials::Unsupported,
            }),
        }
    }

    async fn openai_usage(
        &self,
        provider: &'static str,
        access_token: &str,
        account_id: &str,
    ) -> Result<UsageSnapshot, ProbeError> {
        let body = probe_json(
            provider,
            "usage",
            self.inner
                .http
                .get(&self.inner.endpoints.openai_usage)
                .bearer_auth(access_token)
                .header("chatgpt-account-id", account_id),
        )
        .await?;
        openai_usage_snapshot(&body).ok_or_else(|| schema_error(provider, &body))
    }

    async fn copilot_usage(&self, api: &str, token: &str) -> Result<UsageSnapshot, ProbeError> {
        let body = probe_json(
            "copilot",
            "usage",
            self.inner
                .http
                .get(format!(
                    "{}/copilot_internal/user",
                    api.trim_end_matches('/')
                ))
                .header("Authorization", format!("token {token}"))
                .header("Accept", "application/json")
                .header("User-Agent", "paku"),
        )
        .await?;
        copilot_usage_snapshot(&body).ok_or_else(|| schema_error("copilot", &body))
    }
}

/// GitHub's `copilot_internal/user`: each metered quota snapshot (premium
/// requests on paid plans; chat/completions on Free) as a window. An
/// all-unlimited plan has no meters but still reports its plan.
pub(super) fn copilot_usage_snapshot(body: &serde_json::Value) -> Option<UsageSnapshot> {
    let resets_at = parse_when(body.get("quota_reset_date_utc"))
        .or_else(|| day_start(body.get("quota_reset_date")))
        .or_else(|| day_start(body.get("limited_user_reset_date")));
    let mut windows = Vec::new();
    if let Some(snapshots) = body.get("quota_snapshots").and_then(|s| s.as_object()) {
        for (key, label) in [
            ("premium_interactions", "Premium"),
            ("chat", "Chat"),
            ("completions", "Completions"),
        ] {
            let Some(snapshot) = snapshots.get(key) else {
                continue;
            };
            if snapshot.get("unlimited").and_then(|v| v.as_bool()) == Some(true) {
                continue;
            }
            let used = match snapshot.get("percent_remaining").and_then(json_f64) {
                Some(remaining) => (100.0 - remaining) / 100.0,
                None => {
                    let entitlement = snapshot.get("entitlement").and_then(json_f64)?;
                    let remaining = snapshot.get("remaining").and_then(json_f64)?;
                    if entitlement <= 0.0 {
                        continue;
                    }
                    1.0 - remaining / entitlement
                }
            };
            windows.push(AgentUsageWindow {
                label: label.to_string(),
                used_fraction: used.clamp(0.0, 1.0) as f32,
                resets_at,
            });
        }
    } else if let (Some(monthly), Some(left)) = (
        body.get("monthly_quotas").and_then(|m| m.as_object()),
        body.get("limited_user_quotas").and_then(|m| m.as_object()),
    ) {
        for (key, label) in [("chat", "Chat"), ("completions", "Completions")] {
            if let (Some(total), Some(remaining)) = (
                monthly.get(key).and_then(json_f64),
                left.get(key).and_then(json_f64),
            ) && total > 0.0
            {
                windows.push(AgentUsageWindow {
                    label: label.to_string(),
                    used_fraction: (1.0 - remaining / total).clamp(0.0, 1.0) as f32,
                    resets_at,
                });
            }
        }
    }
    let plan_label = str_field(body, "copilot_plan").map(|plan| {
        let name = match plan.as_str() {
            "individual" => "Pro".to_string(),
            "individual_pro" => "Pro+".to_string(),
            other => other
                .split('_')
                .map(|w| {
                    let mut c = w.chars();
                    c.next()
                        .map(|f| format!("{}{}", f.to_uppercase(), c.as_str()))
                        .unwrap_or_default()
                })
                .collect::<Vec<_>>()
                .join(" "),
        };
        format!("Copilot {name}")
    });
    (!windows.is_empty() || plan_label.is_some()).then_some(UsageSnapshot {
        windows,
        plan_label,
    })
}
fn json_f64(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn day_start(value: Option<&serde_json::Value>) -> Option<DateTime<Utc>> {
    let day = chrono::NaiveDate::parse_from_str(value?.as_str()?, "%Y-%m-%d").ok()?;
    Some(day.and_hms_opt(0, 0, 0)?.and_utc())
}
