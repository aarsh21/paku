//! Device-local monitoring and safe mutation of independently-installed agent
//! CLIs. This deliberately does no work from `ListHarnesses`: all subprocess
//! and network probes live behind this coordinator and its watch stream.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures::StreamExt;
use paku_harness::process::{Command, Stdio};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use paku_proto::{
    HarnessId, HarnessInstallSource, HarnessUpdateFailure, HarnessUpdatePhase, HarnessUpdatePolicy,
    HarnessUpdateStatus,
};

use crate::now_ms;
use crate::registry::HarnessRegistry;

const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const MAX_JITTER: u64 = 30 * 60;
const FIRST_RETRY: Duration = Duration::from_secs(5 * 60);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(20);
const UPDATE_TIMEOUT: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Preferences {
    policies: HashMap<HarnessId, HarnessUpdatePolicy>,
    dismissed_versions: HashMap<HarnessId, String>,
}

#[derive(Clone, Copy)]
enum LatestSource {
    Npm(&'static str),
    Manual,
}

enum UpdateCheck {
    Version(String),
    Manual,
}

struct ProviderSpec {
    version_args: &'static [&'static str],
    latest: LatestSource,
    update_args: Option<&'static [&'static str]>,
    manual_command: &'static str,
}

/// A Homebrew cask or formula that owns the resolved CLI. The token is taken
/// from the `Caskroom` or `Cellar` directory, never from installer output.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HomebrewPackage {
    brew: PathBuf,
    cask: bool,
    token: String,
}

impl HomebrewPackage {
    fn upgrade_command(&self) -> String {
        if self.cask {
            format!("brew upgrade --cask {}", self.token)
        } else {
            format!("brew upgrade --formula {}", self.token)
        }
    }

    fn upgrade_args(&self) -> Vec<&str> {
        if self.cask {
            vec!["upgrade", "--cask", self.token.as_str()]
        } else {
            vec!["upgrade", "--formula", self.token.as_str()]
        }
    }
}

enum UpdatePlan {
    Command {
        executable: PathBuf,
        args: &'static [&'static str],
    },
    Homebrew(HomebrewPackage),
}

fn provider(id: HarnessId) -> ProviderSpec {
    match id {
        HarnessId::Pi => ProviderSpec {
            version_args: &["--version"],
            latest: LatestSource::Npm("@earendil-works/pi-coding-agent"),
            update_args: Some(&["update", "--self"]),
            manual_command: "pi update --self",
        },
        _ => ProviderSpec {
            version_args: &["--version"],
            latest: LatestSource::Manual,
            update_args: None,
            manual_command: "",
        },
    }
}

fn update_plan(harness: HarnessId, executable: &Path) -> Result<UpdatePlan, String> {
    ensure_supported(harness)?;
    if let Some(package) = homebrew_package(executable) {
        return Ok(UpdatePlan::Homebrew(package));
    }
    if let Some(args) = provider(harness).update_args {
        return Ok(UpdatePlan::Command {
            executable: executable.to_path_buf(),
            args,
        });
    }
    Err("this provider requires a manual update".into())
}

fn can_apply_update(harness: HarnessId, executable: &Path) -> bool {
    update_plan(harness, executable).is_ok()
}

fn ensure_supported(harness: HarnessId) -> Result<(), String> {
    if harness == HarnessId::Pi {
        Ok(())
    } else {
        Err("updates are only supported for Pi".into())
    }
}

fn manual_update_command(harness: HarnessId, can_apply: bool) -> Option<String> {
    (!can_apply)
        .then(|| provider(harness).manual_command.to_string())
        .filter(|command| !command.is_empty())
}

struct ActiveUpdate {
    cancel: CancellationToken,
    automatic: bool,
    previous_phase: HarnessUpdatePhase,
}

struct Inner {
    registry: Arc<HarnessRegistry>,
    order: Vec<HarnessId>,
    prefs_path: PathBuf,
    prefs: Mutex<Preferences>,
    statuses: Mutex<HashMap<HarnessId, HarnessUpdateStatus>>,
    status_tx: watch::Sender<Vec<HarnessUpdateStatus>>,
    cancellations: Mutex<HashMap<HarnessId, ActiveUpdate>>,
    operation_gates: Mutex<HashMap<HarnessId, Arc<tokio::sync::Mutex<()>>>>,
    check_slots: tokio::sync::Semaphore,
    shutdown: CancellationToken,
    worker: Mutex<Option<tokio::task::JoinHandle<()>>>,
    client: reqwest::Client,
    npm_registry_url: String,
}

/// Cloneable engine service exposed to RPC and the periodic worker.
#[derive(Clone)]
pub struct HarnessUpdateCoordinator {
    inner: Arc<Inner>,
}

/// Cancellation-safety for an RPC/app task disappearing mid-update. Dropping
/// the future must never leave the registry's pending marker set forever.
struct UpdateIntentGuard {
    coordinator: HarnessUpdateCoordinator,
    harness: HarnessId,
    complete: bool,
}

impl UpdateIntentGuard {
    fn finish(&mut self) {
        self.complete = true;
    }
}

impl Drop for UpdateIntentGuard {
    fn drop(&mut self) {
        if self.complete {
            return;
        }
        let previous_phase = lock(&self.coordinator.inner.cancellations)
            .remove(&self.harness)
            .map(|update| update.previous_phase)
            .unwrap_or(HarnessUpdatePhase::ManualActionRequired);
        self.coordinator.inner.registry.end_update(self.harness);
        self.coordinator.mutate(self.harness, |status| {
            if matches!(
                status.phase,
                HarnessUpdatePhase::Installing | HarnessUpdatePhase::Verifying
            ) {
                status.phase = HarnessUpdatePhase::Failed;
                status.error = Some(HarnessUpdateFailure {
                    message: "update ended before verification; inspect the CLI installation"
                        .into(),
                    retryable: true,
                });
            } else {
                status.phase = if status.policy == HarnessUpdatePolicy::Off {
                    HarnessUpdatePhase::Dormant
                } else {
                    previous_phase
                };
            }
        });
    }
}

impl HarnessUpdateCoordinator {
    pub fn new(data_dir: &Path, registry: Arc<HarnessRegistry>) -> Self {
        let prefs_path = data_dir.join("harness-update-prefs.json");
        let prefs: Preferences = std::fs::read_to_string(&prefs_path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        let order: Vec<_> = registry
            .descriptors()
            .into_iter()
            .filter(|descriptor| descriptor.id == HarnessId::Pi)
            .map(|descriptor| descriptor.id)
            .collect();
        let enabled = registry.enabled_set();
        let statuses: HashMap<_, _> = order
            .iter()
            .copied()
            .map(|harness| {
                let policy = prefs.policies.get(&harness).copied().unwrap_or_default();
                let phase = if enabled.contains(&harness) && policy != HarnessUpdatePolicy::Off {
                    HarnessUpdatePhase::Checking
                } else {
                    HarnessUpdatePhase::Dormant
                };
                (
                    harness,
                    HarnessUpdateStatus {
                        harness,
                        installed_version: None,
                        latest_version: None,
                        channel: Some("stable".into()),
                        source: HarnessInstallSource::Unknown,
                        policy,
                        phase,
                        progress: None,
                        checked_at: None,
                        error: None,
                        can_apply: provider(harness).update_args.is_some(),
                        manual_command: Some(provider(harness).manual_command.to_string())
                            .filter(|command| !command.is_empty()),
                    },
                )
            })
            .collect();
        let initial = ordered_snapshot(&order, &statuses);
        let (status_tx, _) = watch::channel(initial);
        Self {
            inner: Arc::new(Inner {
                registry,
                order,
                prefs_path,
                prefs: Mutex::new(prefs),
                statuses: Mutex::new(statuses),
                status_tx,
                cancellations: Mutex::new(HashMap::new()),
                operation_gates: Mutex::new(HashMap::new()),
                check_slots: tokio::sync::Semaphore::new(2),
                shutdown: CancellationToken::new(),
                worker: Mutex::new(None),
                npm_registry_url: "https://registry.npmjs.org".into(),
                client: reqwest::Client::builder()
                    .user_agent(concat!("paku/", env!("CARGO_PKG_VERSION")))
                    .timeout(COMMAND_TIMEOUT)
                    .build()
                    .unwrap_or_default(),
            }),
        }
    }

    /// Start the immediate check plus the six-hour jittered/retry loop. Bare
    /// synchronous test assemblies simply omit the worker and can still use
    /// the explicit RPC methods once running under Tokio.
    pub fn start(&self) {
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let mut worker = lock(&self.inner.worker);
        if worker.is_some() {
            return;
        }
        let coordinator = self.clone();
        *worker = Some(tokio::spawn(async move {
            let mut retry = FIRST_RETRY;
            loop {
                // Shutdown must not wait out slow probes or registry requests.
                let snapshot = tokio::select! {
                    _ = coordinator.inner.shutdown.cancelled() => break,
                    snapshot = coordinator.check_all() => snapshot,
                };
                let failed = snapshot
                    .iter()
                    .any(|status| status.phase == HarnessUpdatePhase::Failed);
                let delay = if failed {
                    let current = retry;
                    retry = (retry * 2).min(CHECK_INTERVAL);
                    current
                } else {
                    retry = FIRST_RETRY;
                    let jitter = (now_ms().unsigned_abs() % MAX_JITTER) + 1;
                    CHECK_INTERVAL + Duration::from_secs(jitter)
                };
                tokio::select! {
                    _ = coordinator.inner.shutdown.cancelled() => break,
                    _ = tokio::time::sleep(delay) => {}
                }
            }
        }));
    }

    pub fn watch(&self) -> watch::Receiver<Vec<HarnessUpdateStatus>> {
        self.inner.status_tx.subscribe()
    }

    pub fn snapshot(&self) -> Vec<HarnessUpdateStatus> {
        ordered_snapshot(&self.inner.order, &lock(&self.inner.statuses))
    }

    pub async fn check_all(&self) -> Vec<HarnessUpdateStatus> {
        let enabled = self.inner.registry.enabled_set();
        // Disabled rows remain visible in Settings but never spawn a probe.
        for id in &self.inner.order {
            let policy = self.policy(*id);
            if (!enabled.contains(id) || policy == HarnessUpdatePolicy::Off)
                && !self.is_mutating(*id)
            {
                self.mutate(*id, |status| {
                    status.phase = HarnessUpdatePhase::Dormant;
                    status.progress = None;
                    status.error = None;
                });
            }
        }
        futures::stream::iter(enabled)
            .filter(|id| {
                futures::future::ready(
                    *id == HarnessId::Pi && self.policy(*id) != HarnessUpdatePolicy::Off,
                )
            })
            .map(|id| {
                let coordinator = self.clone();
                async move { coordinator.check_one(id).await }
            })
            .buffer_unordered(2)
            .collect::<Vec<_>>()
            .await;

        self.snapshot()
    }

    pub async fn check_one(&self, harness: HarnessId) -> Result<(), String> {
        ensure_supported(harness)?;
        self.check_one_inner(harness).await?;
        // Every successful discovery, including policy changes and single-row
        // retries, gets the same automatic-install behavior. The check's
        // operation lock has been released before scheduling the mutation.
        self.schedule_automatic_update(harness);
        Ok(())
    }

    fn automatic_update_ready(&self, harness: HarnessId) -> bool {
        let status = self.status(harness);
        harness == HarnessId::Pi
            && !self.inner.shutdown.is_cancelled()
            && self.inner.registry.enabled_set().contains(&harness)
            && status.policy == HarnessUpdatePolicy::AutoWhenIdle
            && status.phase == HarnessUpdatePhase::Available
            && status.can_apply
            // The Update button can still run `brew upgrade` when Homebrew has
            // not published the upstream release. Automatic installs wait until
            // Homebrew itself reports a newer package.
            && !status
                .manual_command
                .as_deref()
                .is_some_and(unpublished_homebrew_upgrade)
    }

    fn schedule_automatic_update(&self, harness: HarnessId) {
        if !self.automatic_update_ready(harness) {
            return;
        }
        let coordinator = self.clone();
        tokio::spawn(async move {
            let _operation = coordinator.operation_gate(harness).lock_owned().await;
            // A policy change, disable, dismissal, or another update may have
            // won while this task waited for the provider operation slot.
            if coordinator.automatic_update_ready(harness)
                && let Err(error) = coordinator.apply_locked(harness, true).await
            {
                tracing::warn!(?harness, %error, "automatic harness update failed");
            }
        });
    }

    async fn check_one_inner(&self, harness: HarnessId) -> Result<(), String> {
        // Checks and activation refreshes must never overwrite a live
        // mutation phase (especially Installing, which is non-interruptible).
        if self.is_mutating(harness) {
            return Ok(());
        }
        let _operation = self.operation_gate(harness).lock_owned().await;
        // The mutation may have claimed this harness while the check was
        // waiting for its per-provider operation slot.
        if self.is_mutating(harness) {
            return Ok(());
        }
        if self.settle_if_unmonitored(harness) {
            return Ok(());
        }
        let _check_slot = self
            .inner
            .check_slots
            .acquire()
            .await
            .map_err(|_| "agent update checker is shutting down".to_string())?;
        if self.settle_if_unmonitored(harness) {
            return Ok(());
        }
        self.mutate(harness, |status| {
            status.phase = HarnessUpdatePhase::Checking;
            status.progress = None;
            status.error = None;
        });
        let executable = match self.executable(harness) {
            Ok(path) => path,
            Err(error) => {
                if self.settle_if_unmonitored(harness) {
                    return Ok(());
                }
                return self.fail_check(harness, error);
            }
        };
        let spec = provider(harness);
        let version_lease = self.inner.registry.execution_lease(harness).await;
        let installed = run_version_command(harness, &executable, spec.version_args).await;
        drop(version_lease);
        let installed = match installed {
            Ok(version) => version,
            Err(error) => {
                if self.settle_if_unmonitored(harness) {
                    return Ok(());
                }
                return self.fail_check(harness, error);
            }
        };
        let source = classify_source(&executable);
        let can_apply = can_apply_update(harness, &executable);
        if self.settle_if_unmonitored(harness) {
            return Ok(());
        }
        if let Some(package) = homebrew_package(&executable) {
            return self
                .finish_homebrew_check(harness, installed, source, package)
                .await;
        }
        let latest = match spec.latest {
            LatestSource::Npm(package) => self.npm_latest(package).await.map(UpdateCheck::Version),
            LatestSource::Manual => Ok(UpdateCheck::Manual),
        };
        if self.settle_if_unmonitored(harness) {
            return Ok(());
        }
        let (latest, available) = match latest {
            Ok(UpdateCheck::Version(version)) => {
                let dismissed = lock(&self.inner.prefs)
                    .dismissed_versions
                    .get(&harness)
                    .is_some_and(|dismissed| dismissed == &version);
                let available = version_is_newer(&version, &installed) && !dismissed;
                (Some(version), available)
            }
            Ok(UpdateCheck::Manual) => {
                let registry = self.inner.registry.clone();
                self.mutate(harness, |status| {
                    status.installed_version = Some(installed);
                    status.latest_version = None;
                    status.channel = None;
                    status.source = source;
                    status.can_apply = can_apply;
                    status.manual_command = manual_update_command(harness, can_apply)
                        .or_else(|| Some(spec.manual_command.into()));
                    status.phase = if status.policy == HarnessUpdatePolicy::Off
                        || !registry.enabled_set().contains(&harness)
                    {
                        HarnessUpdatePhase::Dormant
                    } else {
                        HarnessUpdatePhase::ManualActionRequired
                    };
                    status.checked_at = Some(now_ms());
                    status.error = None;
                });
                return Ok(());
            }

            Err(error) => {
                return self.fail_check_with_installed(harness, installed, source, error);
            }
        };
        let manual_command = manual_update_command(harness, can_apply);
        let registry = self.inner.registry.clone();
        self.mutate(harness, |status| {
            status.installed_version = Some(installed);
            status.latest_version = latest;
            status.source = source;
            status.can_apply = can_apply;
            status.manual_command = manual_command;
            status.phase = if status.policy == HarnessUpdatePolicy::Off
                || !registry.enabled_set().contains(&harness)
            {
                HarnessUpdatePhase::Dormant
            } else if available {
                HarnessUpdatePhase::Available
            } else {
                HarnessUpdatePhase::Current
            };
            status.checked_at = Some(now_ms());
            status.error = None;
        });
        Ok(())
    }

    /// Homebrew owns this binary, so the installable release is the cask or
    /// formula version. An upstream release that Homebrew has not published yet
    /// still gets an Update button — it runs `brew upgrade` — but is not
    /// installed automatically.
    async fn finish_homebrew_check(
        &self,
        harness: HarnessId,
        installed: String,
        source: HarnessInstallSource,
        package: HomebrewPackage,
    ) -> Result<(), String> {
        let brew_version = match self.homebrew_latest(&package).await {
            Ok(version) => version,
            Err(error) => {
                return self.fail_check_with_installed(harness, installed, source, error);
            }
        };
        let upstream = self
            .upstream_version(harness)
            .await
            .ok()
            .filter(|version| version_is_newer(version, &installed));
        let brew_newer = version_is_newer(&brew_version, &installed);
        let (latest, note) = if brew_newer {
            (brew_version, None)
        } else if let Some(upstream) =
            upstream.filter(|version| version_is_newer(version, &brew_version))
        {
            (upstream, Some(package.upgrade_command()))
        } else {
            (brew_version, None)
        };
        if self.settle_if_unmonitored(harness) {
            return Ok(());
        }
        let dismissed = lock(&self.inner.prefs)
            .dismissed_versions
            .get(&harness)
            .is_some_and(|dismissed| dismissed == &latest);
        let available = version_is_newer(&latest, &installed) && !dismissed;
        let manual_command = if available { note } else { None };
        let registry = self.inner.registry.clone();
        self.mutate(harness, |status| {
            status.installed_version = Some(installed);
            status.latest_version = Some(latest);
            status.source = source;
            status.can_apply = true;
            status.manual_command = manual_command;
            status.phase = if status.policy == HarnessUpdatePolicy::Off
                || !registry.enabled_set().contains(&harness)
            {
                HarnessUpdatePhase::Dormant
            } else if available {
                HarnessUpdatePhase::Available
            } else {
                HarnessUpdatePhase::Current
            };
            status.checked_at = Some(now_ms());
            status.error = None;
        });
        Ok(())
    }

    async fn homebrew_latest(&self, package: &HomebrewPackage) -> Result<String, String> {
        let api = self.homebrew_api_version(package).await;
        let local = self.homebrew_local_version(package).await;
        match (api, local) {
            (Ok(api), Ok(local)) => Ok(if version_is_newer(&local, &api) {
                local
            } else {
                api
            }),
            (Ok(version), Err(_)) | (Err(_), Ok(version)) => Ok(version),
            (Err(api), Err(local)) => Err(format!("{api}; {local}")),
        }
    }

    async fn homebrew_api_version(&self, package: &HomebrewPackage) -> Result<String, String> {
        let kind = if package.cask { "cask" } else { "formula" };
        let url = format!(
            "https://formulae.brew.sh/api/{kind}/{}.json",
            homebrew_url_component(&package.token)
        );
        let response = self
            .inner
            .client
            .get(url)
            .send()
            .await
            .map_err(|error| format!("Homebrew version check failed: {error}"))?
            .error_for_status()
            .map_err(|error| format!("Homebrew version check failed: {error}"))?;
        let body = response
            .text()
            .await
            .map_err(|error| format!("Homebrew version response was invalid: {error}"))?;
        parse_homebrew_version(package.cask, &body)
    }

    async fn homebrew_local_version(&self, package: &HomebrewPackage) -> Result<String, String> {
        let brew = resolve_brew(&package.brew)?;
        let token = package.token.as_str();
        let args: &[&str] = if package.cask {
            &["info", "--cask", "--json=v2", token]
        } else {
            &["info", "--formula", "--json=v2", token]
        };
        // Don't refresh taps on a probe. `brew upgrade` does that when applying.
        let output = run_command_output_env(&brew, args, COMMAND_TIMEOUT, BREW_INFO_ENV).await?;
        parse_homebrew_version(package.cask, &output)
    }

    /// Release feeds that do not spawn the CLI. A failure here only hides the
    /// "published upstream, not yet in Homebrew" note.
    async fn upstream_version(&self, harness: HarnessId) -> Result<String, String> {
        match provider(harness).latest {
            LatestSource::Npm(package) => self.npm_latest(package).await,
            LatestSource::Manual => Err("no separate upstream feed".into()),
        }
    }

    /// Apply one known release. Cancellation is honored while waiting for the
    /// exclusive lease and before mutation; once the vendor updater starts,
    /// the installation phase is intentionally non-interruptible. The owned
    /// task is detached from the requesting RPC so closing Settings, losing a
    /// relay, or timing out a client cannot drop an updater mid-mutation.
    pub async fn apply(&self, harness: HarnessId) -> Result<String, String> {
        ensure_supported(harness)?;
        let coordinator = self.clone();
        tokio::spawn(async move { coordinator.apply_inner(harness).await })
            .await
            .map_err(|error| format!("agent update task failed: {error}"))?
    }

    async fn apply_inner(&self, harness: HarnessId) -> Result<String, String> {
        // A provider probe and mutation must never overlap: a late check
        // result could otherwise overwrite Installing/Verifying state or
        // inspect a binary while its owner is replacing it.
        let _operation = tokio::select! {
            biased;
            _ = self.inner.shutdown.cancelled() => return Err("update cancelled".into()),
            operation = self.operation_gate(harness).lock_owned() => operation,
        };
        self.apply_locked(harness, false).await
    }

    /// Caller holds the provider operation lock through verification.
    async fn apply_locked(&self, harness: HarnessId, automatic: bool) -> Result<String, String> {
        ensure_supported(harness)?;
        if self.inner.shutdown.is_cancelled() {
            return Err("update cancelled".into());
        }
        let current = self
            .snapshot()
            .into_iter()
            .find(|status| status.harness == harness)
            .ok_or_else(|| "unknown harness".to_string())?;
        if !matches!(
            current.phase,
            HarnessUpdatePhase::Available | HarnessUpdatePhase::ManualActionRequired
        ) {
            return Err("no applicable harness update".into());
        }
        if !current.can_apply {
            return Err("this provider requires a manual update".into());
        }
        let executable = self.executable(harness)?;
        let plan = update_plan(harness, &executable)?;
        // A request queued behind a check must inherit shutdown even if it
        // reaches this point after shutdown's cancellation-map snapshot.
        let cancel = self.inner.shutdown.child_token();
        {
            let mut cancellations = lock(&self.inner.cancellations);
            if cancellations.contains_key(&harness) {
                return Err("an update is already in progress".into());
            }
            if automatic && self.policy(harness) != HarnessUpdatePolicy::AutoWhenIdle {
                return Err("update cancelled".into());
            }
            cancellations.insert(
                harness,
                ActiveUpdate {
                    cancel: cancel.clone(),
                    automatic,
                    previous_phase: current.phase,
                },
            );
        }
        self.inner.registry.begin_update(harness);
        let mut intent = UpdateIntentGuard {
            coordinator: self.clone(),
            harness,
            complete: false,
        };
        self.mutate(harness, |status| {
            status.phase = HarnessUpdatePhase::WaitingForIdle;
            status.progress = None;
            status.error = None;
        });
        let lease = tokio::select! {
            _ = cancel.cancelled() => {
                self.finish_cancelled(harness);
                intent.finish();
                return Err("update cancelled".into());
            }
            lease = self.inner.registry.update_lease(harness) => lease,
        };
        if cancel.is_cancelled() || !self.inner.registry.enabled_set().contains(&harness) {
            drop(lease);
            self.finish_cancelled(harness);
            intent.finish();
            return Err("update cancelled".into());
        }
        self.mutate(harness, |status| {
            status.phase = HarnessUpdatePhase::Preparing
        });
        tokio::task::yield_now().await;
        if cancel.is_cancelled() || !self.inner.registry.enabled_set().contains(&harness) {
            drop(lease);
            self.finish_cancelled(harness);
            intent.finish();
            return Err("update cancelled".into());
        }
        let applied = match plan {
            UpdatePlan::Command { executable, args } => {
                match self.begin_install(harness, &cancel) {
                    Ok(()) => run_command(&executable, args, UPDATE_TIMEOUT).await,
                    Err(error) => Err(error),
                }
            }
            UpdatePlan::Homebrew(package) => match resolve_brew(&package.brew) {
                Ok(brew) => {
                    let args = package.upgrade_args();
                    match self.begin_install(harness, &cancel) {
                        Ok(()) => {
                            run_command_output_env(&brew, &args, UPDATE_TIMEOUT, BREW_UPGRADE_ENV)
                                .await
                                .map(drop)
                        }
                        Err(error) => Err(error),
                    }
                }
                Err(error) => Err(error),
            },
        };
        if let Err(error) = applied {
            drop(lease);
            if cancel.is_cancelled() {
                self.finish_cancelled(harness);
            } else {
                self.finish_failed_update(harness, error.clone());
            }
            intent.finish();
            return Err(error);
        }
        self.mutate(harness, |status| status.progress = None);
        self.mutate(harness, |status| {
            status.phase = HarnessUpdatePhase::Verifying
        });
        let verified = match self.executable(harness) {
            Ok(executable) => {
                run_version_command(harness, &executable, provider(harness).version_args)
                    .await
                    .map(|version| (version, executable))
            }
            Err(error) => Err(error),
        };
        let result = match verified {
            Ok((version, executable)) => {
                let expected = current.latest_version.as_deref();
                if let Some(latest) = expected.filter(|latest| version_is_newer(latest, &version)) {
                    let lag = current
                        .manual_command
                        .as_deref()
                        .is_some_and(unpublished_homebrew_upgrade);
                    Err(match homebrew_package(&executable) {
                        Some(package) if lag => format!(
                            "Homebrew is still on {version}. {latest} is not available from `{}` yet",
                            package.upgrade_command()
                        ),
                        Some(_) => format!(
                            "Homebrew upgrade left the CLI on {version}, older than expected {latest}"
                        ),
                        None => {
                            format!("verification returned {version}, older than expected {latest}")
                        }
                    })
                } else {
                    lock(&self.inner.prefs).dismissed_versions.remove(&harness);
                    self.persist_preferences();
                    self.mutate(harness, |status| {
                        status.installed_version = Some(version.clone());
                        status.phase = HarnessUpdatePhase::Updated;
                        status.checked_at = Some(now_ms());
                        status.error = None;
                    });
                    Ok(version)
                }
            }
            Err(error) => Err(format!("post-update verification failed: {error}")),
        };
        drop(lease);
        lock(&self.inner.cancellations).remove(&harness);
        self.inner.registry.end_update(harness);
        // Updated was published before the operation left the active set.
        // Wake shutdown even if it already consumed that final status frame.
        self.inner.status_tx.send_modify(|_| {});
        if let Err(error) = &result {
            self.fail(harness, error.clone()).ok();
        } else {
            let coordinator = self.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(4)).await;
                coordinator.mutate(harness, |status| {
                    if status.phase == HarnessUpdatePhase::Updated {
                        status.phase = if status.policy == HarnessUpdatePolicy::Off {
                            HarnessUpdatePhase::Dormant
                        } else {
                            HarnessUpdatePhase::Current
                        };
                    }
                });
            });
        }
        intent.finish();
        result
    }

    /// Serialize the final cancellation check and installation commit with
    /// `cancel`: once cancellation is accepted, mutation cannot begin.
    fn begin_install(&self, harness: HarnessId, cancel: &CancellationToken) -> Result<(), String> {
        let cancellations = lock(&self.inner.cancellations);
        if cancellations
            .get(&harness)
            .is_some_and(|update| update.automatic)
            && self.policy(harness) != HarnessUpdatePolicy::AutoWhenIdle
        {
            cancel.cancel();
        }
        if cancel.is_cancelled() || !self.inner.registry.enabled_set().contains(&harness) {
            return Err("update cancelled".into());
        }
        self.mutate(harness, |status| {
            status.phase = HarnessUpdatePhase::Installing;
            status.progress = None;
        });
        Ok(())
    }

    pub fn cancel(&self, harness: HarnessId) -> bool {
        let cancellations = lock(&self.inner.cancellations);
        if !matches!(
            self.status(harness).phase,
            HarnessUpdatePhase::WaitingForIdle
                | HarnessUpdatePhase::Preparing
                | HarnessUpdatePhase::Downloading
        ) {
            return false;
        }
        cancellations
            .get(&harness)
            .map(|update| update.cancel.cancel())
            .is_some()
    }

    pub fn dismiss(&self, harness: HarnessId, version: Option<String>) -> HarnessUpdateStatus {
        let version = version.or_else(|| {
            self.snapshot()
                .into_iter()
                .find(|status| status.harness == harness)
                .and_then(|status| status.latest_version)
        });
        if let Some(version) = version {
            lock(&self.inner.prefs)
                .dismissed_versions
                .insert(harness, version);
            self.persist_preferences();
            self.mutate(harness, |status| {
                if status.phase == HarnessUpdatePhase::Available {
                    status.phase = HarnessUpdatePhase::Current;
                }
            });
        }
        self.status(harness)
    }

    pub fn set_policy(
        &self,
        harness: HarnessId,
        policy: HarnessUpdatePolicy,
    ) -> HarnessUpdateStatus {
        let current = self.status(harness);
        let was_applicable = current.phase == HarnessUpdatePhase::Available && current.can_apply;
        {
            // Policy selection and the installation boundary share the same
            // lock: a queued automatic request cannot outlive opting out.
            let active = lock(&self.inner.cancellations);
            lock(&self.inner.prefs).policies.insert(harness, policy);
            if let Some(update) = active.get(&harness)
                && (policy == HarnessUpdatePolicy::Off
                    || (update.automatic && policy != HarnessUpdatePolicy::AutoWhenIdle))
                && !matches!(
                    self.status(harness).phase,
                    HarnessUpdatePhase::Installing
                        | HarnessUpdatePhase::Verifying
                        | HarnessUpdatePhase::Updated
                )
            {
                update.cancel.cancel();
            }
            self.mutate(harness, |status| {
                status.policy = policy;
                if policy == HarnessUpdatePolicy::Off && !active.contains_key(&harness) {
                    status.phase = HarnessUpdatePhase::Dormant;
                    status.error = None;
                }
            });
        }
        self.persist_preferences();
        if policy == HarnessUpdatePolicy::AutoWhenIdle && was_applicable {
            self.schedule_automatic_update(harness);
        } else if policy != HarnessUpdatePolicy::Off {
            let coordinator = self.clone();
            tokio::spawn(async move {
                let _ = coordinator.check_one(harness).await;
            });
        }
        self.status(harness)
    }

    pub fn refresh_enabled(&self) {
        // Cancel before scheduling checks: checks intentionally skip a provider
        // that already owns its operation slot while waiting for an active run.
        let enabled = self.inner.registry.enabled_set();
        for harness in &self.inner.order {
            if !enabled.contains(harness) {
                self.cancel(*harness);
            }
        }
        let coordinator = self.clone();
        tokio::spawn(async move {
            coordinator.check_all().await;
        });
    }

    pub async fn shutdown(&self) {
        self.inner.shutdown.cancel();
        for update in lock(&self.inner.cancellations).values() {
            update.cancel.cancel();
        }
        let worker = lock(&self.inner.worker).take();
        if let Some(worker) = worker {
            let _ = worker.await;
        }
        // Waiting/preparing operations observe cancellation immediately;
        // installing/verifying operations are deliberately non-interruptible
        // and must settle before the engine tears down their environment.
        let mut status = self.watch();
        let settle = async {
            while !lock(&self.inner.cancellations).is_empty() {
                if status.changed().await.is_err() {
                    break;
                }
            }
        };
        if tokio::time::timeout(UPDATE_TIMEOUT, settle).await.is_err() {
            tracing::warn!("timed out waiting for harness update to settle during shutdown");
        }
    }

    fn policy(&self, harness: HarnessId) -> HarnessUpdatePolicy {
        lock(&self.inner.prefs)
            .policies
            .get(&harness)
            .copied()
            .unwrap_or_default()
    }

    fn status(&self, harness: HarnessId) -> HarnessUpdateStatus {
        lock(&self.inner.statuses)
            .get(&harness)
            .cloned()
            .unwrap_or_else(|| HarnessUpdateStatus {
                harness,
                installed_version: None,
                latest_version: None,
                channel: Some("stable".into()),
                source: HarnessInstallSource::Unknown,
                policy: self.policy(harness),
                phase: HarnessUpdatePhase::Dormant,
                progress: None,
                checked_at: None,
                error: None,
                can_apply: provider(harness).update_args.is_some(),
                manual_command: None,
            })
    }

    fn mutate(&self, harness: HarnessId, change: impl FnOnce(&mut HarnessUpdateStatus)) {
        let mut statuses = lock(&self.inner.statuses);
        let snapshot = {
            let policy = self.policy(harness);
            let status = statuses
                .entry(harness)
                .or_insert_with(|| HarnessUpdateStatus {
                    harness,
                    installed_version: None,
                    latest_version: None,
                    channel: Some("stable".into()),
                    source: HarnessInstallSource::Unknown,
                    policy,
                    phase: HarnessUpdatePhase::Dormant,
                    progress: None,
                    checked_at: None,
                    error: None,
                    can_apply: provider(harness).update_args.is_some(),
                    manual_command: None,
                });
            // Preferences are the durable authority. Refresh the copy while
            // holding the status lock so a check result and a policy change
            // have one deterministic order instead of resurrecting an
            // Available/Failed phase after Updates: Off.
            status.policy = policy;
            change(status);
            ordered_snapshot(&self.inner.order, &statuses)
        };
        // Keep publication in the same critical section as the state change.
        // Otherwise concurrent providers can publish full snapshots backwards.
        self.inner.status_tx.send_replace(snapshot);
    }

    fn executable(&self, harness: HarnessId) -> Result<PathBuf, String> {
        self.inner
            .registry
            .resolve(harness)
            .map_err(|error| error.to_string())?
            .executable_path()
            .ok_or_else(|| "agent CLI executable is unavailable".into())
    }

    fn operation_gate(&self, harness: HarnessId) -> Arc<tokio::sync::Mutex<()>> {
        lock(&self.inner.operation_gates)
            .entry(harness)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    fn settle_if_unmonitored(&self, harness: HarnessId) -> bool {
        let monitored = self.policy(harness) != HarnessUpdatePolicy::Off
            && self.inner.registry.enabled_set().contains(&harness);
        if monitored {
            return false;
        }
        self.mutate(harness, |status| {
            status.phase = HarnessUpdatePhase::Dormant;
            status.progress = None;
            status.error = None;
        });
        true
    }

    async fn npm_latest(&self, package: &str) -> Result<String, String> {
        self.npm_release(package, "latest").await
    }

    async fn npm_release(&self, package: &str, channel: &str) -> Result<String, String> {
        let encoded = package.replace('/', "%2f");
        let url = format!("{}/{encoded}/{channel}", self.inner.npm_registry_url);
        let response = self
            .inner
            .client
            .get(url)
            .send()
            .await
            .map_err(|error| format!("latest-version check failed: {error}"))?
            .error_for_status()
            .map_err(|error| format!("latest-version check failed: {error}"))?;
        let json: serde_json::Value = response
            .json()
            .await
            .map_err(|error| format!("latest-version response was invalid: {error}"))?;
        json.get("version")
            .and_then(serde_json::Value::as_str)
            .filter(|version| version_numbers(version).is_some())
            .map(str::to_owned)
            .ok_or_else(|| "latest-version response contained no version".into())
    }

    fn is_mutating(&self, harness: HarnessId) -> bool {
        lock(&self.inner.cancellations).contains_key(&harness)
    }

    fn fail(&self, harness: HarnessId, error: String) -> Result<(), String> {
        self.mutate(harness, |status| {
            status.phase = HarnessUpdatePhase::Failed;
            status.checked_at = Some(now_ms());
            status.error = Some(HarnessUpdateFailure {
                message: error.clone(),
                retryable: true,
            });
        });
        Err(error)
    }

    fn fail_check(&self, harness: HarnessId, error: String) -> Result<(), String> {
        let registry = self.inner.registry.clone();
        self.mutate(harness, |status| {
            if status.policy == HarnessUpdatePolicy::Off
                || !registry.enabled_set().contains(&harness)
            {
                status.phase = HarnessUpdatePhase::Dormant;
                status.error = None;
            } else {
                status.phase = HarnessUpdatePhase::Failed;
                status.checked_at = Some(now_ms());
                status.error = Some(HarnessUpdateFailure {
                    message: error.clone(),
                    retryable: true,
                });
            }
        });
        Err(error)
    }

    fn fail_check_with_installed(
        &self,
        harness: HarnessId,
        installed: String,
        source: HarnessInstallSource,
        error: String,
    ) -> Result<(), String> {
        self.mutate(harness, |status| {
            status.installed_version = Some(installed);
            status.source = source;
        });
        self.fail_check(harness, error)
    }

    fn finish_cancelled(&self, harness: HarnessId) {
        let previous_phase = lock(&self.inner.cancellations)
            .remove(&harness)
            .map(|update| update.previous_phase)
            .unwrap_or(HarnessUpdatePhase::ManualActionRequired);
        self.inner.registry.end_update(harness);
        let enabled = self.inner.registry.enabled_set().contains(&harness);
        self.mutate(harness, |status| {
            status.progress = None;
            status.phase = if !enabled || status.policy == HarnessUpdatePolicy::Off {
                HarnessUpdatePhase::Dormant
            } else {
                previous_phase
            };
            status.error = None;
        });
    }

    fn finish_failed_update(&self, harness: HarnessId, error: String) {
        lock(&self.inner.cancellations).remove(&harness);
        self.inner.registry.end_update(harness);
        self.fail(harness, error).ok();
    }

    fn persist_preferences(&self) {
        // Keep snapshot order and replacement order identical. Every writer
        // shares this lock and temporary path, including successful updates.
        let prefs = lock(&self.inner.prefs);
        let json = match serde_json::to_string_pretty(&*prefs) {
            Ok(json) => json,
            Err(error) => {
                tracing::warn!(%error, "harness update preferences serialize failed");
                return;
            }
        };
        let temp = self.inner.prefs_path.with_extension("json.tmp");
        if let Err(error) = std::fs::write(&temp, json)
            .and_then(|()| std::fs::rename(&temp, &self.inner.prefs_path))
        {
            tracing::warn!(%error, "harness update preferences save failed");
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn ordered_snapshot(
    order: &[HarnessId],
    statuses: &HashMap<HarnessId, HarnessUpdateStatus>,
) -> Vec<HarnessUpdateStatus> {
    order
        .iter()
        .filter_map(|id| statuses.get(id).cloned())
        .collect()
}

fn unpublished_homebrew_upgrade(command: &str) -> bool {
    command
        .trim()
        .trim_matches('`')
        .starts_with("brew upgrade ")
}

fn homebrew_token_ok(token: &str) -> bool {
    let Some(first) = token.chars().next() else {
        return false;
    };
    first.is_ascii_alphanumeric()
        && token.len() <= 128
        && token
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '_' | '.' | '@'))
        // brew loads a `.rb` or `.json` argument as a local package file.
        && !token.ends_with(".rb")
        && !token.ends_with(".json")
}

fn homebrew_url_component(token: &str) -> String {
    let mut encoded = String::new();
    for byte in token.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// `Caskroom/<token>/<version>/…` and `Cellar/<token>/<version>/{bin,libexec}/…`
/// are Homebrew's own layout. Anything else under a Homebrew prefix is not a
/// Homebrew-owned CLI: npm packages in `lib/node_modules`, including globals
/// that a keg-only runtime such as `node@20` keeps inside its own keg, belong
/// to that package manager, and upgrading the runtime would not update them.
fn homebrew_package(path: &Path) -> Option<HomebrewPackage> {
    // Homebrew runs only on macOS and Linux.
    if !cfg!(unix) {
        return None;
    }
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let components: Vec<_> = canonical.components().collect();
    let (index, cask) =
        components
            .iter()
            .enumerate()
            .find_map(|(index, component)| match component.as_os_str().to_str()? {
                "Caskroom" => Some((index, true)),
                "Cellar" => Some((index, false)),
                _ => None,
            })?;
    let token = components.get(index + 1)?.as_os_str().to_str()?;
    if !homebrew_token_ok(token) {
        return None;
    }
    let is_normal = |offset: usize| {
        matches!(
            components.get(index + offset),
            Some(std::path::Component::Normal(_))
        )
    };
    // The executable must live inside a versioned install of the package.
    if !is_normal(2) || !is_normal(3) {
        return None;
    }
    if !cask
        && !matches!(
            components[index + 3].as_os_str().to_str(),
            Some("bin" | "libexec")
        )
    {
        return None;
    }
    let mut prefix = PathBuf::new();
    for component in &components[..index] {
        prefix.push(component);
    }
    if prefix.as_os_str().is_empty() {
        return None;
    }
    Some(HomebrewPackage {
        brew: prefix.join("bin").join("brew"),
        cask,
        token: token.to_string(),
    })
}

fn resolve_brew(preferred: &Path) -> Result<PathBuf, String> {
    if preferred.is_file() {
        Ok(preferred.to_path_buf())
    } else {
        // A different prefix's brew would upgrade the wrong install.
        Err(format!("Homebrew was not found at {}", preferred.display()))
    }
}

fn parse_homebrew_version(cask: bool, body: &str) -> Result<String, String> {
    let json: serde_json::Value = serde_json::from_str(body)
        .map_err(|error| format!("Homebrew version response was invalid: {error}"))?;
    let version = if cask {
        json.get("version")
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                json.pointer("/casks/0/version")
                    .and_then(serde_json::Value::as_str)
            })
    } else {
        json.pointer("/versions/stable")
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                json.pointer("/formulae/0/versions/stable")
                    .and_then(serde_json::Value::as_str)
            })
    }
    .map(str::trim)
    .filter(|version| !version.is_empty())
    .ok_or_else(|| "Homebrew version response contained no version".to_string())?;
    let comparable = version.split(',').next().unwrap_or(version).trim();
    if version_numbers(comparable).is_none() {
        return Err(format!("Homebrew version {version} is not comparable"));
    }
    Ok(comparable.to_string())
}

fn classify_source(path: &Path) -> HarnessInstallSource {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let text = format!("{}\n{}", path.display(), canonical.display()).to_ascii_lowercase();
    if text.contains("/.cargo/bin/") {
        HarnessInstallSource::Cargo
    } else if text.contains("node_modules")
        || text.contains("/.nvm/")
        || text.contains("/.volta/")
        || text.contains("/.bun/")
        || text.contains("/pnpm/")
    {
        HarnessInstallSource::Npm
    } else if text.contains("homebrew") || text.contains("/cellar/") || text.contains("/caskroom/")
    {
        HarnessInstallSource::Homebrew
    } else {
        HarnessInstallSource::Vendor
    }
}

async fn run_version_command(
    _harness: HarnessId,
    executable: &Path,
    args: &[&str],
) -> Result<String, String> {
    let output = run_command_output(executable, args, COMMAND_TIMEOUT).await?;
    extract_version(&output).ok_or_else(|| "command returned no recognizable version".into())
}

async fn run_command(executable: &Path, args: &[&str], timeout: Duration) -> Result<(), String> {
    run_command_output(executable, args, timeout)
        .await
        .map(drop)
}

const BREW_UPGRADE_ENV: &[(&str, &str)] = &[
    ("NONINTERACTIVE", "1"),
    ("HOMEBREW_NO_ANALYTICS", "1"),
    ("HOMEBREW_NO_ENV_HINTS", "1"),
    ("HOMEBREW_NO_EMOJI", "1"),
];

const BREW_INFO_ENV: &[(&str, &str)] = &[
    ("NONINTERACTIVE", "1"),
    ("HOMEBREW_NO_ANALYTICS", "1"),
    ("HOMEBREW_NO_ENV_HINTS", "1"),
    ("HOMEBREW_NO_EMOJI", "1"),
    ("HOMEBREW_NO_AUTO_UPDATE", "1"),
];

/// Unix updaters can delegate installation to npm, pip, or a shell. Own
/// their process group as well as the leader, including on future cancellation.
#[cfg(unix)]
struct UpdateProcessGroup(libc::pid_t);

#[cfg(unix)]
impl Drop for UpdateProcessGroup {
    fn drop(&mut self) {
        // SAFETY: spawn creates a private group whose ID is this child's PID.
        unsafe { libc::kill(-self.0, libc::SIGKILL) };
    }
}

async fn run_command_output(
    executable: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<String, String> {
    run_command_output_env(executable, args, timeout, &[]).await
}

async fn run_command_output_env(
    executable: &Path,
    args: &[&str],
    timeout: Duration,
    env: &[(&str, &str)],
) -> Result<String, String> {
    let mut command = Command::new(executable);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env("NO_COLOR", "1");
    for (key, value) in env {
        command.env(*key, *value);
    }
    paku_harness::compose_child_path(&mut command, executable);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not run {}: {error}", executable.display()))?;
    #[cfg(unix)]
    let group =
        UpdateProcessGroup(child.id().expect("newly spawned updater has a PID") as libc::pid_t);
    let mut stdout = child.stdout.take().expect("updater stdout is piped");
    let mut stderr = child.stderr.take().expect("updater stderr is piped");
    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();
    let result = {
        use tokio::io::AsyncReadExt as _;
        tokio::time::timeout(timeout, async {
            tokio::try_join!(
                child.wait(),
                stdout.read_to_end(&mut stdout_bytes),
                stderr.read_to_end(&mut stderr_bytes),
            )
        })
        .await
    };
    // Stop descendants before returning and releasing the execution lease,
    // including when the leader exited but a descendant kept its pipes open.
    #[cfg(unix)]
    drop(group);
    let status = match result {
        Ok(Ok((status, _, _))) => status,
        error => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(match error {
                Err(_) => format!("{} timed out", executable.display()),
                Ok(Err(error)) => format!("could not run {}: {error}", executable.display()),
                Ok(Ok(_)) => unreachable!(),
            });
        }
    };
    let output = std::process::Output {
        status,
        stdout: stdout_bytes,
        stderr: stderr_bytes,
    };
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if !output.status.success() {
        let detail = if stderr.is_empty() { stdout } else { stderr };
        return Err(format!(
            "{} exited with {}{}",
            executable.display(),
            output.status,
            (!detail.is_empty())
                .then(|| format!(": {detail}"))
                .unwrap_or_default()
        ));
    }
    Ok(if stdout.is_empty() { stderr } else { stdout })
}

fn version_tokens(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|character: char| {
        !(character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '+'))
    })
    .filter(|candidate| version_numbers(candidate).is_some())
    .map(|candidate| candidate.trim_start_matches('v').to_owned())
}

fn extract_version(text: &str) -> Option<String> {
    version_tokens(text).next()
}

fn version_numbers(version: &str) -> Option<Vec<u64>> {
    let version = version.trim().trim_start_matches('v');
    let numeric = version.split(['-', '+']).next()?;
    let values: Vec<u64> = numeric
        .split('.')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    (values.len() >= 2).then_some(values)
}

fn version_is_newer(latest: &str, installed: &str) -> bool {
    match (version_numbers(latest), version_numbers(installed)) {
        (Some(mut latest), Some(mut installed)) => {
            let width = latest.len().max(installed.len());
            latest.resize(width, 0);
            installed.resize(width, 0);
            latest > installed
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{LatestSource, extract_version, provider, version_is_newer};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    use async_trait::async_trait;
    use futures::StreamExt as _;
    use paku_harness::{Harness, HarnessError, RunControls};
    use paku_proto::{
        AgentEvent, HarnessId, HarnessUpdatePhase, Model, ReasoningLevel, RunRequest, SteeringMode,
    };

    use crate::registry::HarnessRegistry;

    struct ExecutableHarness(PathBuf, HarnessId);

    #[cfg(unix)]
    #[tokio::test]
    async fn updater_timeout_stops_descendants_before_returning() {
        for leader_exits in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let marker = temp.path().join("mutated");
            let ready = temp.path().join("ready");
            let script = format!(
                "echo $$ > \"$2\"; /bin/sh -c 'sleep 0.5; echo mutated > \"$1\"' sh \"$1\" & {}",
                if leader_exits { "exit 0" } else { "wait" }
            );
            let result = super::run_command_output(
                std::path::Path::new("/bin/sh"),
                &[
                    "-c",
                    &script,
                    "sh",
                    marker.to_str().unwrap(),
                    ready.to_str().unwrap(),
                ],
                Duration::from_millis(250),
            )
            .await;
            assert!(result.unwrap_err().contains("timed out"));
            let pid: i32 = std::fs::read_to_string(&ready)
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            // The direct child must already be reaped, not merely signalled.
            assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
            tokio::time::sleep(Duration::from_millis(500)).await;
            assert!(
                !marker.exists(),
                "installer descendant outlived its execution lease"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dropping_update_probe_stops_descendants() {
        let temp = tempfile::tempdir().unwrap();
        let marker = temp.path().join("mutated");
        let ready = temp.path().join("ready");
        let args = [
            "-c",
            "/bin/sh -c 'echo ready > \"$2\"; sleep 0.5; echo mutated > \"$1\"' sh \"$1\" \"$2\" & wait",
            "sh",
            marker.to_str().unwrap(),
            ready.to_str().unwrap(),
        ];
        let mut probe = Box::pin(super::run_command_output(
            std::path::Path::new("/bin/sh"),
            &args,
            Duration::from_secs(10),
        ));
        tokio::select! {
            result = &mut probe => panic!("probe ended early: {result:?}"),
            _ = async {
                tokio::time::timeout(Duration::from_secs(5), async {
                    while !ready.exists() {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                }).await.unwrap();
            } => {}
        }
        drop(probe);
        tokio::time::sleep(Duration::from_millis(700)).await;
        assert!(
            !marker.exists(),
            "cancelled probe left an installer running"
        );
    }

    #[async_trait]
    impl Harness for ExecutableHarness {
        fn id(&self) -> HarnessId {
            self.1
        }

        fn display_name(&self) -> &str {
            "Fixture"
        }

        fn supports_steering(&self) -> bool {
            false
        }

        fn steering_mode(&self) -> SteeringMode {
            SteeringMode::TurnBoundary
        }

        fn reasoning_levels(&self) -> &[ReasoningLevel] {
            &[]
        }

        fn executable_path(&self) -> Option<PathBuf> {
            Some(self.0.clone())
        }

        fn installed(&self) -> bool {
            !self.0.with_extension("unavailable").exists()
        }

        async fn models(&self) -> Result<Vec<Model>, HarnessError> {
            Ok(Vec::new())
        }

        async fn run(
            &self,
            _request: RunRequest,
            _controls: RunControls,
        ) -> Result<
            futures::stream::BoxStream<'static, Result<AgentEvent, HarnessError>>,
            HarnessError,
        > {
            Ok(futures::stream::empty().boxed())
        }
    }

    #[test]
    fn extracts_versions_from_vendor_output() {
        assert_eq!(extract_version("pi 1.0.4"), Some("1.0.4".into()));
        assert_eq!(
            extract_version("latest: v1.4.0\ninstalled: 1.3.2"),
            Some("1.4.0".into())
        );
        assert_eq!(extract_version("no release here"), None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shutdown_does_not_wait_for_a_slow_periodic_check() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("agent");
        std::fs::write(&executable, "#!/bin/sh\nexec sleep 30\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        let registry = Arc::new(HarnessRegistry::new());
        registry.register(Arc::new(ExecutableHarness(executable, HarnessId::Pi)));
        let coordinator = super::HarnessUpdateCoordinator::new(temp.path(), registry);
        let mut watch = coordinator.watch();
        coordinator.start();
        // The startup check is now blocked in the CLI's version probe.
        tokio::time::timeout(Duration::from_secs(5), async {
            while coordinator.status(HarnessId::Pi).phase != HarnessUpdatePhase::Checking
                || !super::lock(&coordinator.inner.operation_gates).contains_key(&HarnessId::Pi)
            {
                watch.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(2), coordinator.shutdown())
            .await
            .expect("shutdown waited for a probe that can take the full command timeout");
    }

    #[cfg(unix)]
    async fn automatic_fixture() -> (tempfile::TempDir, super::HarnessUpdateCoordinator) {
        use std::os::unix::fs::PermissionsExt;
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("agent");
        std::fs::write(temp.path().join("version"), "1.0.0\n").unwrap();
        std::fs::write(
            &executable,
            r#"#!/bin/sh
root="$(dirname "$0")"
case "$1:$2" in
  --version:) cat "$root/version" ;;
  update:--self) printf '2.0.0\n' > "$root/version" ;;
  *) exit 2 ;;
esac
"#,
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        let registry = Arc::new(HarnessRegistry::new());
        registry.register(Arc::new(ExecutableHarness(executable, HarnessId::Pi)));
        let mut coordinator = super::HarnessUpdateCoordinator::new(temp.path(), registry);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let inner = Arc::get_mut(&mut coordinator.inner).unwrap();
        inner.npm_registry_url = url;
        inner.client = reqwest::Client::builder()
            .no_proxy()
            .timeout(super::COMMAND_TIMEOUT)
            .build()
            .unwrap();
        let fail_check = temp.path().join("fail-check");
        let shutdown = coordinator.inner.shutdown.clone();
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = tokio::select! {
                    _ = shutdown.cancelled() => break,
                    connection = listener.accept() => connection.unwrap(),
                };
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    request.push(socket.read_u8().await.unwrap());
                }
                assert!(
                    String::from_utf8_lossy(&request)
                        .starts_with("GET /@earendil-works%2fpi-coding-agent/latest ")
                );
                let status = if fail_check.exists() {
                    "503 Unavailable"
                } else {
                    "200 OK"
                };
                let body = r#"{"version":"2.0.0"}"#;
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });
        (temp, coordinator)
    }

    #[test]
    fn pi_checks_the_active_package_scope() {
        let spec = provider(HarnessId::Pi);
        assert!(matches!(
            spec.latest,
            LatestSource::Npm("@earendil-works/pi-coding-agent")
        ));
        assert_eq!(spec.version_args, &["--version"]);
        assert_eq!(spec.update_args, Some(&["update", "--self"][..]));
        assert_eq!(spec.manual_command, "pi update --self");
    }

    #[tokio::test]
    async fn mock_updates_are_rejected_before_any_probe_or_mutation() {
        use paku_proto::HarnessUpdatePolicy;
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("missing-agent");
        let registry = Arc::new(HarnessRegistry::new());
        registry.register(Arc::new(ExecutableHarness(
            executable.clone(),
            HarnessId::Mock,
        )));
        let coordinator = super::HarnessUpdateCoordinator::new(temp.path(), registry.clone());
        assert!(coordinator.snapshot().is_empty());
        assert!(!coordinator.status(HarnessId::Mock).can_apply);
        assert!(coordinator.check_all().await.is_empty());
        assert_eq!(
            coordinator.check_one(HarnessId::Mock).await.unwrap_err(),
            "updates are only supported for Pi"
        );
        assert_eq!(
            coordinator.apply(HarnessId::Mock).await.unwrap_err(),
            "updates are only supported for Pi"
        );
        // Even a stale or injected applicable status must not authorize Mock.
        super::lock(&coordinator.inner.prefs)
            .policies
            .insert(HarnessId::Mock, HarnessUpdatePolicy::AutoWhenIdle);
        coordinator.mutate(HarnessId::Mock, |status| {
            status.phase = HarnessUpdatePhase::Available;
            status.can_apply = true;
        });
        assert!(!coordinator.automatic_update_ready(HarnessId::Mock));
        assert!(
            coordinator
                .apply_locked(HarnessId::Mock, true)
                .await
                .is_err()
        );
        assert!(!registry.update_pending(HarnessId::Mock));
        assert!(super::lock(&coordinator.inner.operation_gates).is_empty());
        assert!(super::lock(&coordinator.inner.cancellations).is_empty());
        for path in [
            executable.as_path(),
            std::path::Path::new("/opt/homebrew/Cellar/pi-coding-agent/1.2.3/bin/pi"),
        ] {
            assert!(super::update_plan(HarnessId::Mock, path).is_err());
            assert!(!super::can_apply_update(HarnessId::Mock, path));
        }
        coordinator.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pi_discovery_dismissal_and_explicit_self_update() {
        let (temp, coordinator) = automatic_fixture().await;
        coordinator.check_one(HarnessId::Pi).await.unwrap();
        let status = coordinator.status(HarnessId::Pi);
        assert_eq!(status.phase, HarnessUpdatePhase::Available);
        assert_eq!(status.installed_version.as_deref(), Some("1.0.0"));
        assert_eq!(status.latest_version.as_deref(), Some("2.0.0"));
        assert!(status.can_apply);
        assert_eq!(
            coordinator.dismiss(HarnessId::Pi, None).phase,
            HarnessUpdatePhase::Current
        );
        coordinator.check_one(HarnessId::Pi).await.unwrap();
        assert_eq!(
            coordinator.status(HarnessId::Pi).phase,
            HarnessUpdatePhase::Current
        );
        super::lock(&coordinator.inner.prefs)
            .dismissed_versions
            .remove(&HarnessId::Pi);
        coordinator.check_one(HarnessId::Pi).await.unwrap();
        assert_eq!(coordinator.apply(HarnessId::Pi).await.unwrap(), "2.0.0");
        assert_eq!(
            std::fs::read_to_string(temp.path().join("version")).unwrap(),
            "2.0.0\n"
        );
        assert!(!coordinator.inner.registry.update_pending(HarnessId::Pi));
        coordinator.check_one(HarnessId::Pi).await.unwrap();
        assert_eq!(
            coordinator.status(HarnessId::Pi).phase,
            HarnessUpdatePhase::Current
        );
        coordinator.shutdown().await;
    }

    #[test]
    fn concurrent_publications_never_regress_the_watched_state() {
        let temp = tempfile::tempdir().unwrap();
        let registry = Arc::new(HarnessRegistry::new());
        registry.register(Arc::new(ExecutableHarness(
            temp.path().join("agent"),
            HarnessId::Pi,
        )));
        let coordinator = super::HarnessUpdateCoordinator::new(temp.path(), registry);
        let mut watch = coordinator.watch();
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        std::thread::scope(|scope| {
            let reader = scope.spawn(|| {
                let mut previous = 0;
                while !done.load(std::sync::atomic::Ordering::Acquire) {
                    let current = watch.borrow_and_update()[0].checked_at.unwrap_or(0);
                    assert!(
                        current >= previous,
                        "published state regressed: {previous} → {current}"
                    );
                    previous = current;
                    std::thread::yield_now();
                }
            });
            let writers: Vec<_> = (0..4)
                .map(|_| {
                    scope.spawn(|| {
                        for _ in 0..1000 {
                            coordinator.mutate(HarnessId::Pi, |status| {
                                status.checked_at = Some(status.checked_at.unwrap_or(0) + 1);
                            });
                        }
                    })
                })
                .collect();
            for writer in writers {
                writer.join().unwrap();
            }
            done.store(true, std::sync::atomic::Ordering::Release);
            reader.join().unwrap();
        });
        assert_eq!(coordinator.snapshot()[0].checked_at, Some(4000));
        assert_eq!(*watch.borrow(), coordinator.snapshot());
    }

    async fn wait_for_phase(
        coordinator: &super::HarnessUpdateCoordinator,
        harness: HarnessId,
        phase: HarnessUpdatePhase,
    ) {
        let mut watch = coordinator.watch();
        tokio::time::timeout(Duration::from_secs(5), async {
            while coordinator.status(harness).phase != phase {
                watch.changed().await.unwrap();
            }
        })
        .await
        .unwrap_or_else(|_| panic!("expected {phase:?}, got {:?}", coordinator.status(harness)));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn notify_cancels_waiting_automatic_but_preserves_explicit_updates() {
        use paku_proto::HarnessUpdatePolicy;
        for automatic in [true, false] {
            let (temp, coordinator) = automatic_fixture().await;
            coordinator.check_one(HarnessId::Pi).await.unwrap();
            let running = coordinator
                .inner
                .registry
                .execution_lease(HarnessId::Pi)
                .await;
            let explicit = if automatic {
                coordinator.set_policy(HarnessId::Pi, HarnessUpdatePolicy::AutoWhenIdle);
                None
            } else {
                // Start explicit work under Auto without scheduling competing work.
                super::lock(&coordinator.inner.prefs)
                    .policies
                    .insert(HarnessId::Pi, HarnessUpdatePolicy::AutoWhenIdle);
                coordinator.mutate(HarnessId::Pi, |status| {
                    status.policy = HarnessUpdatePolicy::AutoWhenIdle
                });
                let update = coordinator.clone();
                Some(tokio::spawn(
                    async move { update.apply(HarnessId::Pi).await },
                ))
            };
            wait_for_phase(
                &coordinator,
                HarnessId::Pi,
                HarnessUpdatePhase::WaitingForIdle,
            )
            .await;
            coordinator.set_policy(HarnessId::Pi, HarnessUpdatePolicy::Notify);
            if automatic {
                tokio::time::timeout(Duration::from_secs(2), async {
                    while coordinator.inner.registry.update_pending(HarnessId::Pi) {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("Notify cancels without waiting for the active run");
                wait_for_phase(&coordinator, HarnessId::Pi, HarnessUpdatePhase::Available).await;
            } else {
                assert!(
                    !super::lock(&coordinator.inner.cancellations)[&HarnessId::Pi]
                        .cancel
                        .is_cancelled()
                );
            }
            drop(running);
            if let Some(explicit) = explicit {
                explicit.await.unwrap().unwrap();
            }
            {
                // Wait for all work using the gate before observing the version.
                let _operation = coordinator.operation_gate(HarnessId::Pi).lock_owned().await;
                assert_eq!(
                    std::fs::read_to_string(temp.path().join("version")).unwrap(),
                    if automatic { "1.0.0\n" } else { "2.0.0\n" }
                );
            }
            coordinator.shutdown().await;
        }
    }

    #[tokio::test]
    async fn automatic_install_boundary_rechecks_policy() {
        use paku_proto::HarnessUpdatePolicy;
        let temp = tempfile::tempdir().unwrap();
        let registry = Arc::new(HarnessRegistry::new());
        registry.register(Arc::new(ExecutableHarness(
            temp.path().join("agent"),
            HarnessId::Pi,
        )));
        let coordinator = super::HarnessUpdateCoordinator::new(temp.path(), registry);
        let cancel = tokio_util::sync::CancellationToken::new();
        super::lock(&coordinator.inner.cancellations).insert(
            HarnessId::Pi,
            super::ActiveUpdate {
                cancel: cancel.clone(),
                automatic: true,
                previous_phase: HarnessUpdatePhase::Available,
            },
        );
        coordinator.mutate(HarnessId::Pi, |status| {
            status.phase = HarnessUpdatePhase::Preparing
        });
        super::lock(&coordinator.inner.prefs)
            .policies
            .insert(HarnessId::Pi, HarnessUpdatePolicy::Notify);
        assert!(coordinator.begin_install(HarnessId::Pi, &cancel).is_err());
        assert!(cancel.is_cancelled());
        assert_eq!(
            coordinator.status(HarnessId::Pi).phase,
            HarnessUpdatePhase::Preparing
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn losing_the_only_agent_cancels_its_waiting_automatic_update() {
        use paku_proto::HarnessUpdatePolicy;
        let (temp, coordinator) = automatic_fixture().await;
        let registry = &coordinator.inner.registry;
        let running = registry.execution_lease(HarnessId::Pi).await;
        coordinator.mutate(HarnessId::Pi, |status| {
            status.phase = HarnessUpdatePhase::Available;
            status.can_apply = true;
            status.latest_version = Some("2.0.0".into());
        });
        coordinator.set_policy(HarnessId::Pi, HarnessUpdatePolicy::AutoWhenIdle);
        wait_for_phase(
            &coordinator,
            HarnessId::Pi,
            HarnessUpdatePhase::WaitingForIdle,
        )
        .await;
        // Pi is the only runnable provider: simulate it becoming unavailable,
        // which changes enablement without disabling the last installed CLI.
        std::fs::write(temp.path().join("agent.unavailable"), "").unwrap();
        coordinator.refresh_enabled();
        tokio::time::timeout(Duration::from_secs(1), async {
            while registry.update_pending(HarnessId::Pi) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("disable cancels the idle wait before the active run finishes");
        drop(running);
        coordinator.shutdown().await;
        assert_eq!(
            std::fs::read_to_string(temp.path().join("version")).unwrap(),
            "1.0.0\n"
        );
    }

    #[test]
    fn concurrent_preferences_persist_the_latest_complete_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let coordinator =
            super::HarnessUpdateCoordinator::new(temp.path(), Arc::new(HarnessRegistry::new()));
        std::thread::scope(|scope| {
            for worker in 0..8 {
                let coordinator = &coordinator;
                scope.spawn(move || {
                    for revision in 0..100 {
                        super::lock(&coordinator.inner.prefs)
                            .dismissed_versions
                            .insert(HarnessId::Pi, format!("{worker}.{revision}.0"));
                        coordinator.persist_preferences();
                        let bytes = std::fs::read(&coordinator.inner.prefs_path).unwrap();
                        serde_json::from_slice::<super::Preferences>(&bytes)
                            .expect("atomic complete JSON");
                    }
                });
            }
        });
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&coordinator.inner.prefs_path).unwrap()).unwrap();
        assert_eq!(
            saved,
            serde_json::to_value(&*super::lock(&coordinator.inner.prefs)).unwrap()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn enabling_auto_updates_installs_release_discovered_by_its_check() {
        use paku_proto::HarnessUpdatePolicy;
        for phase in [
            HarnessUpdatePhase::Dormant,
            HarnessUpdatePhase::Checking,
            HarnessUpdatePhase::Current,
        ] {
            let (temp, coordinator) = automatic_fixture().await;
            coordinator.mutate(HarnessId::Pi, |status| status.phase = phase);
            coordinator.set_policy(HarnessId::Pi, HarnessUpdatePolicy::AutoWhenIdle);
            wait_for_phase(&coordinator, HarnessId::Pi, HarnessUpdatePhase::Updated).await;
            assert_eq!(
                std::fs::read_to_string(temp.path().join("version")).unwrap(),
                "2.0.0\n"
            );
            coordinator.shutdown().await;
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn retrying_one_failed_check_schedules_automatic_installation() {
        use paku_proto::HarnessUpdatePolicy;
        let (temp, coordinator) = automatic_fixture().await;
        std::fs::write(temp.path().join("fail-check"), "").unwrap();
        coordinator.set_policy(HarnessId::Pi, HarnessUpdatePolicy::AutoWhenIdle);
        wait_for_phase(&coordinator, HarnessId::Pi, HarnessUpdatePhase::Failed).await;
        std::fs::remove_file(temp.path().join("fail-check")).unwrap();
        coordinator.check_one(HarnessId::Pi).await.unwrap();
        wait_for_phase(&coordinator, HarnessId::Pi, HarnessUpdatePhase::Updated).await;
        assert_eq!(
            std::fs::read_to_string(temp.path().join("version")).unwrap(),
            "2.0.0\n"
        );
        coordinator.shutdown().await;
    }

    #[test]
    fn compares_different_width_versions() {
        assert!(version_is_newer("1.2.1", "1.2"));
        assert!(!version_is_newer("1.2.0", "1.2"));
        assert!(!version_is_newer("1.1.9", "1.2.0"));
    }

    #[tokio::test]
    async fn accepted_cancellation_and_installation_are_mutually_exclusive() {
        let temp = tempfile::tempdir().unwrap();
        let registry = Arc::new(HarnessRegistry::new());
        let harness = HarnessId::Pi;
        registry.register(Arc::new(ExecutableHarness(
            temp.path().join("agent"),
            harness,
        )));
        let coordinator = super::HarnessUpdateCoordinator::new(temp.path(), registry);

        // Exercise both orderings and simultaneous contenders at Pi's
        // command installation boundary.
        for phase in [HarnessUpdatePhase::Preparing] {
            for ordering in 0..66 {
                let token = tokio_util::sync::CancellationToken::new();
                super::lock(&coordinator.inner.cancellations).insert(
                    harness,
                    super::ActiveUpdate {
                        cancel: token.clone(),
                        automatic: false,
                        previous_phase: HarnessUpdatePhase::Available,
                    },
                );
                coordinator.mutate(harness, |status| status.phase = phase);
                let barrier = std::sync::Barrier::new(2);
                let (cancelled, installed) = match ordering {
                    0 => {
                        let cancelled = coordinator.cancel(harness);
                        (
                            cancelled,
                            coordinator.begin_install(harness, &token).is_ok(),
                        )
                    }
                    1 => {
                        let installed = coordinator.begin_install(harness, &token).is_ok();
                        (coordinator.cancel(harness), installed)
                    }
                    _ => std::thread::scope(|scope| {
                        let barrier = &barrier;
                        let coordinator = &coordinator;
                        let cancellation = scope.spawn(move || {
                            barrier.wait();
                            coordinator.cancel(harness)
                        });
                        barrier.wait();
                        let installed = coordinator.begin_install(harness, &token).is_ok();
                        (cancellation.join().unwrap(), installed)
                    }),
                };
                assert_ne!(cancelled, installed);
                assert_eq!(token.is_cancelled(), cancelled);
                assert_eq!(
                    coordinator.status(harness).phase,
                    if installed {
                        HarnessUpdatePhase::Installing
                    } else {
                        phase
                    }
                );
            }
        }
    }

    #[tokio::test]
    async fn a_check_refresh_does_not_overwrite_an_installing_phase() {
        let temp = tempfile::tempdir().unwrap();
        let registry = Arc::new(HarnessRegistry::new());
        registry.register(Arc::new(ExecutableHarness(
            temp.path().join("agent"),
            HarnessId::Pi,
        )));
        let coordinator = super::HarnessUpdateCoordinator::new(temp.path(), registry);
        super::lock(&coordinator.inner.cancellations).insert(
            HarnessId::Pi,
            super::ActiveUpdate {
                cancel: tokio_util::sync::CancellationToken::new(),
                automatic: false,
                previous_phase: HarnessUpdatePhase::Available,
            },
        );
        coordinator.mutate(HarnessId::Pi, |status| {
            status.phase = HarnessUpdatePhase::Installing
        });
        coordinator.check_one(HarnessId::Pi).await.unwrap();
        assert_eq!(
            coordinator.status(HarnessId::Pi).phase,
            HarnessUpdatePhase::Installing
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelling_a_busy_host_does_not_touch_another_device_or_its_installation() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let registry = Arc::new(HarnessRegistry::new());
        registry.register(Arc::new(ExecutableHarness(
            first.path().join("agent"),
            HarnessId::Pi,
        )));
        let other_registry = Arc::new(HarnessRegistry::new());
        other_registry.register(Arc::new(ExecutableHarness(
            second.path().join("agent"),
            HarnessId::Pi,
        )));
        let host = super::HarnessUpdateCoordinator::new(first.path(), registry.clone());
        let other = super::HarnessUpdateCoordinator::new(second.path(), other_registry);
        for coordinator in [&host, &other] {
            coordinator.mutate(HarnessId::Pi, |status| {
                status.phase = HarnessUpdatePhase::Available;
                status.installed_version = Some("1.0.0".into());
                status.latest_version = Some("2.0.0".into());
            });
        }
        let run = registry.execution_lease(HarnessId::Pi).await;
        let applying = tokio::spawn({
            let host = host.clone();
            async move { host.apply(HarnessId::Pi).await }
        });
        wait_for_phase(&host, HarnessId::Pi, HarnessUpdatePhase::WaitingForIdle).await;
        assert_eq!(
            other.status(HarnessId::Pi).phase,
            HarnessUpdatePhase::Available
        );
        assert!(host.cancel(HarnessId::Pi));
        let result = tokio::time::timeout(Duration::from_secs(2), applying)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.unwrap_err(), "update cancelled");
        assert!(!registry.update_pending(HarnessId::Pi));
        assert_eq!(
            other.status(HarnessId::Pi).installed_version.as_deref(),
            Some("1.0.0")
        );
        drop(run);
        // A host restart probes again rather than replaying an old update.
        let restarted = super::HarnessUpdateCoordinator::new(first.path(), registry);
        assert_eq!(
            restarted.status(HarnessId::Pi).phase,
            HarnessUpdatePhase::Checking
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn installing_survives_the_request_task_being_dropped_and_verifies() {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("fixture-agent");
        std::fs::write(temp.path().join("version"), "1.0.0\n").unwrap();
        std::fs::write(
            &executable,
            r#"#!/bin/sh
version_file="$(dirname "$0")/version"
case "$1:$2" in
  --version:) cat "$version_file" ;;
  update:--self) sleep 0.2; printf '2.0.0\n' > "$version_file" ;;
  *) exit 2 ;;
esac
"#,
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&executable, permissions).unwrap();

        let registry = Arc::new(HarnessRegistry::new());
        registry.register(Arc::new(ExecutableHarness(executable, HarnessId::Pi)));
        let coordinator = super::HarnessUpdateCoordinator::new(temp.path(), registry.clone());
        coordinator.mutate(HarnessId::Pi, |status| {
            status.installed_version = Some("1.0.0".into());
            status.latest_version = Some("2.0.0".into());
            status.phase = HarnessUpdatePhase::Available;
        });

        let apply = tokio::spawn({
            let coordinator = coordinator.clone();
            async move { coordinator.apply(HarnessId::Pi).await }
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while coordinator.status(HarnessId::Pi).phase != HarnessUpdatePhase::Installing {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        apply.abort();

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let status = coordinator.status(HarnessId::Pi);
                if matches!(
                    status.phase,
                    HarnessUpdatePhase::Updated | HarnessUpdatePhase::Current
                ) {
                    assert_eq!(status.installed_version.as_deref(), Some("2.0.0"));
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!registry.update_pending(HarnessId::Pi));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shutdown_cancels_apply_queued_behind_provider_check() {
        let (temp, coordinator) = automatic_fixture().await;
        coordinator.check_one(HarnessId::Pi).await.unwrap();
        let checking = coordinator.operation_gate(HarnessId::Pi).lock_owned().await;
        let applying = tokio::spawn({
            let coordinator = coordinator.clone();
            async move { coordinator.apply(HarnessId::Pi).await }
        });
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        coordinator.shutdown().await;
        // Queued work must settle without needing the old checker to finish.
        let result = tokio::time::timeout(Duration::from_secs(1), applying)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.unwrap_err(), "update cancelled");
        drop(checking);
        assert_eq!(
            std::fs::read_to_string(temp.path().join("version")).unwrap(),
            "1.0.0\n"
        );
        assert!(!coordinator.inner.registry.update_pending(HarnessId::Pi));
        assert_eq!(
            coordinator.apply(HarnessId::Pi).await.unwrap_err(),
            "update cancelled"
        );
    }

    #[cfg(unix)]
    #[test]
    fn homebrew_pi_formula_update_and_version_parsing() {
        let formula = super::homebrew_package(std::path::Path::new(
            "/home/linuxbrew/.linuxbrew/Cellar/pi-coding-agent/1.2.3/bin/pi",
        ))
        .unwrap();
        assert!(!formula.cask);
        assert_eq!(formula.token, "pi-coding-agent");
        assert_eq!(
            formula.upgrade_command(),
            "brew upgrade --formula pi-coding-agent"
        );
        assert_eq!(
            formula.upgrade_args(),
            ["upgrade", "--formula", "pi-coding-agent"]
        );
        assert_eq!(
            formula.brew,
            std::path::PathBuf::from("/home/linuxbrew/.linuxbrew/bin/brew")
        );

        assert!(super::can_apply_update(
            HarnessId::Pi,
            std::path::Path::new("/home/linuxbrew/.linuxbrew/Cellar/pi-coding-agent/1.2.3/bin/pi")
        ));

        assert_eq!(
            super::parse_homebrew_version(true, r#"{"version":"0.159.1"}"#).unwrap(),
            "0.159.1"
        );
        assert_eq!(
            super::parse_homebrew_version(true, r#"{"casks":[{"version":"0.159.1"}]}"#).unwrap(),
            "0.159.1"
        );
        assert_eq!(
            super::parse_homebrew_version(false, r#"{"versions":{"stable":"1.2.3"}}"#).unwrap(),
            "1.2.3"
        );
        assert_eq!(
            super::parse_homebrew_version(
                false,
                r#"{"formulae":[{"versions":{"stable":"1.2.3"}}]}"#
            )
            .unwrap(),
            "1.2.3"
        );
        assert!(super::parse_homebrew_version(true, r#"{"version":"latest"}"#).is_err());
        assert_eq!(super::homebrew_url_component("node@20"), "node%4020");
    }
}
