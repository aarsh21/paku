//! Removed agent identities must fail at application RPC boundaries, not become Pi.
use paku_engine::{EngineCore, default_registry};
use paku_proto::HarnessId;
use paku_rpc::methods;
use serde_json::json;
use std::sync::Arc;

const REMOVED: &[&str] = &[
    "claude-code",
    "claude",
    "codex",
    "cursor",
    "grok",
    "devin",
    "hermes",
    "opencode",
    "antigravity",
    "acp",
    "pi-acp",
    "openai-codex",
    "unknown-agent",
];

#[test]
fn removed_harness_identities_are_not_aliases_for_pi() {
    assert_eq!(HarnessId::default(), HarnessId::Pi);
    assert_eq!(
        serde_json::from_value::<HarnessId>(json!("pi")).unwrap(),
        HarnessId::Pi
    );
    assert_eq!(
        serde_json::from_value::<HarnessId>(json!("mock")).unwrap(),
        HarnessId::Mock
    );
    for identity in REMOVED {
        assert!(
            serde_json::from_value::<HarnessId>(json!(identity)).is_err(),
            "{identity} must not silently become Pi"
        );
    }
    let registry = default_registry();
    let ids: Vec<_> = registry.descriptors().into_iter().map(|d| d.id).collect();
    assert_eq!(
        ids,
        vec![HarnessId::Pi],
        "Mock and removed agents never enter the production catalog"
    );
    assert!(
        registry.resolve(HarnessId::Mock).is_err(),
        "Mock is only explicitly injectable in tests"
    );
}

#[tokio::test]
async fn application_rpcs_reject_removed_harnesses_before_provider_work() {
    let dir = tempfile::tempdir().unwrap();
    let core = EngineCore::assemble(
        dir.path(),
        Arc::new(default_registry()),
        HarnessId::Pi,
        None,
    )
    .unwrap();
    let client = paku_rpc::memory_client(core.rpc_service());
    let before = core.registry.title_settings();
    for identity in REMOVED {
        for method in [
            methods::LIST_MODELS,
            methods::LIST_COMMANDS,
            methods::LIST_SKILLS,
            methods::INSTALL_HARNESS,
            methods::CANCEL_INSTALL,
            methods::SET_HARNESS_ENABLED,
            methods::SET_TITLE_SETTINGS,
            methods::START_AGENT_LOGIN,
            methods::ACTIVATE_AGENT_ACCOUNT,
            methods::FORGET_AGENT_ACCOUNT,
        ] {
            let error = client
                .call(
                    method,
                    json!({
                        "harness":identity, "enabled":true, "accountId":"0123456789abcdef",
                        "provider":"openai-codex",
                    }),
                )
                .await
                .expect_err("unsupported identity must fail, never dispatch another agent");
            assert!(
                error.to_string().contains("unknown variant"),
                "{method} {identity}: {error}"
            );
        }
        let chat_id = format!("unsupported-{identity}");
        client.call(methods::MUTATE, json!({
            "op":"createChat", "chatId":chat_id, "deviceId":core.device_id,
            "config":{"harness":identity,"model":null,"reasoning":null,"sandbox":"workspace-write"},
        })).await.expect_err("an unsupported chat config cannot be stored as Pi");
        assert!(core.workspace.chat(&chat_id).unwrap().is_none());
    }
    assert_eq!(
        core.registry.title_settings(),
        before,
        "rejected title identities do not mutate preferences"
    );
    core.shutdown().await;
}
