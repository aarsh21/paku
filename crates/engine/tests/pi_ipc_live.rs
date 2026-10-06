//! Production engine IPC + genuine installed Pi RPC, with an isolated local model.
//! No scripted harness, paid API, user credentials, or production relay is used.
//! Build `cargo build -p paku` first (or set PAKU_TEST_APP_BIN). This launches
//! the actual headless application, including its executable-backed MCP server.
#![cfg(unix)]

use paku_doc::transcript_delta::{TranscriptFrame, TranscriptUpdate, apply_transcript_frame};
use paku_doc::{
    MessagePart, MessageRole, MessageStatus, SessionCommandPayload, SessionMessageEntry,
};
use paku_engine::EngineConfig;
use paku_harness::PiHarness;
use paku_proto::{HarnessId, RunRequest, SandboxLevel};
use paku_rpc::{RpcClient, RpcSubscription, connect_ws, methods};
use serde_json::{Value, json};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

const CHAT: &str = "pi-ipc-native";
const DEADLINE: Duration = Duration::from_secs(30);

async fn start(
    config: EngineConfig,
    executable: &Path,
    agent: &Path,
) -> (RpcClient, tokio::process::Child) {
    let url = format!("ws://127.0.0.1:{}", config.ipc_port);
    let binary = std::env::var_os("PAKU_TEST_APP_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/paku"));
    let home = config.data_dir.parent().unwrap().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let daemon = tokio::process::Command::new(binary)
        .arg("headless")
        .env("HOME", home)
        .env("PAKU_DATA_DIR", &config.data_dir)
        .env("PAKU_IPC_PORT", config.ipc_port.to_string())
        .env("PAKU_HARNESS", "pi")
        .env("PAKU_EDGE_URL", &config.edge_url)
        .env("PAKU_WORKOS_CLIENT_ID", "")
        .env_remove("PAKU_EDGE_TOKEN")
        .env_remove("PAKU_ORG_ID")
        .env("PI_EXECUTABLE", executable)
        .env("PI_CODING_AGENT_DIR", agent)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .expect("build the real Paku application before running this test");
    let client = tokio::time::timeout(DEADLINE, async {
        loop {
            if let Ok(client) = connect_ws(&url).await {
                break client;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("production IPC listener starts");
    client.call(methods::ENGINE_READY, json!({})).await.unwrap();
    (client, daemon)
}

async fn stop(client: &RpcClient, mut daemon: tokio::process::Child) {
    client.call(methods::STOP_ENGINE, json!({})).await.unwrap();
    let status = tokio::time::timeout(DEADLINE, daemon.wait())
        .await
        .expect("engine drains Pi before shutdown")
        .unwrap();
    assert!(status.success(), "Paku exited with {status}");
}

fn has_reply(entries: &[SessionMessageEntry], reply: &str) -> bool {
    entries.iter().any(|entry| {
        entry.role == MessageRole::Assistant
            && entry.status == Some(MessageStatus::Complete)
            && entry
                .parts
                .iter()
                .any(|part| matches!(part, MessagePart::Text { text, .. } if text == reply))
    })
}

async fn transcript(client: &RpcClient) -> (RpcSubscription, Vec<SessionMessageEntry>) {
    let mut watch = client
        .subscribe_scoped(methods::WATCH_DOC_MESSAGES, json!({"chatId":CHAT}))
        .await
        .unwrap();
    let value = tokio::time::timeout(DEADLINE, watch.recv())
        .await
        .unwrap()
        .unwrap();
    let update: TranscriptUpdate = serde_json::from_value(value).unwrap();
    let TranscriptFrame::Reset { reset } = update.frame else {
        panic!("opening read is a reset")
    };
    (watch, reset)
}

async fn send(client: &RpcClient, cwd: &Path, prompt: &str, message_id: &str) {
    let request = RunRequest {
        prompt: prompt.into(),
        harness: Some(HarnessId::Pi),
        model: Some("paku-probe/mock".into()),
        reasoning: None,
        model_options: Default::default(),
        cwd: cwd.display().to_string(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: true,
        resume: None,
        attachments: vec![],
        worktree: None,
        mcp: None,
    };
    let command = SessionCommandPayload::Run {
        request,
        message_id: message_id.into(),
    };
    let queued = client
        .call(
            methods::QUEUE_COMMAND,
            json!({"chatId":CHAT,"command":command}),
        )
        .await
        .unwrap();
    assert!(queued["commandId"].is_string());
}

async fn receive_reply(
    watch: &mut RpcSubscription,
    entries: &mut Vec<SessionMessageEntry>,
    reply: &str,
) -> usize {
    tokio::time::timeout(DEADLINE, async {
        let mut frames = 0;
        while !has_reply(entries, reply) {
            let update: TranscriptUpdate =
                serde_json::from_value(watch.recv().await.expect("transcript remains live"))
                    .unwrap();
            apply_transcript_frame(entries, update.frame)
                .expect("transcript frame remains consistent");
            frames += 1;
        }
        frames
    })
    .await
    .expect("actual Pi reply arrives over IPC")
}

async fn native_session(client: &RpcClient) -> String {
    let mut chats = client
        .subscribe_scoped(methods::WATCH_CHATS, json!({}))
        .await
        .unwrap();
    tokio::time::timeout(DEADLINE, async {
        loop {
            let rows = chats.recv().await.expect("chat watch remains live");
            let rows: Vec<paku_proto::Chat> = serde_json::from_value(rows).unwrap();
            if let Some(id) = rows
                .into_iter()
                .find(|chat| chat.id == CHAT)
                .and_then(|chat| chat.harness_session_id)
            {
                break id;
            }
        }
    })
    .await
    .expect("native Pi UUID is persisted in the application chat")
}

#[tokio::test]
#[ignore = "requires installed native Pi; isolated local provider, no paid API; run alone"]
async fn genuine_pi_engine_ipc_stream_read_and_restart_resume() {
    let dir = tempfile::tempdir().unwrap();
    let agent = dir.path().join("agent");
    let extensions = agent.join("extensions");
    std::fs::create_dir_all(&extensions).unwrap();
    std::fs::write(
        agent.join("settings.json"),
        r#"{"retry":{"enabled":false}}"#,
    )
    .unwrap();
    std::fs::write(
        extensions.join("probe.ts"),
        include_str!("../../harness/tests/fixtures/pi-rpc-probe.ts"),
    )
    .unwrap();
    let executable = PiHarness::new()
        .resolve_executable()
        .expect("native Pi CLI must be installed");
    let quote = |path: &Path| format!("'{}'", path.display().to_string().replace('\'', "'\\''"));
    let wrapper = dir.path().join("isolated-native-pi");
    // This wrapper executes the genuine installed CLI, not the fake-pi fixture.
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nexport PI_CODING_AGENT_DIR={}\nexec {} \"$@\"\n",
            quote(&agent),
            quote(&executable)
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let cwd = dir.path().join("workspace");
    std::fs::create_dir(&cwd).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let config = EngineConfig {
        data_dir: dir.path().join("engine"),
        edge_url: String::new(),
        edge_token: None,
        ipc_port: port,
        default_harness: HarnessId::Pi,
        org_id: None,
        workos_client_id: None,
    };
    let (client, daemon) = start(config.clone(), &wrapper, &agent).await;
    let info: paku_proto::EngineInfo = client
        .call_as(methods::ENGINE_INFO, json!({}))
        .await
        .unwrap();
    assert!(info.supports(paku_proto::capabilities::PAKU_PI_ONLY_V1));
    assert_eq!(info.workspace_scope, paku_proto::WorkspaceScope::Local);
    let catalog = client
        .call(methods::LIST_HARNESSES, json!({}))
        .await
        .unwrap();
    assert_eq!(
        catalog.as_array().unwrap().len(),
        1,
        "production registry is Pi-only: {catalog}"
    );
    assert_eq!(catalog[0]["id"], "pi");
    assert_eq!(catalog[0]["installed"], true);
    assert_eq!(catalog[0]["enabled"], true);
    let device = client.call(methods::LOCAL_DEVICE, json!({})).await.unwrap();
    client.call(methods::MUTATE, json!({"op":"createSpace","spaceId":"pi-space","deviceId":device["deviceId"],"path":cwd})).await.unwrap();
    client.call(methods::MUTATE, json!({"op":"createChat","chatId":CHAT,"spaceId":"pi-space","config":{"harness":"pi","model":"paku-probe/mock","reasoning":null,"sandbox":"workspace-write"}})).await.unwrap();
    // Avoid an unrelated auto-title model call changing the native session.
    client
        .call(
            methods::MUTATE,
            json!({"op":"renameChat","chatId":CHAT,"title":"Native Pi IPC probe"}),
        )
        .await
        .unwrap();
    let (mut watch, mut entries) = transcript(&client).await;
    assert!(entries.is_empty());
    send(&client, &cwd, "hello", "first-user").await;
    let streamed_frames = receive_reply(&mut watch, &mut entries, "MOCK:hello").await;
    assert!(streamed_frames > 0, "live transcript changed after sending");
    assert!(
        entries
            .iter()
            .flat_map(|e| &e.parts)
            .all(|p| !matches!(p, MessagePart::Error { .. })),
        "real application MCP and Pi must start without error parts: {entries:?}"
    );
    let first_native = native_session(&client).await;
    drop(watch);
    let (read, snapshot) = transcript(&client).await;
    assert!(
        has_reply(&snapshot, "MOCK:hello"),
        "fresh IPC read replays persisted reply"
    );
    drop(read);
    stop(&client, daemon).await;
    drop(client);
    let (client, daemon) = start(config, &wrapper, &agent).await;
    let (mut watch, mut entries) = transcript(&client).await;
    assert!(
        has_reply(&entries, "MOCK:hello"),
        "restart preserves application transcript"
    );
    send(&client, &cwd, "resume", "second-user").await;
    receive_reply(&mut watch, &mut entries, "MOCK:resume").await;
    assert!(
        entries
            .iter()
            .flat_map(|e| &e.parts)
            .all(|p| !matches!(p, MessagePart::Error { .. })),
        "resumed Pi/MCP must have no error parts: {entries:?}"
    );
    let resumed_native = native_session(&client).await;
    assert_eq!(
        resumed_native, first_native,
        "engine restart resumes the same genuine Pi session"
    );
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry.role == MessageRole::User)
            .count(),
        2
    );
    let evidence: Value = json!({"transport":"engine WebSocket IPC","harness":"genuine installed Pi","model":"paku-probe/mock (local extension)","catalog":catalog,"streamedFrames":streamed_frames,"nativeSession":first_native,"resumedSession":resumed_native,"entries":entries});
    println!("PI_IPC_EVIDENCE={evidence}");
    if let Ok(root) = std::env::var("PAKU_EVIDENCE_DIR") {
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            Path::new(&root).join("pi-engine-ipc.json"),
            serde_json::to_vec_pretty(&evidence).unwrap(),
        )
        .unwrap();
    }
    drop(watch);
    stop(&client, daemon).await;
}
