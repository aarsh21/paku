//! Pi's per-provider auth.json store: detect OAuth accounts, identify opaque
//! tokens once, and merge one entry under Pi's proper-lockfile directory lock.
//! Self-hosted GitHub Enterprise tokens are identified locally, never sent to
//! a credential-selected untrusted host.

use super::*;

fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}
pub(super) fn default_pi_agent_dir() -> PathBuf {
    env_dir("PI_CODING_AGENT_DIR").unwrap_or_else(|| home_dir().join(".pi").join("agent"))
}

pub(super) fn cli_name(harness: HarnessId) -> &'static str {
    if harness == HarnessId::Pi {
        "pi"
    } else {
        "mock"
    }
}

pub(super) fn upstream_vendor(store_key: &str) -> &'static str {
    match store_key {
        "openai-codex" => "OpenAI",
        "anthropic" => "Anthropic",
        "github-copilot" => "GitHub",
        _ => "the provider",
    }
}

pub(super) fn unsupported_login(harness: HarnessId, provider: &str) -> EngineError {
    EngineError::Other(format!(
        "paku can't add a {provider} login for {} — sign in with pi's own /login.",
        cli_name(harness)
    ))
}

fn hashed_key(secret: &str) -> String {
    let digest = Sha256::digest(secret.as_bytes());
    crate::repos::hex(&digest)[..12].to_string()
}

/// The model provider behind a per-provider store entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Upstream {
    /// ChatGPT OAuth (the Codex client).
    OpenAi,
    /// Claude OAuth (the Claude Code client).
    Anthropic,
    /// GitHub Copilot (a GitHub OAuth token).
    Copilot,
}

/// The store keys paku manages per agent.
pub(super) fn keyed_accounts(harness: HarnessId) -> &'static [(&'static str, Upstream)] {
    if harness != HarnessId::Pi {
        return &[];
    }
    &[
        ("openai-codex", Upstream::OpenAi),
        ("anthropic", Upstream::Anthropic),
        ("github-copilot", Upstream::Copilot),
    ]
}

pub(super) fn upstream_of(harness: HarnessId, store_key: &str) -> Option<Upstream> {
    keyed_accounts(harness)
        .iter()
        .find(|(key, _)| *key == store_key)
        .map(|(_, upstream)| *upstream)
}

/// An OAuth entry paku can use: `type: "oauth"` with an access token.
fn oauth_entry(entry: &serde_json::Value) -> bool {
    entry.get("type").and_then(|v| v.as_str()) == Some("oauth")
        && str_field(entry, "access").is_some()
}

/// A ChatGPT login entry's identity from its token claims: the access
/// token carries `https://api.openai.com/{auth,profile}` (an `id_token`,
/// when a fresh login still has one, carries `email` at top level).
pub(super) fn openai_detected(
    store_key: &str,
    entry: &serde_json::Value,
    id_token: Option<&str>,
) -> Option<Detected> {
    let access = jwt_claims(&str_field(entry, "access")?).unwrap_or_default();
    let id = id_token.and_then(jwt_claims).unwrap_or_default();
    let auth = access
        .get("https://api.openai.com/auth")
        .or_else(|| id.get("https://api.openai.com/auth"))
        .cloned()
        .unwrap_or_default();
    let email = access
        .get("https://api.openai.com/profile")
        .and_then(|p| str_field(p, "email"))
        .or_else(|| str_field(&id, "email"));
    let account_id =
        str_field(entry, "accountId").or_else(|| str_field(&auth, "chatgpt_account_id"));
    let identity = account_id.clone().or_else(|| email.clone())?;
    Some(
        Detected::known(
            format!("{store_key}:{identity}"),
            SlotProfile {
                email: email.unwrap_or_else(|| "ChatGPT account".to_string()),
                display_name: str_field(&id, "name"),
                organization: None,
                plan: chatgpt_plan(str_field(&auth, "chatgpt_plan_type").as_deref())
                    .or_else(|| Some("ChatGPT".to_string())),
                auth_kind: AgentAuthKind::Oauth,
            },
            entry.clone(),
        )
        .keyed(store_key),
    )
}

/// The secret an opaque entry is matched by: Copilot's GitHub token lives
/// in `refresh` (both agents), Claude's rotating pair in `refresh`.
fn opaque_secret(entry: &serde_json::Value) -> Option<String> {
    str_field(entry, "refresh").or_else(|| str_field(entry, "access"))
}

/// A Copilot login on a GitHub Enterprise host paku doesn't send tokens
/// to (self-hosted GHES): identified without the network — labelled by its
/// host, keyed by a SHA-256 fingerprint of its GitHub token (never the token
/// itself) — so it snapshots into a slot, switches and restores like any
/// other. Its usage stays skipped ([`copilot_api_base`] is `None`).
pub(super) fn local_enterprise_identity(
    store_key: &str,
    github_token: &str,
    entry: &serde_json::Value,
) -> (String, SlotProfile) {
    let host = str_field(entry, "enterpriseUrl").and_then(|raw| {
        let raw = raw.trim();
        let candidate = match raw.contains("://") {
            true => raw.to_string(),
            false => format!("https://{raw}"),
        };
        reqwest::Url::parse(&candidate)
            .ok()?
            .host_str()
            .map(str::to_ascii_lowercase)
    });
    let email = match host {
        Some(host) => format!("GitHub Enterprise account · {host}"),
        None => "GitHub Enterprise account".to_string(),
    };
    (
        format!("{store_key}:enterprise:{}", hashed_key(github_token)),
        SlotProfile {
            email,
            display_name: None,
            organization: None,
            plan: Some("GitHub Copilot".to_string()),
            auth_kind: AgentAuthKind::Oauth,
        },
    )
}

fn unresolved(store_key: &str, upstream: Upstream) -> Detected {
    let (email, plan) = match upstream {
        Upstream::Anthropic => ("Claude account", "Claude"),
        Upstream::Copilot => ("GitHub account", "GitHub Copilot"),
        Upstream::OpenAi => ("ChatGPT account", "ChatGPT"),
    };
    Detected {
        account_key: format!("{store_key}:unidentified"),
        profile: SlotProfile {
            email: email.to_string(),
            display_name: None,
            organization: None,
            plan: Some(plan.to_string()),
            auth_kind: AgentAuthKind::Oauth,
        },
        credentials: None,
        store_key: Some(store_key.to_string()),
        identity_known: false,
    }
}

// ── locks ───────────────────────────────────────────────────────────────────

const LOCK_WAIT: Duration = Duration::from_secs(5);
/// proper-lockfile's lock: a `<file>.lock` DIRECTORY (Pi). A lock older than
/// proper-lockfile's own staleness window (10s, refreshed while held) is a
/// crashed holder's and is taken over.
pub(super) struct DirLock(PathBuf);

impl DirLock {
    pub(super) fn acquire(path: &Path) -> Result<Self, EngineError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let deadline = Instant::now() + LOCK_WAIT;
        loop {
            match std::fs::create_dir(path) {
                Ok(()) => return Ok(Self(path.to_path_buf())),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    let stale = std::fs::metadata(path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|at| at.elapsed().ok())
                        .is_some_and(|age| age > Duration::from_secs(30));
                    if stale {
                        let _ = std::fs::remove_dir(path);
                        continue;
                    }
                    if Instant::now() > deadline {
                        return Err(EngineError::Other(format!(
                            "{} is locked by the agent — try again in a moment.",
                            path.display()
                        )));
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(err) => return Err(err.into()),
            }
        }
    }
}

impl Drop for DirLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.0);
    }
}

fn lock_path(file: &Path) -> PathBuf {
    let mut name = file.file_name().unwrap_or_default().to_os_string();
    name.push(".lock");
    file.with_file_name(name)
}

pub(super) enum StoreLock {
    Dir(PathBuf),
}

/// Rounds of compare-before-write before a store that keeps changing wins.
const MERGE_ATTEMPTS: usize = 3;

/// Replace (or, with `entry` = `None`, remove) ONE entry of a JSON-object
/// credential store, keeping every other key as it is, under `lock` — then compare-before-write: the file is read
/// again right before the rename, and a change since the first read (an
/// agent writing without taking the lock, or Pi mid-refresh) restarts
/// the cycle from the new contents instead of
/// clobbering it. The replacement is staged (written and synced to its temp
/// file) before that second read, so the unguarded window is only the
/// comparison plus the rename. RESIDUAL RISK: a writer that ignores the lock
/// and lands inside that window is overwritten; the entry it would have
/// changed is re-detected (and re-snapshotted) on the next list.
///
/// An existing store that doesn't parse is never overwritten — writing only
/// our entry would wipe the user's other logins.
pub(super) fn merge_json_entry(
    file: &Path,
    key: &str,
    entry: Option<&serde_json::Value>,
    lock: StoreLock,
) -> Result<(), EngineError> {
    let _guard = match lock {
        StoreLock::Dir(path) => DirLock::acquire(&path)?,
    };
    let read = || match std::fs::read(file) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(EngineError::from(err)),
    };
    for _ in 0..MERGE_ATTEMPTS {
        let before = read()?;
        if before.is_none() && entry.is_none() {
            return Ok(()); // nothing to remove
        }
        let mut store = match &before {
            Some(bytes) => serde_json::from_slice::<serde_json::Value>(bytes)
                .ok()
                .filter(serde_json::Value::is_object)
                .ok_or_else(|| {
                    EngineError::Other(format!(
                        "{} exists but could not be parsed — not switching to avoid wiping it.",
                        file.display()
                    ))
                })?,
            None => serde_json::json!({}),
        };
        if let Some(map) = store.as_object_mut() {
            match entry {
                Some(entry) => {
                    map.insert(key.to_string(), entry.clone());
                }
                None => {
                    if map.remove(key).is_none() {
                        return Ok(()); // already gone
                    }
                }
            }
        }
        let json = serde_json::to_string_pretty(&store)
            .map_err(|e| EngineError::Other(format!("serialize {}: {e}", file.display())))?;
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // Stage (create, write, fsync) BEFORE the final comparison, so the
        // unguarded window is just compare + rename, not the temp-file I/O.
        let staged = stage_file_atomic(file, json.as_bytes(), true)?;
        if read()? != before {
            drop(staged); // deletes the temp file
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        staged
            .persist(file)
            .map_err(|e| EngineError::from(e.error))?;
        return Ok(());
    }
    Err(EngineError::Other(format!(
        "{} kept changing while paku was switching — try again in a moment.",
        file.display()
    )))
}

// ── credential-defined endpoints ────────────────────────────────────────────

/// A Copilot enterprise base safe to send secrets to: `https` on a single-label
/// subdomain of ghe.com, no userinfo, port, query, fragment, or path beyond `/`.
/// Bare hosts (Pi's `enterpriseUrl`) read as `https://host`. `allow_loopback`
/// additionally admits `http://127.0.0.1|localhost:<port>` — tests' mock
/// servers only, never production. `None` = don't send anything.
pub(super) fn trusted_base(raw: &str, allow_loopback: bool) -> Option<reqwest::Url> {
    let raw = raw.trim();
    let candidate = if raw.contains("://") {
        raw.to_string()
    } else {
        format!("https://{}", raw.trim_end_matches('/'))
    };
    let url = reqwest::Url::parse(&candidate).ok()?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return None;
    }
    if allow_loopback
        && url.scheme() == "http"
        && matches!(url.host_str(), Some("127.0.0.1" | "localhost"))
    {
        return Some(url);
    }
    if url.scheme() != "https" || url.port().is_some() {
        return None;
    }
    // `domain()` is `None` for IP literals: a secret never goes to a bare IP.
    let host = url.domain()?.to_ascii_lowercase();
    let allowed = host
        .strip_suffix(".ghe.com")
        .is_some_and(|tenant| !tenant.is_empty() && !tenant.contains('.'));
    allowed.then_some(url)
}

/// for a GitHub Enterprise Cloud (`*.ghe.com`) `enterpriseUrl`. Any other
/// enterprise host — self-hosted GHES included, whose REST root would be
/// `https://<host>/api/v3` — is `None`: paku skips identity/usage probes
/// rather than send the token to a host the credential file alone names
/// (the login gets a local identity, [`local_enterprise_identity`]).
pub(super) fn copilot_api_base(
    entry: &serde_json::Value,
    github_api: &str,
    allow_loopback: bool,
) -> Option<String> {
    match str_field(entry, "enterpriseUrl") {
        None => Some(github_api.trim_end_matches('/').to_string()),
        Some(raw) => {
            let url = trusted_base(&raw, allow_loopback)?;
            Some(match url.scheme() {
                "https" => format!("https://api.{}", url.host_str()?),
                _ => url.as_str().trim_end_matches('/').to_string(),
            })
        }
    }
}

impl AgentAccounts {
    fn keyed_file(&self, _harness: HarnessId) -> PathBuf {
        self.inner.config.pi_agent_dir.join("auth.json")
    }

    /// The live entry under `store_key`, if it's an OAuth login.
    pub(super) fn live_keyed_entry(
        &self,
        harness: HarnessId,
        store_key: &str,
    ) -> Option<serde_json::Value> {
        if harness != HarnessId::Pi {
            return None;
        }
        read_json(&self.keyed_file(harness))?
            .get(store_key)
            .filter(|entry| oauth_entry(entry))
            .cloned()
    }

    /// Replace one provider entry under Pi's own proper-lockfile lock.
    pub(super) fn write_keyed_entry(
        &self,
        harness: HarnessId,
        store_key: &str,
        entry: Option<&serde_json::Value>,
    ) -> Result<(), EngineError> {
        require_pi(harness)?;
        if upstream_of(harness, store_key).is_none() {
            return Err(EngineError::Other(
                "That saved login names no supported Pi provider.".into(),
            ));
        }
        let file = self.keyed_file(harness);
        merge_json_entry(&file, store_key, entry, StoreLock::Dir(lock_path(&file)))
    }

    /// Resolved live logins and opaque logins that could not be identified.
    pub(super) async fn detect_keyed(&self, harness: HarnessId) -> (Vec<Detected>, Vec<Detected>) {
        let mut resolved = Vec::new();
        let mut unidentified = Vec::new();
        if harness != HarnessId::Pi {
            return (resolved, unidentified);
        }
        let Some(store) = read_json(&self.keyed_file(harness)) else {
            return (resolved, unidentified);
        };
        for &(store_key, upstream) in keyed_accounts(harness) {
            let Some(entry) = store.get(store_key).filter(|e| oauth_entry(e)) else {
                continue;
            };
            match self
                .identify_entry(harness, store_key, upstream, entry)
                .await
            {
                Some(detected) => resolved.push(detected),
                None => unidentified.push(unresolved(store_key, upstream)),
            }
        }
        (resolved, unidentified)
    }

    /// The live login under ONE store key: `Some(None)` when an OAuth entry
    /// is there but couldn't be identified, `None` when there is none.
    pub(super) async fn detect_keyed_entry(
        &self,
        harness: HarnessId,
        store_key: &str,
    ) -> Option<Option<Detected>> {
        let upstream = upstream_of(harness, store_key)?;
        let entry = self.live_keyed_entry(harness, store_key)?;
        Some(
            self.identify_entry(harness, store_key, upstream, &entry)
                .await,
        )
    }

    async fn identify_entry(
        &self,
        harness: HarnessId,
        store_key: &str,
        upstream: Upstream,
        entry: &serde_json::Value,
    ) -> Option<Detected> {
        match upstream {
            Upstream::OpenAi => openai_detected(store_key, entry, None),
            Upstream::Anthropic | Upstream::Copilot => {
                self.identify_opaque(harness, store_key, upstream, entry)
                    .await
            }
        }
    }

    /// Who an opaque live entry is: the slot already holding this exact
    /// token, else the remembered answer for it, else one read-only profile
    /// call (cached per token, so a list doesn't re-ask).
    async fn identify_opaque(
        &self,
        harness: HarnessId,
        store_key: &str,
        upstream: Upstream,
        entry: &serde_json::Value,
    ) -> Option<Detected> {
        let secret = opaque_secret(entry)?;
        if let Some(slot) = self.read_slots(harness).into_iter().find(|slot| {
            slot.store_key.as_deref() == Some(store_key)
                && opaque_secret(&slot.credentials).as_deref() == Some(secret.as_str())
        }) {
            return Some(
                Detected::known(slot.account_key, slot.profile, entry.clone()).keyed(store_key),
            );
        }
        // A GitHub Enterprise host paku won't send the token to (self-hosted
        // GHES): no profile call — a local identity keeps it switchable.
        if upstream == Upstream::Copilot
            && copilot_api_base(
                entry,
                &self.inner.endpoints.github_api,
                self.inner.endpoints.allow_loopback_http,
            )
            .is_none()
        {
            let (account_key, profile) = local_enterprise_identity(store_key, &secret, entry);
            return Some(Detected::known(account_key, profile, entry.clone()).keyed(store_key));
        }
        let fingerprint = format!("{store_key}:{}", hashed_key(&secret));
        let cached = lock(&self.inner.identities).get(&fingerprint).cloned();
        let (account_key, profile) = match cached {
            Some(IdentityLookup::Known(key, profile)) => (key, profile),
            Some(IdentityLookup::Failed(at)) if at.elapsed() < IDENTITY_RETRY => return None,
            _ => {
                let known = match upstream {
                    Upstream::Anthropic => match str_field(entry, "access") {
                        Some(access) => self.anthropic_identity(store_key, &access).await,
                        None => None,
                    },
                    Upstream::Copilot => self.github_identity(store_key, &secret, entry).await,
                    Upstream::OpenAi => None,
                };
                let lookup = match &known {
                    Some((key, profile)) => IdentityLookup::Known(key.clone(), profile.clone()),
                    None => IdentityLookup::Failed(Instant::now()),
                };
                lock(&self.inner.identities).insert(fingerprint, lookup);
                known?
            }
        };
        Some(Detected::known(account_key, profile, entry.clone()).keyed(store_key))
    }

    /// Claude's profile for a live access token (`user:profile` scope).
    async fn anthropic_identity(
        &self,
        store_key: &str,
        access_token: &str,
    ) -> Option<(String, SlotProfile)> {
        let profile: serde_json::Value = self
            .inner
            .http
            .get(&self.inner.endpoints.anthropic_profile)
            .bearer_auth(access_token)
            .header("anthropic-beta", "oauth-2025-04-20")
            .send()
            .await
            .ok()
            .filter(|res| res.status().is_success())?
            .json()
            .await
            .ok()?;
        let account = profile.get("account")?;
        let org = profile.get("organization").cloned().unwrap_or_default();
        let email = str_field(account, "email_address")?;
        let uuid = str_field(account, "uuid").unwrap_or_else(|| email.clone());
        Some((
            format!("{store_key}:{uuid}"),
            SlotProfile {
                email,
                display_name: str_field(account, "display_name")
                    .or_else(|| str_field(account, "full_name")),
                organization: str_field(&org, "name"),
                plan: claude_plan(
                    str_field(&org, "organization_type").as_deref(),
                    str_field(&org, "rate_limit_tier").as_deref(),
                )
                .map(|plan| format!("Claude {plan}"))
                .or_else(|| Some("Claude".to_string())),
                auth_kind: AgentAuthKind::Oauth,
            },
        ))
    }

    /// The GitHub user behind a Copilot login's GitHub token.
    pub(super) async fn github_identity(
        &self,
        store_key: &str,
        github_token: &str,
        entry: &serde_json::Value,
    ) -> Option<(String, SlotProfile)> {
        // A GitHub Enterprise host is validated before the token goes there.
        let base = copilot_api_base(
            entry,
            &self.inner.endpoints.github_api,
            self.inner.endpoints.allow_loopback_http,
        )?;
        let user: serde_json::Value = self
            .inner
            .http
            .get(format!("{base}/user"))
            .header("Authorization", format!("token {github_token}"))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "paku")
            .send()
            .await
            .ok()
            .filter(|res| res.status().is_success())?
            .json()
            .await
            .ok()?;
        let id = user.get("id").and_then(|v| {
            v.as_i64()
                .map(|n| n.to_string())
                .or_else(|| str_field(&user, "id"))
        })?;
        let login = str_field(&user, "login")?;
        Some((
            format!("{store_key}:{id}"),
            SlotProfile {
                email: str_field(&user, "email").unwrap_or_else(|| login.clone()),
                display_name: str_field(&user, "name").or(Some(login)),
                organization: str_field(entry, "enterpriseUrl"),
                plan: Some("GitHub Copilot".to_string()),
                auth_kind: AgentAuthKind::Oauth,
            },
        ))
    }

    /// Persist a fresh login as a slot, then let it take over the live login
    /// where [`AgentAccounts::adopt_if_live`] says so: no live login yet
    /// ("Connect" means runs work afterwards), or a re-login of the live
    /// account (else the next list would snapshot the old, possibly revoked,
    /// live tokens straight back over the fresh slot). Any other live login
    /// is left alone — switching stays explicit. Under [`Inner::ops`], so a
    /// concurrent list can't land between the two writes.
    pub(super) async fn save_new_login(
        &self,
        harness: HarnessId,
        detected: &Detected,
    ) -> Result<(), EngineError> {
        let _ops = self.inner.ops.lock().await;
        self.snapshot_detected(harness, detected)?;
        let id = slot_id_for(harness, &detected.account_key);
        if let Some(slot) = self.read_slot(harness, &id) {
            self.adopt_if_live(&slot).await?;
        }
        Ok(())
    }
}
