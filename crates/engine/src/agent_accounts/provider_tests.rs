//! Pi accounts against temporary credential stores and local provider mocks.
//! No real credentials, CLI processes, or external network calls.

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::stores::*;
use super::usage::*;
use super::*;

fn write(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

/// A JWT whose payload is `claims` (unsigned — only claims are mined).
fn jwt(claims: serde_json::Value) -> String {
    format!(
        "e30.{}.sig",
        BASE64_URL.encode(serde_json::to_vec(&claims).unwrap())
    )
}

fn chatgpt_access(email: &str, account_id: &str, plan: &str) -> String {
    jwt(serde_json::json!({
        "https://api.openai.com/auth": {
            "chatgpt_account_id": account_id,
            "chatgpt_plan_type": plan,
        },
        "https://api.openai.com/profile": { "email": email },
    }))
}

type Handler = dyn Fn(&str, &str, &str) -> (u16, String) + Send + Sync;

/// A local stand-in for provider endpoints: `handler(method, path+query,
/// body)` answers every request; `hits` records `"METHOD path"`.
struct MockServer {
    base: String,
    hits: Arc<Mutex<Vec<String>>>,
    _task: tokio::task::JoinHandle<()>,
}

impl MockServer {
    async fn start(
        handler: impl Fn(&str, &str, &str) -> (u16, String) + Send + Sync + 'static,
    ) -> Self {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let hits: Arc<Mutex<Vec<String>>> = Arc::default();
        let handler: Arc<Handler> = Arc::new(handler);
        let task_hits = hits.clone();
        let task = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let handler = handler.clone();
                let hits = task_hits.clone();
                tokio::spawn(async move {
                    let mut raw = Vec::new();
                    let mut chunk = [0u8; 8192];
                    let (head_end, length) = loop {
                        let n = socket.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        raw.extend_from_slice(&chunk[..n]);
                        if let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                            let head = String::from_utf8_lossy(&raw[..end]).to_ascii_lowercase();
                            let length = head
                                .lines()
                                .find_map(|l| l.strip_prefix("content-length:"))
                                .and_then(|v| v.trim().parse::<usize>().ok())
                                .unwrap_or(0);
                            break (end + 4, length);
                        }
                    };
                    while raw.len() < head_end + length {
                        let n = socket.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        raw.extend_from_slice(&chunk[..n]);
                    }
                    let head = String::from_utf8_lossy(&raw[..head_end]).to_string();
                    let body = String::from_utf8_lossy(&raw[head_end..]).to_string();
                    let mut first = head.lines().next().unwrap_or("").split_whitespace();
                    let method = first.next().unwrap_or("").to_string();
                    let path = first.next().unwrap_or("").to_string();
                    lock(&hits).push(format!("{method} {path}"));
                    let (status, reply) = handler(&method, &path, &body);
                    let response = http_response(
                        &format!("{status} X"),
                        &[("Content-Type", "application/json")],
                        &reply,
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        Self {
            base,
            hits,
            _task: task,
        }
    }

    fn hits(&self, prefix: &str) -> usize {
        lock(&self.hits)
            .iter()
            .filter(|h| h.starts_with(prefix))
            .count()
    }
}

fn accounts_with(root: &Path, endpoints: ProbeEndpoints) -> (AgentAccounts, AgentAccountsConfig) {
    let config = AgentAccountsConfig::isolated(root);
    (
        AgentAccounts::with_endpoints(config.clone(), endpoints, Default::default()),
        config,
    )
}

/// Endpoints that all point at `base` (a mock).
fn mocked(base: &str) -> ProbeEndpoints {
    ProbeEndpoints {
        anthropic_usage: format!("{base}/api/oauth/usage"),
        anthropic_profile: format!("{base}/api/oauth/profile"),
        openai_usage: format!("{base}/backend-api/wham/usage"),
        github_api: base.to_string(),
        openai_auth: base.to_string(),
        openai_port: 0,
        allow_loopback_http: true,
    }
}

fn rows(snapshot: &AgentAccountsSnapshot, harness: HarnessId) -> Vec<AgentAccount> {
    snapshot
        .accounts
        .iter()
        .filter(|a| a.harness == harness)
        .cloned()
        .collect()
}

async fn settle(accounts: &AgentAccounts, login_id: &str) -> Vec<AgentLoginPoll> {
    let mut seen = Vec::new();
    for _ in 0..300 {
        let poll = accounts.poll_login(login_id).await.unwrap();
        let done = poll.status != AgentLoginStatus::Pending;
        seen.push(poll);
        if done {
            return seen;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("login never settled: {seen:?}");
}

async fn browser_get(port: u16, target: &str) -> String {
    let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    socket
        .write_all(format!("GET {target} HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut response = String::new();
    let _ = socket.read_to_string(&mut response).await;
    response
}

fn query_param(url: &str, name: &str) -> String {
    reqwest::Url::parse(url)
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == name)
        .unwrap()
        .1
        .into_owned()
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}
fn openai_entry(email: &str, account: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "oauth",
        "access": chatgpt_access(email, account, "plus"),
        "refresh": format!("refresh-{account}"),
        "expires": 1,
        "accountId": account,
    })
}

#[tokio::test]
async fn pi_swaps_under_its_lockfile_and_groups_rows_per_provider() {
    let tmp = tempfile::tempdir().unwrap();
    let (accounts, config) = accounts_with(tmp.path(), ProbeEndpoints::default());
    let file = config.pi_agent_dir.join("auth.json");
    write(
        &file,
        &serde_json::json!({ "openai-codex": openai_entry("a@example.com", "acct-a") }).to_string(),
    );
    let a_id = rows(&accounts.list(false).await.unwrap(), HarnessId::Pi)[0]
        .id
        .clone();
    write(
        &file,
        &serde_json::json!({ "openai-codex": openai_entry("b@example.com", "acct-b") }).to_string(),
    );
    accounts.list(false).await.unwrap();
    accounts.activate(HarnessId::Pi, &a_id).await.unwrap();
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(written["openai-codex"]["accountId"], "acct-a");
    assert!(
        !config.pi_agent_dir.join("auth.json.lock").exists(),
        "lock released"
    );

    // A lock held by pi makes the swap wait, then give up — never write
    // underneath it.
    std::fs::create_dir(config.pi_agent_dir.join("auth.json.lock")).unwrap();
    let blocked = accounts.write_keyed_entry(
        HarnessId::Pi,
        "openai-codex",
        Some(&openai_entry("b@example.com", "acct-b")),
    );
    assert!(blocked.unwrap_err().to_string().contains("locked"));
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(written["openai-codex"]["accountId"], "acct-a");
}

#[tokio::test]
async fn an_opaque_pi_claude_token_is_identified_once_then_matched_by_token() {
    let server = MockServer::start(|_, path, _| match path {
        "/api/oauth/profile" => (
            200,
            serde_json::json!({
                "account": { "uuid": "acct-uuid", "email_address": "claude@example.com" },
                "organization": { "name": "Org", "organization_type": "claude_max",
                                  "rate_limit_tier": "default_claude_max_20x" },
            })
            .to_string(),
        ),
        _ => (404, String::new()),
    })
    .await;
    let tmp = tempfile::tempdir().unwrap();
    let (accounts, config) = accounts_with(tmp.path(), mocked(&server.base));
    let file = config.pi_agent_dir.join("auth.json");
    let entry = |refresh: &str| {
        serde_json::json!({ "anthropic": {
            "type": "oauth", "access": "opaque-access", "refresh": refresh, "expires": 1,
        }})
        .to_string()
    };
    write(&file, &entry("r1"));
    let pi = rows(&accounts.list(false).await.unwrap(), HarnessId::Pi);
    assert_eq!(pi.len(), 1);
    assert_eq!(pi[0].email.as_deref(), Some("claude@example.com"));
    assert_eq!(pi[0].plan_label.as_deref(), Some("Claude Max 20×"));
    assert!(pi[0].active && pi[0].switchable);
    assert_eq!(server.hits("GET /api/oauth/profile"), 1);
    // The same token again: matched to its slot, no network.
    accounts.list(false).await.unwrap();
    assert_eq!(server.hits("GET /api/oauth/profile"), 1);
    // Pi refreshed (new pair): one more lookup, SAME account.
    write(&file, &entry("r2"));
    let pi = rows(&accounts.list(false).await.unwrap(), HarnessId::Pi);
    assert_eq!(pi.len(), 1);
    assert_eq!(server.hits("GET /api/oauth/profile"), 2);
}

#[tokio::test]
async fn chatgpt_sign_in_for_pi_lands_on_the_loopback_and_connects_the_first_login() {
    let server = MockServer::start(|method, path, body| match (method, path) {
        ("POST", "/oauth/token") => {
            assert!(body.contains("grant_type=authorization_code"), "{body}");
            assert!(body.contains("code=good-code"), "{body}");
            assert!(body.contains("code_verifier="), "{body}");
            let who = if body.contains("second") { "b" } else { "a" };
            (
                200,
                serde_json::json!({
                    "access_token": chatgpt_access(&format!("{who}@example.com"), &format!("acct-{who}"), "pro"),
                    "refresh_token": format!("refresh-{who}"),
                    "id_token": jwt(serde_json::json!({ "email": format!("{who}@example.com") })),
                    "expires_in": 3600,
                })
                .to_string(),
            )
        }
        _ => (404, String::new()),
    })
    .await;
    let tmp = tempfile::tempdir().unwrap();
    let (accounts, config) = accounts_with(tmp.path(), mocked(&server.base));
    let sign_in = |code: &'static str| {
        let accounts = accounts.clone();
        async move {
            let start = accounts.start_login(HarnessId::Pi).await.unwrap();
            assert_eq!(start.mode, AgentLoginMode::Browser);
            let port = start.callback_port.expect("loopback port reported");
            assert_eq!(loopback_port(&start.url), Some(port));
            assert_eq!(query_param(&start.url, "originator"), "pi");
            let state = query_param(&start.url, "state");
            // A stray without our state neither finishes nor kills it.
            assert!(
                browser_get(port, "/auth/callback?code=x&state=nope")
                    .await
                    .starts_with("HTTP/1.1 400")
            );
            let reply =
                browser_get(port, &format!("/auth/callback?code={code}&state={state}")).await;
            assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
            let polls = settle(&accounts, &start.login_id).await;
            assert_eq!(
                polls.last().unwrap().status,
                AgentLoginStatus::Done,
                "{polls:?}"
            );
        }
    };
    sign_in("good-code").await;
    let file = config.pi_agent_dir.join("auth.json");
    let live: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(
        live["openai-codex"]["accountId"], "acct-a",
        "first login connected"
    );
    assert_eq!(live["openai-codex"]["type"], "oauth");
    // A second account is saved next to it — the live one stays.
    sign_in("good-code-second").await;
    let live: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(live["openai-codex"]["accountId"], "acct-a");
    let pi = rows(&accounts.list(false).await.unwrap(), HarnessId::Pi);
    assert_eq!(pi.len(), 2);
    assert!(
        pi.iter()
            .any(|a| a.email.as_deref() == Some("b@example.com") && !a.active)
    );
    assert!(
        pi.iter()
            .all(|a| a.plan_label.as_deref() == Some("ChatGPT Pro"))
    );
    // Claude logins for Pi stay with pi.
    let refused = accounts
        .start_login_with(HarnessId::Pi, Some("anthropic"), None)
        .await
        .unwrap_err();
    assert!(refused.to_string().contains("/login"), "{refused}");
}

/// Signing in again as the live account (its tokens dead or revoked) makes
/// the fresh login live — otherwise the next list would snapshot the dead
/// live tokens straight back over the fresh slot (#546, for per-provider
/// stores too).
#[tokio::test]
async fn re_signing_in_the_live_chatgpt_account_replaces_its_dead_tokens() {
    let server = MockServer::start(|method, path, _| match (method, path) {
        ("POST", "/oauth/token") => (
            200,
            serde_json::json!({
                "access_token": chatgpt_access("a@example.com", "acct-a", "pro"),
                "refresh_token": "fresh-refresh",
                "expires_in": 3600,
            })
            .to_string(),
        ),
        _ => (404, String::new()),
    })
    .await;
    let tmp = tempfile::tempdir().unwrap();
    let (accounts, config) = accounts_with(tmp.path(), mocked(&server.base));
    let file = config.pi_agent_dir.join("auth.json");
    let mut dead = openai_entry("a@example.com", "acct-a");
    dead["refresh"] = "dead-refresh".into();
    write(
        &file,
        &serde_json::json!({
            "openai-codex": dead,
            "anthropic": { "type": "api", "key": "sk-ant-keep" },
        })
        .to_string(),
    );
    accounts.list(false).await.unwrap();

    let start = accounts.start_login(HarnessId::Pi).await.unwrap();
    let port = start.callback_port.unwrap();
    let state = query_param(&start.url, "state");
    let reply = browser_get(port, &format!("/auth/callback?code=c&state={state}")).await;
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    let polls = settle(&accounts, &start.login_id).await;
    assert_eq!(polls.last().unwrap().status, AgentLoginStatus::Done);

    let live: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(live["openai-codex"]["refresh"], "fresh-refresh");
    assert_eq!(live["anthropic"]["key"], "sk-ant-keep");
    // A list afterwards keeps the fresh tokens in the slot.
    let pi = rows(&accounts.list(false).await.unwrap(), HarnessId::Pi);
    assert_eq!(pi.len(), 1);
    assert!(pi[0].active);
    let slot = accounts.read_slots(HarnessId::Pi).pop().unwrap();
    assert_eq!(slot.credentials["refresh"], "fresh-refresh");
}

/// A live login paku can't identify (an opaque token whose profile call
/// failed) is never replaced by a new sign-in — it has no slot, so it would
/// be lost.
#[test]
fn copilot_usage_reads_metered_quotas_and_the_plan() {
    let paid = serde_json::json!({
        "login": "octo",
        "copilot_plan": "individual",
        "quota_reset_date_utc": "2030-02-01T00:00:00.000Z",
        "quota_snapshots": {
            "chat": { "unlimited": true, "percent_remaining": 100.0 },
            "completions": { "unlimited": true },
            "premium_interactions": { "entitlement": 300, "remaining": 75,
                                      "percent_remaining": 25.0, "unlimited": false },
        }
    });
    let snapshot = copilot_usage_snapshot(&paid).unwrap();
    assert_eq!(snapshot.plan_label.as_deref(), Some("Copilot Pro"));
    assert_eq!(snapshot.windows.len(), 1);
    assert_eq!(snapshot.windows[0].label, "Premium");
    assert!((snapshot.windows[0].used_fraction - 0.75).abs() < 1e-6);
    assert!(snapshot.windows[0].resets_at.is_some());

    let free = serde_json::json!({
        "copilot_plan": "free",
        "limited_user_reset_date": "2030-02-01",
        "monthly_quotas": { "chat": 50, "completions": 2000 },
        "limited_user_quotas": { "chat": 40, "completions": 500 },
    });
    let snapshot = copilot_usage_snapshot(&free).unwrap();
    let labels: Vec<_> = snapshot.windows.iter().map(|w| w.label.as_str()).collect();
    assert_eq!(labels, ["Chat", "Completions"]);
    assert!((snapshot.windows[0].used_fraction - 0.2).abs() < 1e-6);
    assert!((snapshot.windows[1].used_fraction - 0.75).abs() < 1e-6);
    assert!(copilot_usage_snapshot(&serde_json::json!({})).is_none());
}

/// The secret writer: random exclusive temp file, 0600 before any byte,
/// renamed over the target (replacing a symlink rather than writing through
/// it), and nothing left behind.
#[cfg(unix)]
#[test]
fn secret_writes_are_exclusive_owner_only_and_never_follow_symlinks() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let target = dir.join("auth.json");
    std::fs::write(&target, "old").unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
    // The old predictable temp name, pre-planted by someone else.
    let planted = dir.join(format!("auth.tmp-{}", std::process::id()));
    std::fs::write(&planted, "planted").unwrap();
    std::fs::set_permissions(&planted, std::fs::Permissions::from_mode(0o666)).unwrap();
    write_file_atomic(&target, b"secret", true).unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "secret");
    assert_eq!(
        mode(&target),
        0o600,
        "narrowed even though the target was 0644"
    );
    assert_eq!(std::fs::read_to_string(&planted).unwrap(), "planted");
    // A symlink at the target is replaced, never written through.
    let victim = dir.join("victim");
    std::fs::write(&victim, "keep").unwrap();
    let link = dir.join("link.json");
    std::os::unix::fs::symlink(&victim, &link).unwrap();
    write_file_atomic(&link, b"secret", true).unwrap();
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
    assert!(
        !std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(mode(&link), 0o600);
    // No temp files survive a write.
    let names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(names.iter().all(|n| !n.ends_with(".tmp")), "{names:?}");
    // Non-secret writes keep the target's mode.
    let config = dir.join("config.json");
    std::fs::write(&config, "{}").unwrap();
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o640)).unwrap();
    write_file_atomic(&config, b"{\"a\":1}", false).unwrap();
    assert_eq!(mode(&config), 0o640);
}

// Pi portion of the original mixed-harness provider usage regression.
#[tokio::test]
async fn keyed_logins_probe_the_vendor_behind_them() {
    let server = MockServer::start(|method, path, _| match (method, path) {
        ("GET", "/copilot_internal/user") => (200,
            r#"{"copilot_plan":"business","quota_snapshots":{"premium_interactions":{"percent_remaining":90,"unlimited":false}}}"#.into()),
        _ => (404, String::new()),
    }).await;
    let tmp = tempfile::tempdir().unwrap();
    let (accounts, _) = accounts_with(tmp.path(), mocked(&server.base));
    let copilot = Slot {
        id: "0123456789abcdef".into(),
        harness: HarnessId::Pi,
        account_key: "github-copilot:1".into(),
        profile: SlotProfile {
            email: "octo".into(),
            display_name: None,
            organization: None,
            plan: None,
            auth_kind: AgentAuthKind::Oauth,
        },
        credentials: serde_json::json!({ "type": "oauth", "access": "copilot-session", "refresh": "gho_x" }),
        saved_at: 1,
        created_at: None,
        store_key: Some("github-copilot".into()),
    };
    let usage = accounts.keyed_usage(HarnessId::Pi, &copilot).await.unwrap();
    assert_eq!(usage.plan_label.as_deref(), Some("Copilot Business"));
    assert!((usage.windows[0].used_fraction - 0.1).abs() < 1e-6);
}

// Retain the Copilot endpoint safety portion, not the removed agents' policies.
#[test]
fn credential_defined_endpoints_must_be_the_vendors_own_https_hosts() {
    let ghe = |raw: &str| {
        copilot_api_base(
            &serde_json::json!({ "enterpriseUrl": raw }),
            "https://api.github.com",
            false,
        )
    };
    assert_eq!(
        ghe("company.ghe.com").as_deref(),
        Some("https://api.company.ghe.com")
    );
    assert_eq!(
        ghe("https://company.ghe.com/").as_deref(),
        Some("https://api.company.ghe.com")
    );
    for raw in [
        "company.ghe.com/x",
        "http://company.ghe.com",
        "evil@company.ghe.com",
        "localhost",
        "company.ghe.com?x=1",
        "10.0.0.1",
        "evil.example",
        "https://evil.example",
        "github.company.com",
        "ghe.com",
        "company.ghe.com.evil.example",
        "a.b.ghe.com",
        "https://company.ghe.com:8443",
        "https://company.ghe.com#fragment",
    ] {
        assert_eq!(ghe(raw), None, "{raw}");
    }
    assert_eq!(
        copilot_api_base(&serde_json::json!({}), "https://api.github.com", false).as_deref(),
        Some("https://api.github.com")
    );
}

#[tokio::test]
async fn pi_all_supported_providers_probe_without_refreshing_tokens() {
    let server = MockServer::start(|method, path, _| match (method, path) {
        ("GET", "/api/oauth/profile") => (200, r#"{"account":{"uuid":"anthropic-a","email_address":"a@example.com"}}"#.into()),
        ("GET", "/user") => (200, r#"{"id":42,"login":"octo"}"#.into()),
        ("GET", "/api/oauth/usage") => (200, r#"{"five_hour":{"utilization":25}}"#.into()),
        ("GET", "/backend-api/wham/usage") => (200, r#"{"plan_type":"pro","rate_limit":{"primary_window":{"used_percent":40,"limit_window_seconds":18000}}}"#.into()),
        ("GET", "/copilot_internal/user") => (200, r#"{"copilot_plan":"business","quota_snapshots":{"premium_interactions":{"percent_remaining":90,"unlimited":false}}}"#.into()),
        _ => (404, String::new()),
    }).await;
    let tmp = tempfile::tempdir().unwrap();
    let (accounts, config) = accounts_with(tmp.path(), mocked(&server.base));
    let file = config.pi_agent_dir.join("auth.json");
    let original = serde_json::json!({
        "openai-codex": openai_entry("a@example.com", "acct-a"),
        "anthropic": { "type": "oauth", "access": "anthropic-access", "refresh": "anthropic-refresh", "expires": 1 },
        "github-copilot": { "type": "oauth", "access": "copilot-session", "refresh": "github-token", "expires": 1 },
        "openrouter": { "type": "api_key", "key": "leave-alone" },
    }).to_string();
    write(&file, &original);
    let snapshot = accounts.list(true).await.unwrap();
    assert_eq!(snapshot.accounts.len(), 3);
    for (key, fraction) in [
        ("openai-codex", 0.4),
        ("anthropic", 0.25),
        ("github-copilot", 0.1),
    ] {
        let row = snapshot
            .accounts
            .iter()
            .find(|r| r.provider.as_deref() == Some(key))
            .unwrap();
        assert!(row.active && row.switchable);
        assert!((row.usage_windows[0].used_fraction - fraction).abs() < 1e-6);
        assert!(row.usage_fetched_at.is_some());
        assert!(row.usage_error.is_none());
    }
    assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
    let hits = lock(&server.hits).clone();
    assert!(hits.iter().all(|h| h.starts_with("GET ")), "{hits:?}");
    // A second force call is deduplicated; a restart serves persisted usage offline.
    accounts.list(true).await.unwrap();
    assert_eq!(lock(&server.hits).len(), hits.len());
    let restarted = AgentAccounts::with_endpoints(config, mocked(&server.base), Default::default());
    let cached = restarted.list(false).await.unwrap();
    assert!(cached.accounts.iter().all(|r| r.usage_fetched_at.is_some()));
    assert_eq!(lock(&server.hits).len(), hits.len());
}

#[tokio::test]
async fn pi_unidentified_live_tokens_stay_active_without_snapshot_or_repeated_lookup() {
    let server = MockServer::start(|_, _, _| (401, String::new())).await;
    let tmp = tempfile::tempdir().unwrap();
    let (accounts, config) = accounts_with(tmp.path(), mocked(&server.base));
    let file = config.pi_agent_dir.join("auth.json");
    write(
        &file,
        r#"{"anthropic":{"type":"oauth","access":"opaque","refresh":"unknown","expires":1}}"#,
    );
    let before = std::fs::read_to_string(&file).unwrap();
    for _ in 0..2 {
        let snapshot = accounts.list(false).await.unwrap();
        assert_eq!(snapshot.accounts.len(), 1);
        assert!(snapshot.accounts[0].active && !snapshot.accounts[0].switchable);
        assert_eq!(snapshot.accounts[0].provider.as_deref(), Some("anthropic"));
        assert!(accounts.read_slots(HarnessId::Pi).is_empty());
    }
    assert_eq!(server.hits("GET /api/oauth/profile"), 1);
    let detected = Detected::known(
        "anthropic:new".into(),
        SlotProfile {
            email: "new@example.com".into(),
            display_name: None,
            organization: None,
            plan: None,
            auth_kind: AgentAuthKind::Oauth,
        },
        serde_json::json!({ "type": "oauth", "access": "fresh", "refresh": "fresh-refresh" }),
    )
    .keyed("anthropic");
    accounts
        .save_new_login(HarnessId::Pi, &detected)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        before,
        "unidentified live login never displaced"
    );
    assert_eq!(accounts.read_slots(HarnessId::Pi).len(), 1);
}

#[tokio::test]
async fn pi_self_hosted_copilot_accounts_switch_locally_and_never_probe_untrusted_hosts() {
    let server = MockServer::start(|_, _, _| (200, "{}".into())).await;
    let tmp = tempfile::tempdir().unwrap();
    let (accounts, config) = accounts_with(tmp.path(), ProbeEndpoints::default());
    let file = config.pi_agent_dir.join("auth.json");
    let first = serde_json::json!({ "type": "oauth", "access": "session-a", "refresh": "github-a", "enterpriseUrl": server.base });
    let second = serde_json::json!({ "type": "oauth", "access": "session-b", "refresh": "github-b", "enterpriseUrl": "github.company.com" });
    let store = |entry: &serde_json::Value| {
        serde_json::json!({ "github-copilot": entry, "other": { "type": "api_key", "key": "keep" }})
            .to_string()
    };
    write(&file, &store(&first));
    let a = accounts.list(true).await.unwrap().accounts[0].clone();
    assert!(a.active && a.switchable);
    assert!(a.usage_error.as_deref().unwrap().contains("Usage skipped"));
    write(&file, &store(&second));
    let snapshot = accounts.list(true).await.unwrap();
    let b = snapshot.accounts.iter().find(|r| r.active).unwrap().clone();
    assert!(b.switchable);
    for (row, expected) in [(&a, &first), (&b, &second)] {
        accounts.activate(HarnessId::Pi, &row.id).await.unwrap();
        let written = read_json(&file).unwrap();
        assert_eq!(&written["github-copilot"], expected);
        assert_eq!(written["other"]["key"], "keep");
        let slot = accounts.read_slot(HarnessId::Pi, &row.id).unwrap();
        assert!(slot.account_key.starts_with("github-copilot:enterprise:"));
        assert!(!slot.account_key.contains("github-a") && !slot.account_key.contains("github-b"));
    }
    assert!(lock(&server.hits).is_empty());
}

#[tokio::test]
async fn pi_switch_refuses_malformed_live_store_without_wiping_it() {
    let tmp = tempfile::tempdir().unwrap();
    let (accounts, config) = accounts_with(tmp.path(), ProbeEndpoints::default());
    let file = config.pi_agent_dir.join("auth.json");
    write(
        &file,
        &serde_json::json!({ "openai-codex": openai_entry("a@example.com", "a") }).to_string(),
    );
    let id = accounts.list(false).await.unwrap().accounts[0].id.clone();
    write(&file, "{ not json");
    let error = accounts.activate(HarnessId::Pi, &id).await.unwrap_err();
    assert!(error.to_string().contains("could not be parsed"));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "{ not json");
    assert!(!config.pi_agent_dir.join("auth.json.lock").exists());
}

#[tokio::test]
async fn pi_remote_login_routes_disappear_on_cancel_completion_and_shutdown() {
    let server = MockServer::start(|_, _, _| {
        (200, serde_json::json!({
        "access_token": chatgpt_access("a@example.com", "a", "plus"), "refresh_token": "fresh",
    }).to_string())
    })
    .await;
    let tmp = tempfile::tempdir().unwrap();
    let routes = paku_preview::login::CallbackRoutes::default();
    let config = AgentAccountsConfig::isolated(tmp.path());
    let accounts = AgentAccounts::with_endpoints(config, mocked(&server.base), routes.clone());
    let first = accounts
        .start_login_for(HarnessId::Pi, Some("remote-device"))
        .await
        .unwrap();
    assert!(routes.is_registered(&first.login_id));
    accounts.cancel_login(&first.login_id);
    assert!(!routes.is_registered(&first.login_id));
    assert!(accounts.poll_login(&first.login_id).await.is_err());
    let second = accounts
        .start_login_for(HarnessId::Pi, Some("remote-device"))
        .await
        .unwrap();
    let state = query_param(&second.url, "state");
    let reply = browser_get(
        second.callback_port.unwrap(),
        &format!("/auth/callback?state={state}&code=c"),
    )
    .await;
    assert!(reply.starts_with("HTTP/1.1 200"));
    assert_eq!(
        settle(&accounts, &second.login_id)
            .await
            .last()
            .unwrap()
            .status,
        AgentLoginStatus::Done
    );
    assert!(!routes.is_registered(&second.login_id));
    let third = accounts
        .start_login_for(HarnessId::Pi, Some("remote-device"))
        .await
        .unwrap();
    assert!(routes.is_registered(&third.login_id));
    accounts.shutdown();
    assert!(!routes.is_registered(&third.login_id));
}

#[tokio::test]
async fn pi_restarting_a_login_supersedes_the_previous_flow_and_paste_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let (accounts, _) = accounts_with(tmp.path(), mocked("http://127.0.0.1:9"));
    let first = accounts.start_login(HarnessId::Pi).await.unwrap();
    let second = accounts.start_login(HarnessId::Pi).await.unwrap();
    assert!(accounts.poll_login(&first.login_id).await.is_err());
    assert_eq!(
        accounts.poll_login(&second.login_id).await.unwrap().status,
        AgentLoginStatus::Pending
    );
    assert!(
        accounts
            .complete_login(&second.login_id, "not-a-code")
            .await
            .is_err()
    );
    assert_eq!(
        accounts.poll_login(&second.login_id).await.unwrap().status,
        AgentLoginStatus::Pending
    );
    accounts.cancel_login(&second.login_id);
}

#[tokio::test]
async fn pi_provider_login_boundaries_reject_mock_and_unsupported_providers() {
    let tmp = tempfile::tempdir().unwrap();
    let (accounts, config) = accounts_with(tmp.path(), ProbeEndpoints::default());
    for provider in ["anthropic", "github-copilot", "openai", "unknown"] {
        let error = accounts
            .start_login_with(HarnessId::Pi, Some(provider), None)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("/login"));
    }
    assert!(accounts.start_login(HarnessId::Mock).await.is_err());
    assert!(
        accounts
            .activate(HarnessId::Mock, "0123456789abcdef")
            .await
            .is_err()
    );
    assert!(
        accounts
            .forget(HarnessId::Mock, "0123456789abcdef")
            .await
            .is_err()
    );
    assert!(!config.pi_agent_dir.exists());
    assert!(lock(&accounts.inner.flows).is_empty());
}

#[test]
fn pi_usage_cache_survives_removed_or_malformed_provider_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let config = AgentAccountsConfig::isolated(tmp.path());
    let mut good = UsageEntry::default();
    good.record(
        Ok(UsageSnapshot {
            windows: Vec::new(),
            plan_label: Some("ChatGPT Plus".into()),
        }),
        "secret".into(),
        42,
    );
    let cache = serde_json::json!({ "entries": {
        "pi:openai-codex:a": good,
        "removed:a": { "error": { "kind": "no-credentials", "why": "key-expired" } },
        "pi:bad": { "error": { "kind": "unknown" } },
    }});
    write(&config.usage_cache_file(), &cache.to_string());
    let accounts = AgentAccounts::new(config);
    let usage = lock(&accounts.inner.usage);
    assert_eq!(usage.len(), 1);
    assert_eq!(usage["pi:openai-codex:a"].fetched_at, Some(42));
    assert_eq!(
        usage["pi:openai-codex:a"]
            .usage
            .as_ref()
            .unwrap()
            .plan_label
            .as_deref(),
        Some("ChatGPT Plus")
    );
}

#[tokio::test]
async fn pi_rejected_token_is_not_refreshed_and_preserves_cached_usage() {
    let server = MockServer::start(|_, _, _| (401, String::new())).await;
    let tmp = tempfile::tempdir().unwrap();
    let (accounts, config) = accounts_with(tmp.path(), mocked(&server.base));
    write(
        &config.pi_agent_dir.join("auth.json"),
        &serde_json::json!({ "openai-codex": openai_entry("a@example.com", "a") }).to_string(),
    );
    let account_key = "openai-codex:a";
    let mut cached = UsageEntry::default();
    cached.record(
        Ok(UsageSnapshot {
            windows: vec![AgentUsageWindow {
                label: "Session".into(),
                used_fraction: 0.4,
                resets_at: None,
            }],
            plan_label: None,
        }),
        "old".into(),
        now_ms() - 60_000,
    );
    lock(&accounts.inner.usage).insert(usage_key(HarnessId::Pi, account_key), cached);
    let snapshot = accounts.list(true).await.unwrap();
    let row = &snapshot.accounts[0];
    assert_eq!(
        row.usage_error.as_deref(),
        Some("Session expired — it refreshes the next time pi runs")
    );
    assert!((row.usage_windows[0].used_fraction - 0.4).abs() < 1e-6);
    assert_eq!(server.hits("GET /backend-api/wham/usage"), 1);
    assert_eq!(lock(&server.hits).len(), 1, "no refresh calls");
    accounts.list(true).await.unwrap();
    assert_eq!(lock(&server.hits).len(), 1, "rejections back off");
}

#[tokio::test]
async fn pi_failed_token_exchange_never_exposes_the_response_body() {
    let server = MockServer::start(|_, _, _| (400, "secret-token-should-not-escape".into())).await;
    let tmp = tempfile::tempdir().unwrap();
    let (accounts, config) = accounts_with(tmp.path(), mocked(&server.base));
    let start = accounts.start_login(HarnessId::Pi).await.unwrap();
    let state = query_param(&start.url, "state");
    let reply = browser_get(
        start.callback_port.unwrap(),
        &format!("/auth/callback?state={state}&code=c"),
    )
    .await;
    assert!(reply.starts_with("HTTP/1.1 400"));
    assert!(!reply.contains("secret-token"));
    let polls = settle(&accounts, &start.login_id).await;
    let poll = polls.last().unwrap();
    assert_eq!(poll.status, AgentLoginStatus::Error);
    assert!(!poll.message.as_deref().unwrap().contains("secret-token"));
    assert!(!config.pi_agent_dir.join("auth.json").exists());
}
