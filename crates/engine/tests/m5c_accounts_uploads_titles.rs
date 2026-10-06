//! M5c integration: Pi agent-account slot mechanics, uploads
//! chunk→commit→readback + path jail, chat auto-titling with the mock harness,
//! and the RPC dispatch for each new method over the memory transport.
//!
//! Account tests use `AgentAccountsConfig::isolated` paths under a tempdir
//! (never the real `~/.pi/agent`), so they are hermetic and parallel-safe.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as BASE64_URL;

use paku_engine::{
    AgentAccounts, AgentAccountsConfig, EngineCore, HarnessRegistry, Repos, Uploads,
    worktree_branch_from_title,
};
use paku_harness::mock::MockHarness;
use paku_proto::{AgentAccountsSnapshot, AgentEvent, DoneStatus, HarnessId, SandboxLevel};
use paku_rpc::methods;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// AgentAccounts wired to a temp Pi agent directory.
fn test_accounts(root: &Path) -> (AgentAccounts, AgentAccountsConfig) {
    let config = AgentAccountsConfig::isolated(root);
    (AgentAccounts::new(config.clone()), config)
}

fn write_pi_login(config: &AgentAccountsConfig, account: &str) {
    let claims = serde_json::json!({
        "https://api.openai.com/auth": { "chatgpt_account_id": account, "chatgpt_plan_type": "plus" },
        "https://api.openai.com/profile": { "email": format!("{account}@example.com") },
    });
    std::fs::create_dir_all(&config.pi_agent_dir).expect("pi agent dir");
    std::fs::write(
        config.pi_agent_dir.join("auth.json"),
        serde_json::json!({
            "openai-codex": {
                "type": "oauth",
                "access": format!("e30.{}.sig", BASE64_URL.encode(claims.to_string())),
                "refresh": format!("refresh-{account}"),
                "expires": 1,
                "accountId": account,
            },
            "openrouter": { "type": "api", "key": "keep" },
        })
        .to_string(),
    )
    .expect("pi auth");
}

fn account_emails(snapshot: &AgentAccountsSnapshot, harness: HarnessId) -> Vec<(String, bool)> {
    snapshot
        .accounts
        .iter()
        .filter(|a| a.harness == harness)
        .map(|a| (a.email.clone().unwrap_or_default(), a.active))
        .collect()
}

fn assemble_with_mock(dir: &Path, script: Vec<AgentEvent>) -> EngineCore {
    std::fs::create_dir_all(dir).expect("data dir");
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(MockHarness { script }));
    EngineCore::assemble(dir, Arc::new(registry), HarnessId::Mock, None).expect("engine assembles")
}

async fn git(cwd: &Path, args: &[&str]) {
    let output = tokio::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@test")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@test")
        .output()
        .await
        .expect("git spawns");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn init_repo(dir: &Path) {
    std::fs::create_dir_all(dir).expect("repo dir");
    git(dir, &["init", "-b", "main"]).await;
    std::fs::write(dir.join("a.txt"), "one\n").expect("write a.txt");
    git(dir, &["add", "."]).await;
    git(dir, &["commit", "-m", "initial"]).await;
}

/// Poll until `probe` yields Some, or panic at the deadline.
async fn wait_for<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(value) = probe() {
            return value;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// ---------------------------------------------------------------------------
// Agent accounts — Pi slot swap round trip
// ---------------------------------------------------------------------------

/// Pi's live login is snapshotted, a second login is kept beside it, and
/// switching rewrites exactly that provider's entry at 0600.
#[tokio::test]
async fn pi_logins_swap_round_trip() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (accounts, config) = test_accounts(tmp.path());
    let pi_file = config.pi_agent_dir.join("auth.json");

    write_pi_login(&config, "ann");
    let first = accounts.list(false).await.expect("list ann");
    assert_eq!(
        account_emails(&first, HarnessId::Pi),
        vec![("ann@example.com".to_string(), true)]
    );
    assert!(first.warnings.is_empty(), "{:?}", first.warnings);
    let ann = first
        .accounts
        .iter()
        .find(|a| a.harness == HarnessId::Pi && a.active)
        .expect("live Pi login listed");
    assert_eq!(ann.provider.as_deref(), Some("openai-codex"));
    let ann_id = ann.id.clone();

    write_pi_login(&config, "bob");
    let second = accounts.list(false).await.expect("list bob");
    let rows: Vec<_> = second
        .accounts
        .iter()
        .filter(|a| a.harness == HarnessId::Pi)
        .collect();
    assert_eq!(rows.len(), 2, "Pi keeps both logins");
    assert_eq!(rows.iter().filter(|a| a.active).count(), 1);
    assert!(rows.iter().all(|a| a.switchable));

    let snapshot = accounts
        .activate(HarnessId::Pi, &ann_id)
        .await
        .expect("switch to ann");
    assert!(snapshot.accounts.iter().any(|a| a.id == ann_id && a.active));
    let mut emails = account_emails(&snapshot, HarnessId::Pi);
    emails.sort();
    assert_eq!(
        emails,
        vec![
            ("ann@example.com".to_string(), true),
            ("bob@example.com".to_string(), false),
        ]
    );
    let pi_live: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&pi_file).expect("pi auth"))
            .expect("pi auth json");
    assert_eq!(pi_live["openai-codex"]["accountId"], "ann");
    assert_eq!(pi_live["openrouter"]["key"], "keep");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&pi_file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{}", pi_file.display());
    }
    // Pi rows name the real auth provider they belong to.
    let snapshot = accounts.list(false).await.expect("list");
    assert!(
        snapshot
            .accounts
            .iter()
            .filter(|a| a.harness == HarnessId::Pi)
            .all(|a| a.provider.as_deref() == Some("openai-codex"))
    );
}

#[tokio::test]
async fn pi_forget_guards_and_removes_slots() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (accounts, config) = test_accounts(tmp.path());
    let pi_file = config.pi_agent_dir.join("auth.json");
    let slots_dir = config.data_dir.join("agent-accounts").join("pi");

    write_pi_login(&config, "ann");
    let first = accounts.list(false).await.expect("list ann");
    let ann_id = first
        .accounts
        .iter()
        .find(|a| a.harness == HarnessId::Pi && a.active)
        .expect("ann listed")
        .id
        .clone();
    let live_before = std::fs::read(&pi_file).expect("pi auth");
    let slot_file = slots_dir.join(format!("{ann_id}.json"));
    let slot_before: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&slot_file).expect("ann slot")).expect("slot json");
    let outside = config.data_dir.join("evil.json");
    std::fs::write(&outside, b"keep outside").expect("outside sentinel");

    // Raw RPC ids must not escape the slot directory, even for activation.
    for id in ["../../evil", "ABCDEF0123456789"] {
        assert!(accounts.forget(HarnessId::Pi, id).await.is_err());
        assert!(accounts.activate(HarnessId::Pi, id).await.is_err());
    }
    assert_eq!(std::fs::read(&outside).unwrap(), b"keep outside");
    assert_eq!(std::fs::read(&pi_file).unwrap(), live_before);
    let slot_after: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&slot_file).expect("ann slot survives"))
            .expect("slot json");
    // Activation may refresh the live slot's saved-at timestamp while listing.
    assert_eq!(slot_after["credentials"], slot_before["credentials"]);

    // Forgetting a saved login leaves the live provider entry untouched.
    write_pi_login(&config, "bob");
    accounts.list(false).await.expect("list bob");
    let bob_live = std::fs::read(&pi_file).expect("bob auth");
    let snapshot = accounts
        .forget(HarnessId::Pi, &ann_id)
        .await
        .expect("forget saved ann");
    assert_eq!(
        account_emails(&snapshot, HarnessId::Pi),
        vec![("bob@example.com".to_string(), true)]
    );
    assert!(!slots_dir.join(format!("{ann_id}.json")).exists());
    assert_eq!(std::fs::read(&pi_file).unwrap(), bob_live);

    // Forgetting the live login removes only its auth provider, not API keys.
    let bob_id = snapshot
        .accounts
        .iter()
        .find(|a| a.harness == HarnessId::Pi && a.active)
        .expect("bob listed")
        .id
        .clone();
    let snapshot = accounts
        .forget(HarnessId::Pi, &bob_id)
        .await
        .expect("forget live bob");
    assert!(account_emails(&snapshot, HarnessId::Pi).is_empty());
    assert!(!slots_dir.join(format!("{bob_id}.json")).exists());
    let live: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&pi_file).expect("remaining pi auth"))
            .expect("pi auth json");
    assert!(live.get("openai-codex").is_none());
    assert_eq!(live["openrouter"]["key"], "keep");
    assert!(
        account_emails(
            &accounts.list(false).await.expect("list again"),
            HarnessId::Pi
        )
        .is_empty(),
        "forgotten live login must not be re-snapshotted"
    );
}

#[test]
fn snapshot_wire_shape() {
    let snapshot = AgentAccountsSnapshot::default();
    let value = serde_json::to_value(&snapshot).expect("serializes");
    assert_eq!(value, serde_json::json!({ "accounts": [], "warnings": [] }));
}

// ---------------------------------------------------------------------------
// Uploads
// ---------------------------------------------------------------------------

#[tokio::test]
async fn uploads_chunk_commit_readback_and_jail() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let uploads = Uploads::new(tmp.path());

    // 100KB of pseudo-random bytes, staged as three positional base64 chunks
    // (out of order, with one retried) — chunk boundaries are multiples of 3
    // bytes so independent base64 strings concatenate losslessly.
    let payload: Vec<u8> = (0..100_002u32)
        .map(|i| (i.wrapping_mul(31) % 251) as u8)
        .collect();
    let chunks: Vec<String> = payload.chunks(45_000).map(|c| BASE64.encode(c)).collect();
    assert_eq!(chunks.len(), 3);
    uploads
        .append("up-1", &chunks[2], Some(2))
        .expect("chunk 2");
    uploads
        .append("up-1", &chunks[0], Some(0))
        .expect("chunk 0");
    uploads
        .append("up-1", &chunks[0], Some(0))
        .expect("chunk 0 retry is idempotent");
    uploads
        .append("up-1", &chunks[1], Some(1))
        .expect("chunk 1");
    let path = uploads.commit("up-1", "photo.png").expect("commit");
    assert!(path.ends_with("up-1-photo.png"), "path: {path}");
    assert_eq!(std::fs::read(&path).expect("committed file"), payload);

    // Readback: chunked reassembly round-trips.
    let mut assembled = Vec::new();
    let mut offset = 0u64;
    loop {
        let chunk = uploads.read_chunk(&path, offset, &[]).expect("read chunk");
        assert_eq!(chunk.mime_type, "image/png");
        assert_eq!(chunk.name, "up-1-photo.png");
        assembled.extend(BASE64.decode(&chunk.data).expect("chunk base64"));
        offset = chunk.next_offset;
        if chunk.done {
            break;
        }
    }
    assert_eq!(assembled, payload);

    // Missing chunk → commit fails.
    uploads
        .append("up-2", &chunks[0], Some(0))
        .expect("chunk 0");
    uploads
        .append("up-2", &chunks[2], Some(2))
        .expect("chunk 2 (hole at 1)");
    assert!(
        uploads.commit("up-2", "holey.png").is_err(),
        "hole detected"
    );

    // Path jail: files outside the uploads dir (and outside any allowed cwd
    // root) are rejected, including traversal attempts and the dir itself.
    let outside = tmp.path().join("outside.png");
    std::fs::write(&outside, b"nope").expect("outside file");
    assert!(
        uploads
            .read_chunk(&outside.to_string_lossy(), 0, &[])
            .is_err()
    );
    assert!(uploads.read_chunk("/etc/passwd", 0, &[]).is_err());
    let sneaky = format!("{}/../outside.png", uploads.dir().display());
    assert!(
        uploads.read_chunk(&sneaky, 0, &[]).is_err(),
        "traversal rejected"
    );
    // …but a workspace-known cwd root admits its files.
    let ok = uploads
        .read_chunk(&outside.to_string_lossy(), 0, &[tmp.path().to_path_buf()])
        .expect("cwd-rooted read");
    assert_eq!(BASE64.decode(&ok.data).expect("data"), b"nope");
    // Files the user uploaded read back whatever their type (a queued message
    // restores them for editing)…
    let text = PathBuf::from(uploads.dir()).join("notes.txt");
    std::fs::create_dir_all(uploads.dir()).expect("uploads dir");
    std::fs::write(&text, b"text").expect("txt");
    let chunk = uploads
        .read_chunk(&text.to_string_lossy(), 0, &[])
        .expect("an uploaded file reads back");
    assert_eq!(chunk.mime_type, "application/octet-stream");
    // …but a non-image in a workspace root is still refused (paku parity).
    let in_root = tmp.path().join("notes.txt");
    std::fs::write(&in_root, b"text").expect("txt");
    assert!(
        uploads
            .read_chunk(&in_root.to_string_lossy(), 0, &[tmp.path().to_path_buf()])
            .is_err()
    );

    // Bogus upload ids never become paths.
    assert!(uploads.append("../evil", "aGk=", None).is_err());
    assert!(uploads.commit("unknown-upload", "x.png").is_err());
}

// ---------------------------------------------------------------------------
// Titling
// ---------------------------------------------------------------------------

// Scripted Pi identity for the title boundary, not genuine CLI coverage.
// Mock remains the coding-session fixture; only Pi can generate app titles.
struct PiTitleProbe(MockHarness);
#[async_trait::async_trait]
impl paku_harness::Harness for PiTitleProbe {
    fn id(&self) -> HarnessId {
        HarnessId::Pi
    }
    fn display_name(&self) -> &str {
        "Scripted Pi title probe"
    }
    fn supports_steering(&self) -> bool {
        false
    }
    fn steering_mode(&self) -> paku_proto::SteeringMode {
        paku_proto::SteeringMode::TurnBoundary
    }
    fn reasoning_levels(&self) -> &[paku_proto::ReasoningLevel] {
        &[]
    }
    async fn models(&self) -> Result<Vec<paku_proto::Model>, paku_harness::HarnessError> {
        Ok(vec![])
    }
    async fn run(
        &self,
        _: paku_proto::RunRequest,
        _: paku_harness::RunControls,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<AgentEvent, paku_harness::HarnessError>>,
        paku_harness::HarnessError,
    > {
        Err(paku_harness::HarnessError::Protocol(
            "titles must not invoke the coding entrypoint".into(),
        ))
    }
    async fn run_title(
        &self,
        request: paku_proto::RunRequest,
        controls: paku_harness::RunControls,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<AgentEvent, paku_harness::HarnessError>>,
        paku_harness::HarnessError,
    > {
        paku_harness::Harness::run_title(&self.0, request, controls).await
    }
}

#[tokio::test]
async fn titling_e2e_names_chat_and_renames_worktree_branch() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // Worktree root must be inside the tempdir (EngineCore reads the env-less
    // default otherwise) — create the worktree with a dedicated Repos handle.
    let repo_dir = tmp.path().join("repo");
    init_repo(&repo_dir).await;
    let repos = Repos::with_worktrees_root(
        &tmp.path().join("data"),
        "device-test",
        tmp.path().join("worktrees"),
    );
    let worktree = repos
        .create_worktree(&repo_dir, "main")
        .await
        .expect("worktree");

    let core = assemble_with_mock(
        &tmp.path().join("data"),
        vec![
            AgentEvent::TextDelta {
                text: "Fix Login Flow".into(),
            },
            AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: None,
            },
        ],
    );
    core.registry.register(Arc::new(PiTitleProbe(MockHarness {
        script: vec![
            AgentEvent::TextDelta {
                text: "Fix Login Flow".into(),
            },
            AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: None,
            },
        ],
    })));
    let chat_id = "chat-title-1";
    core.workspace
        .create_space(
            "space-title",
            &core.device_id,
            &repo_dir.to_string_lossy(),
            None,
            true,
        )
        .expect("create space");
    core.workspace
        .create_chat(
            chat_id,
            Some("space-title"),
            None,
            None,
            Some(worktree.path.clone()),
        )
        .expect("create chat");
    core.workspace
        .set_chat_branch(chat_id, &worktree.branch)
        .expect("set branch");

    let request = paku_proto::RunRequest {
        mcp: None,
        prompt: "please fix the login flow".into(),
        harness: None,
        model: None,
        reasoning: None,
        model_options: serde_json::Map::new(),
        cwd: worktree.path.clone(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: true,
        attachments: Vec::new(),
        worktree: None,
        resume: None,
    };
    core.sessions
        .dispatch(chat_id, HarnessId::Mock, request, None)
        .await
        .expect("dispatch");

    // The separate Pi-title probe supplies the restricted titling output.
    let chat = wait_for("chat title", || {
        core.workspace
            .chat(chat_id)
            .ok()
            .flatten()
            .filter(|c| c.title.as_deref().is_some_and(|t| !t.is_empty()))
    })
    .await;
    assert_eq!(chat.title.as_deref(), Some("Fix Login Flow"));
    // Branch renamed from the title, chat row updated to match.
    assert_eq!(chat.branch.as_deref(), Some("paku/fix-login-flow"));
    let head = tokio::process::Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(&worktree.path)
        .output()
        .await
        .expect("git");
    assert_eq!(
        String::from_utf8_lossy(&head.stdout).trim(),
        "paku/fix-login-flow"
    );

    // A titled chat is never re-titled: rename, run again, title sticks.
    core.workspace
        .rename_chat(chat_id, "My Custom Name")
        .expect("rename");
    let request = paku_proto::RunRequest {
        mcp: None,
        prompt: "another request".into(),
        harness: None,
        model: None,
        reasoning: None,
        model_options: serde_json::Map::new(),
        cwd: worktree.path.clone(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: true,
        attachments: Vec::new(),
        worktree: None,
        resume: None,
    };
    core.sessions
        .dispatch(chat_id, HarnessId::Mock, request, None)
        .await
        .expect("second dispatch");
    tokio::time::sleep(Duration::from_millis(400)).await;
    let chat = core.workspace.chat(chat_id).expect("chat").expect("row");
    assert_eq!(chat.title.as_deref(), Some("My Custom Name"));
    core.shutdown().await;
}

#[tokio::test]
async fn rename_worktree_branch_guards_and_collisions() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo_dir = tmp.path().join("repo");
    init_repo(&repo_dir).await;
    let repos = Repos::with_worktrees_root(
        &tmp.path().join("data"),
        "device-test",
        tmp.path().join("worktrees"),
    );
    let wt = repos
        .create_worktree(&repo_dir, "main")
        .await
        .expect("worktree");
    let wt_path = Path::new(&wt.path);

    // Guard: expected branch mismatch → no-op, returns the actual branch.
    let unchanged = repos
        .rename_worktree_branch(wt_path, "paku/not-this-one", "Some Title")
        .await
        .expect("guarded");
    assert_eq!(unchanged, wt.branch);

    // Happy path: renamed to the title slug.
    let renamed = repos
        .rename_worktree_branch(wt_path, &wt.branch, "Add Dark Mode!")
        .await
        .expect("renamed");
    assert_eq!(renamed, "paku/add-dark-mode");

    // Already renamed → the guard (branch no longer paku/<folder>) makes any
    // further title rename a no-op.
    let again = repos
        .rename_worktree_branch(wt_path, "paku/add-dark-mode", "Different Title")
        .await
        .expect("second rename");
    assert_eq!(again, "paku/add-dark-mode");

    // Collision: a second worktree whose title slug already exists gets the
    // stable hash suffix.
    let wt2 = repos
        .create_worktree(&repo_dir, "main")
        .await
        .expect("worktree 2");
    let renamed2 = repos
        .rename_worktree_branch(Path::new(&wt2.path), &wt2.branch, "Add Dark Mode!")
        .await
        .expect("suffixed rename");
    assert!(
        renamed2.starts_with("paku/add-dark-mode-")
            && renamed2.len() == "paku/add-dark-mode-".len() + 6,
        "suffixed: {renamed2}"
    );

    // Slug edge cases.
    assert_eq!(
        worktree_branch_from_title("  Fix `Login` Flow!  "),
        "paku/fix-login-flow"
    );
    assert_eq!(worktree_branch_from_title("***"), "paku/update");
    assert_eq!(
        worktree_branch_from_title("Cafe's Dark Mode"),
        "paku/cafes-dark-mode"
    );
}

// ---------------------------------------------------------------------------
// RPC dispatch
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rpc_dispatch_for_m5c_methods() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let core = assemble_with_mock(&tmp.path().join("data"), Vec::new());
    let client = paku_rpc::memory_client(core.rpc_service());

    // Uploads: chunk → commit → readback over the wire.
    let payload = b"fake png bytes".to_vec();
    let ok = client
        .call(
            methods::UPLOAD_CHUNK,
            serde_json::json!({ "uploadId": "rpc-up", "data": BASE64.encode(&payload), "seq": 0 }),
        )
        .await
        .expect("UploadChunk");
    assert_eq!(ok["ok"], true);
    let committed = client
        .call(
            methods::UPLOAD_COMMIT,
            serde_json::json!({ "uploadId": "rpc-up", "fileName": "shot.png" }),
        )
        .await
        .expect("UploadCommit");
    let path = committed["path"].as_str().expect("path").to_string();
    assert!(path.ends_with("rpc-up-shot.png"));
    let chunk = client
        .call(
            methods::READ_ATTACHMENT_CHUNK,
            serde_json::json!({ "path": path, "offset": 0 }),
        )
        .await
        .expect("ReadAttachmentChunk");
    assert_eq!(chunk["mimeType"], "image/png");
    assert_eq!(chunk["done"], true);
    assert_eq!(
        BASE64
            .decode(chunk["data"].as_str().expect("data"))
            .expect("base64"),
        payload
    );
    // Jail holds over RPC too.
    assert!(
        client
            .call(
                methods::READ_ATTACHMENT_CHUNK,
                serde_json::json!({ "path": "/etc/passwd", "offset": 0 })
            )
            .await
            .is_err()
    );

    // Agent accounts: snapshot shape (this machine's real Pi state may or may
    // not include logins — assert the envelope, not the contents).
    let snapshot = client
        .call(methods::LIST_AGENT_ACCOUNTS, serde_json::json!({}))
        .await
        .expect("ListAgentAccounts");
    assert!(snapshot["accounts"].is_array());
    assert!(snapshot["warnings"].is_array());

    // The provider param reaches the engine: Pi's Claude login is refused
    // with its reason (no port bound, no network).
    let refused = client
        .call(
            methods::START_AGENT_LOGIN,
            serde_json::json!({ "harness": "pi", "provider": "anthropic" }),
        )
        .await
        .expect_err("pi claude login is not offered");
    assert!(refused.to_string().contains("/login"), "{refused}");

    // Error paths: junk account ids and dead logins fail cleanly.
    assert!(
        client
            .call(
                methods::FORGET_AGENT_ACCOUNT,
                serde_json::json!({ "harness": "pi", "accountId": "../nope" })
            )
            .await
            .is_err()
    );
    assert!(
        client
            .call(
                methods::ACTIVATE_AGENT_ACCOUNT,
                serde_json::json!({ "harness": "pi", "accountId": "0123456789abcdef" })
            )
            .await
            .is_err(),
        "unknown slot cannot be activated"
    );
    assert!(
        client
            .call(
                methods::COMPLETE_AGENT_LOGIN,
                serde_json::json!({ "loginId": "no-such-login", "code": "x#y" })
            )
            .await
            .is_err()
    );
    core.shutdown().await;
}
