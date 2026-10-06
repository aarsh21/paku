#![cfg(unix)]
use paku_harness::{CatalogFailure, CatalogFailureCode, Harness, PiHarness};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

fn binary(root: &Path) -> PathBuf {
    let path = root.join("pi-fixture");
    std::fs::write(&path, r#"#!/usr/bin/python3
import json, pathlib, sys, os
root = pathlib.Path(__file__).parent
if '--version' in sys.argv:
    print('pi 1.2.3')
    sys.exit(0)
state = json.loads((root / 'state.json').read_text())
with (root / 'pids').open('a') as pids: pids.write(str(os.getpid()) + '\n')
for line in sys.stdin:
    request = json.loads(line)
    if 'id' not in request: continue
    if state.get('malformed'):
        print('{invalid json', flush=True)
        sys.exit(1)
    data = {}
    if request['type'] == 'get_available_models':
        data = {'models': [] if state.get('empty') else (state['models'] if 'models' in state else [{'id':state['id'],'provider':'openai','name':state['id']}])}
    if request['type'] == 'get_available_thinking_levels': data = {'levels':['off','low','max']}
    print(json.dumps({'id':request['id'],'type':'response','command':request['type'], 'success':not state.get('fail'), 'data':data,'error':state.get('error','rate limit')}),flush=True)
"#).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}
fn harness(root: &Path) -> PiHarness {
    PiHarness::new()
        .with_executable(binary(root))
        .with_agent_dir(root.join("agent"))
}
fn state(root: &Path, value: serde_json::Value) {
    std::fs::write(root.join("state.json"), serde_json::to_vec(&value).unwrap()).unwrap();
}
fn reaped(root: &Path) {
    for pid in std::fs::read_to_string(root.join("pids")).unwrap().lines() {
        let pid: i32 = pid.parse().unwrap();
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "discovery child {pid} still exists"
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }
}

#[tokio::test]
async fn pi_catalog_retains_last_good_but_not_auth_failures() {
    use serde_json::json;
    let dir = tempfile::tempdir().unwrap();
    let pi = harness(dir.path());
    state(dir.path(), json!({"id":"account-model"}));
    let first = pi.model_catalog(true).await.unwrap();
    assert_eq!(first.source, "live");
    assert_eq!(first.models[0].id, "openai/account-model");
    state(dir.path(), json!({"fail":true,"id":"unused"}));
    let retained = pi.model_catalog(true).await.unwrap();
    assert_eq!(retained.source, "cache");
    assert_eq!(retained.models, first.models);
    assert!(harness(dir.path()).model_catalog(false).await.is_err());
    state(
        dir.path(),
        json!({"fail":true,"id":"unused","error":"not logged in"}),
    );
    let error = pi.model_catalog(true).await.unwrap_err();
    assert_eq!(
        CatalogFailure::classify(&error),
        CatalogFailureCode::AuthRequired
    );
    assert!(pi.models().await.is_err());
    reaped(dir.path());
}

#[tokio::test]
async fn pi_empty_and_malformed_catalogs_reap_children_and_recover() {
    use serde_json::json;
    let dir = tempfile::tempdir().unwrap();
    let pi = harness(dir.path());
    state(dir.path(), json!({"empty":true}));
    assert!(pi.model_catalog(true).await.is_err());
    reaped(dir.path());
    state(
        dir.path(),
        json!({"models":[
            {"id":"shared-model","provider":"openai","name":"OpenAI model"},
            {"id":"shared-model","provider":"anthropic","name":"Anthropic model"}
        ]}),
    );
    let good = pi.model_catalog(true).await.unwrap();
    assert_eq!(
        good.models
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
        ["openai/shared-model", "anthropic/shared-model"]
    );
    assert!(good.models.iter().all(|m| {
        m.reasoning_levels
            .contains(&paku_proto::ReasoningLevel::Max)
    }));
    for failure in [json!({"empty":true}), json!({"malformed":true})] {
        state(dir.path(), failure);
        let retained = pi.model_catalog(true).await.unwrap();
        assert_eq!(retained.source, "cache");
        assert_eq!(retained.models, good.models);
        reaped(dir.path());
    }
}

#[test]
fn pi_explicit_agent_context_hashes_auth_models_and_settings_without_exposing_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let pi = harness(dir.path());
    let agent = dir.path().join("agent");
    std::fs::create_dir_all(&agent).unwrap();
    for file in ["auth.json", "models.json", "settings.json"] {
        let before = pi.model_context().unwrap().unwrap().hash;
        std::fs::write(agent.join(file), "first-account-secret").unwrap();
        let first = pi.model_context().unwrap().unwrap().hash;
        assert_ne!(first, before, "{file}");
        assert!(!first.contains("secret"));
        std::fs::write(agent.join(file), "second-account-secret").unwrap();
        assert_ne!(first, pi.model_context().unwrap().unwrap().hash, "{file}");
    }
}
