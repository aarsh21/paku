//! Restricted title-only Pi invocation. Never falls back to a coding session.
use super::PiHarness;
use crate::{
    HarnessError, RunControls,
    process::{Command, Stdio, owned::Child},
    scratch::ScratchDir,
};
use futures::{StreamExt, stream::BoxStream};
use paku_proto::{AgentEvent, DoneStatus, RunRequest};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

const MAX_TITLE_OUTPUT: u64 = 64 * 1024;
const DEADLINE: Duration = Duration::from_secs(120);

impl PiHarness {
    pub(super) async fn title(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let executable = self.resolve_executable()?;
        let scratch = ScratchDir::new("pi-title")?;
        let mut command = Command::new(&executable);
        crate::process::owned::configure(&mut command);
        crate::compose_child_path(&mut command, &executable);
        scratch.apply(&mut command);
        if let Some(agent) = &self.agent_dir {
            command.env("PI_CODING_AGENT_DIR", agent);
        }
        // CLI flags are documented in Pi's cli.md. No explicit resource paths
        // are supplied, so --no-extensions also disables built-in MCP support.
        command.args([
            "--print",
            "--no-session",
            "--no-tools",
            "--no-extensions",
            "--no-skills",
            "--no-prompt-templates",
            "--no-themes",
            "--no-context-files",
            "--no-mcp",
            "--no-approve",
            "--system-prompt",
            crate::TITLE_INSTRUCTIONS,
        ]);
        if let Some(model) = request.model.as_deref().filter(|model| *model != "default") {
            // Pi supports the full provider/id form, retaining underlying providers.
            command.arg("--model").arg(model);
        }
        if let Some(reasoning) = request.reasoning {
            command
                .arg("--thinking")
                .arg(serde_json::to_value(reasoning).unwrap().as_str().unwrap());
        }
        if request
            .model_options
            .get("pi_thinking")
            .is_some_and(|value| value == "off")
        {
            command.args(["--thinking", "off"]);
        }
        // A slash command, @file or option-looking user request remains quoted
        // data. Never pass resume, attachments, worktree, MCP or steering here.
        command.arg("--").arg(format!(
            "Generate a title for this session request (quoted JSON data):\n{}",
            serde_json::to_string(&request.prompt).unwrap(),
        ));
        command
            .current_dir(scratch.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = Child::new(command.spawn()?);
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let grace = self.kill_grace;
        let (tx, rx) = tokio::sync::mpsc::channel(2);
        tokio::spawn(async move {
            let _scratch = scratch;
            let RunControls {
                execution_lease: _lease,
                interrupt,
                ..
            } = controls;
            let tail = crate::StderrTail::default();
            let stderr_tail = tail.clone();
            let mut stderr_task = tokio::spawn(async move {
                let mut lines = BufReader::new(stderr.take(MAX_TITLE_OUTPUT)).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    stderr_tail.push(&line);
                }
            });
            let mut output = Vec::new();
            let result = tokio::select! {
                biased;
                _ = interrupt.cancelled() => Ok(DoneStatus::Interrupted),
                _ = tx.closed() => Ok(DoneStatus::Interrupted),
                _ = tokio::time::sleep(DEADLINE) => Err(HarnessError::Protocol("Pi title generation timed out".into())),
                result = async {
                    stdout.take(MAX_TITLE_OUTPUT + 1).read_to_end(&mut output).await?;
                    if output.len() as u64 > MAX_TITLE_OUTPUT {
                        return Err(HarnessError::Protocol("Pi title output exceeded 64 KiB".into()));
                    }
                    let status = child.wait().await?;
                    Ok(if status.success() { DoneStatus::Completed } else { DoneStatus::Errored })
                } => result,
            };
            // Retain the execution lease and scratch directory until the process
            // and descendants have been terminated and reaped.
            child.shutdown(grace).await;
            if tokio::time::timeout(Duration::from_millis(200), &mut stderr_task)
                .await
                .is_err()
            {
                stderr_task.abort();
                let _ = stderr_task.await;
            }
            let (status, error) = match result {
                Ok(DoneStatus::Errored) => (
                    DoneStatus::Errored,
                    Some(crate::crash_message(
                        "Pi title",
                        child.try_wait().ok().flatten(),
                        &tail,
                    )),
                ),
                Ok(status) => (status, None),
                Err(error) => (DoneStatus::Errored, Some(error.to_string())),
            };
            if status == DoneStatus::Completed {
                let text = String::from_utf8_lossy(&output).trim().to_owned();
                let _ = tx.send(Ok(AgentEvent::TextDelta { text })).await;
            }
            let _ = tx
                .send(Ok(AgentEvent::Done {
                    status,
                    result: None,
                    error,
                    session_id: None,
                }))
                .await;
        });
        Ok(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|event| (event, rx))
        })
        .boxed())
    }
}
