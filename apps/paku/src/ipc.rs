//! Verify product identity before using a daemon's IPC, even on overridden ports.
use paku_proto::EngineInfo;
use paku_rpc::RpcClient;

pub async fn connect_paku_engine(port: u16) -> anyhow::Result<(RpcClient, EngineInfo)> {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        let client = paku_rpc::connect_ws(&format!("ws://127.0.0.1:{port}"))
            .await
            .map_err(|e| anyhow::anyhow!("no Paku engine on 127.0.0.1:{port}: {e}"))?;
        let info: EngineInfo = client
            .call_as(paku_rpc::methods::ENGINE_INFO, serde_json::json!({}))
            .await?;
        anyhow::ensure!(
            info.supports(paku_proto::capabilities::PAKU_PI_ONLY_V1),
            "Refusing to attach to a non-Paku engine on 127.0.0.1:{port}"
        );
        Ok((client, info))
    })
    .await
    .map_err(|_| anyhow::anyhow!("Paku engine identity check timed out"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    struct Identity(bool);
    #[async_trait::async_trait]
    impl paku_rpc::RpcService for Identity {
        async fn handle(
            &self,
            method: &str,
            _: serde_json::Value,
        ) -> Result<paku_rpc::RpcReply, paku_rpc::RpcError> {
            assert_eq!(
                method,
                paku_rpc::methods::ENGINE_INFO,
                "must verify identity before reading sync state"
            );
            paku_rpc::RpcReply::value(&EngineInfo {
                device_id: "fixture".into(),
                workspace_scope: paku_proto::WorkspaceScope::Local,
                capabilities: if self.0 {
                    paku_proto::capabilities::current()
                } else {
                    vec![paku_proto::capabilities::MESSAGE_QUEUE_V1.into()]
                },
            })
        }
    }
    #[tokio::test]
    async fn sync_rejects_a_compatible_upstream_daemon_on_an_overridden_port() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(paku_rpc::serve_ws_listener(
            listener,
            Arc::new(Identity(false)),
        ));
        let error = crate::sync_cli(port).await.unwrap_err();
        assert!(error.to_string().contains("non-Paku engine"));
        server.abort();
    }
    #[tokio::test]
    async fn current_paku_identity_is_accepted() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(paku_rpc::serve_ws_listener(
            listener,
            Arc::new(Identity(true)),
        ));
        let (_, info) = connect_paku_engine(port).await.unwrap();
        assert_eq!(info.workspace_scope, paku_proto::WorkspaceScope::Local);
        server.abort();
    }
}
