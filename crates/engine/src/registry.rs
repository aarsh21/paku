//! HarnessRegistry — the engine's harness catalog: eager injected instances plus
//! lazy slots resolved on first use. Production registers only native Pi RPC.
//! Lazy slots carry a static descriptor so `ListHarnesses` never forces a spawn.
//!
//! Also owns the device's harness ENABLEMENT (Settings → Providers): which harnesses
//! this device's composer offers, persisted in `{data_dir}/harness-prefs.json`.
//! Per-device because CLI installs are — a viewer retargets the settings page at
//! another device and edits THAT device's set over the forwarded RPCs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};

use paku_harness::{Harness, HarnessError};
use paku_proto::{HarnessId, ReasoningLevel, SteeringMode};

/// What `ListHarnesses` reports per harness.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessDescriptor {
    pub id: HarnessId,
    pub name: String,
    pub supports_steering: bool,
    pub steering_mode: SteeringMode,
    pub reasoning_levels: Vec<ReasoningLevel>,
    /// Whether the agent's CLI is present on the listing device (the settings
    /// enable-gate). Defaults true so catalogs from engines predating the
    /// field never read as uninstallable.
    #[serde(default = "default_installed")]
    pub installed: bool,
    /// Explicit CLI installation is available on this listing device.
    #[serde(default)]
    pub can_install: bool,
    /// Whether the listing device offers this harness (Settings → Providers).
    /// `None` — the catalog came from an engine predating the setting — means
    /// "unknown": consumers fall back to detection (see [`descriptor_enabled`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

impl HarnessDescriptor {
    /// Whether this harness can accept a prompt inside the turn that is
    /// currently running. Turn-boundary steering is still useful to the
    /// automatic queue drain, but it is not the non-interrupting "Steer"
    /// action exposed on an individual queued row.
    pub fn steers_mid_turn(&self) -> bool {
        self.supports_steering && self.steering_mode == SteeringMode::StepBoundary
    }
}

fn default_installed() -> bool {
    true
}

/// Mock remains injectable for tests but must never count as a runnable CLI
/// when protecting the last enabled production harness.
fn auto_enabled(id: HarnessId) -> bool {
    id == HarnessId::Pi
}

/// A descriptor's effective enabled flag. `None` — a catalog from an engine
/// predating the setting — falls back to detection, the same rule new devices
/// start from (see [`HarnessRegistry::enabled_set`]).
pub fn descriptor_enabled(descriptor: &HarnessDescriptor) -> bool {
    descriptor
        .enabled
        .unwrap_or_else(|| descriptor.installed && auto_enabled(descriptor.id))
}

fn describe(harness: &dyn Harness) -> HarnessDescriptor {
    HarnessDescriptor {
        id: harness.id(),
        name: harness.display_name().to_string(),
        supports_steering: harness.supports_steering(),
        steering_mode: harness.steering_mode(),
        reasoning_levels: harness.reasoning_levels().to_vec(),
        installed: harness.installed(),
        can_install: false,
        enabled: None,
    }
}

/// The persisted shape of `harness-prefs.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct HarnessPrefsFile {
    /// The user's explicit opt-OUTS. Enablement otherwise follows detection,
    /// so the file only records "no" — an agent installed later turns itself
    /// on without a trip to Settings.
    disabled: Vec<HarnessId>,
    titles: TitleSettings,
    /// The allow-list written back when enablement was a fixed default set.
    /// Read once, folded into `disabled`, and never written again.
    #[serde(skip_serializing)]
    enabled: Option<Vec<HarnessId>>,
}

/// Per-device automatic session title preferences.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TitleSettings {
    /// None follows the session harness, using a supported installed fallback.
    pub harness: Option<HarnessId>,
    /// None selects the cheapest model offered by the selected harness.
    pub model: Option<String>,
}

type Factory = Box<dyn Fn() -> Result<Arc<dyn Harness>, HarnessError> + Send + Sync>;
type InstalledProbe = Box<dyn Fn() -> bool + Send + Sync>;

enum Slot {
    Ready(Arc<dyn Harness>),
    Lazy {
        descriptor: HarnessDescriptor,
        /// Re-run on every `descriptors()` call — a CLI installed mid-session
        /// shows up on the next settings/picker open, no restart needed.
        installed: InstalledProbe,
        factory: Factory,
    },
}

pub struct HarnessRegistry {
    pub(crate) installs: crate::rpc::Installations,
    slots: Mutex<HashMap<HarnessId, Slot>>,
    order: Mutex<Vec<HarnessId>>,
    /// This device's enabled set; `None` inner value = the default set.
    prefs: Mutex<HarnessPrefsFile>,
    /// Where the prefs persist; `None` (tests, bare registries) skips writes.
    prefs_path: Mutex<Option<PathBuf>>,
    /// Fair per-harness execution gates. Runs and title generation hold a
    /// shared lease; an accepted update queues an exclusive lease. Tokio's
    /// write-preferring FIFO policy prevents a stream of new runs from
    /// starving an update that is already waiting.
    gates: Mutex<HashMap<HarnessId, Arc<tokio::sync::RwLock<()>>>>,
    pending_updates: Mutex<std::collections::HashSet<HarnessId>>,
    update_generation: tokio::sync::watch::Sender<u64>,
}

impl Default for HarnessRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl HarnessRegistry {
    pub async fn discover_models(
        &self,
        id: HarnessId,
    ) -> Result<Vec<paku_proto::Model>, HarnessError> {
        let lease = Arc::new(self.execution_lease(id).await);
        self.discover_models_with_lease(id, lease).await
    }

    /// A caller such as titling already holds a lease. Reacquiring after an
    /// update queues would deadlock it against its own existing reader.
    pub(crate) async fn discover_models_with_lease(
        &self,
        id: HarnessId,
        lease: Arc<tokio::sync::OwnedRwLockReadGuard<()>>,
    ) -> Result<Vec<paku_proto::Model>, HarnessError> {
        let harness = self.resolve(id)?;
        tokio::spawn(async move {
            // An RPC cancellation must not drop the gate before the probe's
            // own deadline and child cleanup finish.
            let _lease = lease;
            harness.models().await
        })
        .await
        .map_err(|error| HarnessError::Protocol(format!("model discovery task failed: {error}")))?
    }

    pub async fn discover_commands(
        &self,
        id: HarnessId,
        cwd: &Path,
    ) -> Result<Vec<paku_proto::SlashCommand>, HarnessError> {
        let cwd = cwd.to_owned();
        let lease = self.execution_lease(id).await;
        let harness = self.resolve(id)?;
        tokio::spawn(async move {
            let _lease = lease;
            harness.commands_for(&cwd).await
        })
        .await
        .map_err(|error| {
            HarnessError::Protocol(format!("command discovery task failed: {error}"))
        })?
    }

    pub async fn discover_skills(
        &self,
        id: HarnessId,
        cwd: &Path,
    ) -> Result<Option<Vec<paku_proto::invocation::Skill>>, HarnessError> {
        let cwd = cwd.to_owned();
        let lease = self.execution_lease(id).await;
        let harness = self.resolve(id)?;
        tokio::spawn(async move {
            // Retain the read lease through the adapter's deadline and cleanup,
            // even when the requesting RPC is dropped.
            let _lease = lease;
            harness.skills(&cwd).await
        })
        .await
        .map_err(|error| HarnessError::Protocol(format!("skill discovery task failed: {error}")))?
    }

    pub fn new() -> Self {
        let (update_generation, _) = tokio::sync::watch::channel(0);
        Self {
            installs: Default::default(),
            slots: Mutex::new(HashMap::new()),
            order: Mutex::new(Vec::new()),
            prefs: Mutex::new(HarnessPrefsFile::default()),
            prefs_path: Mutex::new(None),
            gates: Mutex::new(HashMap::new()),
            pending_updates: Mutex::new(std::collections::HashSet::new()),
            update_generation,
        }
    }

    fn gate(&self, id: HarnessId) -> Arc<tokio::sync::RwLock<()>> {
        self.gates
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(id)
            .or_insert_with(|| Arc::new(tokio::sync::RwLock::new(())))
            .clone()
    }

    /// Shared lease held for the full lifetime of a harness subprocess.
    pub async fn execution_lease(&self, id: HarnessId) -> tokio::sync::OwnedRwLockReadGuard<()> {
        let mut updates = self.update_generation.subscribe();
        loop {
            // The marker closes the small begin-update → writer-future polling
            // gap. Watch retains a generation change, so an update finishing
            // between this check and `changed()` cannot lose the wakeup.
            if self.update_pending(id) {
                let _ = updates.changed().await;
                continue;
            }
            let lease = self.gate(id).read_owned().await;
            if !self.update_pending(id) {
                return lease;
            }
            drop(lease);
        }
    }

    /// Exclusive lease held from immediately before update mutation through
    /// post-install verification.
    pub async fn update_lease(&self, id: HarnessId) -> tokio::sync::OwnedRwLockWriteGuard<()> {
        self.gate(id).write_owned().await
    }

    /// Mark an accepted update before queueing its writer. Existing persistent
    /// runtimes use this signal to retire at their next turn boundary instead
    /// of parking indefinitely while the writer waits.
    pub fn begin_update(&self, id: HarnessId) {
        self.pending_updates
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id);
    }

    pub fn end_update(&self, id: HarnessId) {
        self.pending_updates
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id);
        self.update_generation
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }

    pub fn update_pending(&self, id: HarnessId) -> bool {
        self.pending_updates
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&id)
    }

    /// Run a synchronous dispatch-boundary action only if no update has been
    /// accepted for this harness. Holding the marker lock through the action
    /// gives direct steering a strict order against `begin_update`: either the
    /// prompt is accepted first and belongs to the existing run, or it waits
    /// behind the update through the ordinary dispatch path.
    pub(crate) fn while_update_clear<T>(
        &self,
        id: HarnessId,
        action: impl FnOnce() -> T,
    ) -> Option<T> {
        let pending = self
            .pending_updates
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if pending.contains(&id) {
            None
        } else {
            Some(action())
        }
    }

    fn slots(&self) -> MutexGuard<'_, HashMap<HarnessId, Slot>> {
        self.slots.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn order(&self) -> MutexGuard<'_, Vec<HarnessId>> {
        self.order.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn prefs(&self) -> MutexGuard<'_, HarnessPrefsFile> {
        self.prefs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Load `harness-prefs.json` from the engine data dir and remember the
    /// path for writes. Corrupt/missing files fall back to the default set.
    pub fn load_prefs(&self, data_dir: &Path) {
        let path = data_dir.join("harness-prefs.json");
        let loaded = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<HarnessPrefsFile>(&text).ok())
            .unwrap_or_default();
        *self.prefs() = loaded;
        *self
            .prefs_path
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(path);
        self.migrate_legacy_prefs();
    }

    /// Fold a legacy allow-list into the opt-out shape: a registered harness
    /// missing from it was a deliberate "no", so it stays off. Rewrites the
    /// file once, which is what lets later installs auto-enable.
    fn migrate_legacy_prefs(&self) {
        let legacy = { self.prefs().enabled.take() };
        let Some(legacy) = legacy else { return };
        let registered: Vec<HarnessId> = self.order().iter().copied().collect();
        let disabled: Vec<HarnessId> = registered
            .into_iter()
            .filter(|id| auto_enabled(*id) && !legacy.contains(id))
            .collect();
        self.prefs().disabled = disabled;
        self.persist_prefs();
    }

    /// What this device offers: every harness whose CLI is FOUND, minus the
    /// user's explicit opt-outs. Enablement follows detection, so installing
    /// an agent is all it takes for it to appear in the composer.
    pub fn enabled_set(&self) -> Vec<HarnessId> {
        // Both guards drop before the installed probes run: `descriptors()`
        // takes `slots` then `order`, so holding `order` across a probe (which
        // takes `slots`) would invert the lock order.
        let registered: Vec<HarnessId> = self.order().iter().copied().collect();
        let disabled = self.prefs().disabled.clone();
        registered
            .into_iter()
            .filter(|id| auto_enabled(*id) && !disabled.contains(id) && self.installed_for(*id))
            .collect()
    }

    /// Whether this device's CLI probe passes for `id` (no spawn, no resolve).
    fn installed_for(&self, id: HarnessId) -> bool {
        match self.slots().get(&id) {
            Some(Slot::Ready(harness)) => harness.installed(),
            Some(Slot::Lazy { installed, .. }) => installed(),
            None => false,
        }
    }

    /// Flip one harness's enablement and persist. Refuses unknown harnesses,
    /// enabling one whose CLI is missing (the settings gate, enforced where
    /// the state lives), and disabling the last enabled harness — under
    /// detection-based enablement everything enabled is runnable, so the
    /// last one standing is always worth protecting (the composer needs
    /// something to run). A harness whose CLI is missing is never enabled
    /// in the first place, so turning it off is a clean no-op.
    pub fn set_enabled(&self, id: HarnessId, on: bool) -> Result<(), String> {
        if !self.slots().contains_key(&id) {
            return Err(format!("unknown harness {id:?}"));
        }
        if on && !auto_enabled(id) {
            return Err(format!("{id:?} cannot be enabled from Settings"));
        }
        if on && !self.installed_for(id) {
            return Err(format!("{id:?} CLI is not installed on this device"));
        }
        let enabled = self.enabled_set();
        match (on, enabled.contains(&id)) {
            (true, false) => {
                let mut prefs = self.prefs();
                prefs.disabled.retain(|h| *h != id);
            }
            (false, true) => {
                if enabled.len() == 1 {
                    return Err("cannot disable the last enabled harness".into());
                }
                let mut prefs = self.prefs();
                prefs.disabled.push(id);
            }
            _ => return Ok(()),
        }
        self.persist_prefs();
        Ok(())
    }

    /// Best-effort atomic write (temp + rename, the ui-settings pattern).
    fn persist_prefs(&self) {
        let Some(path) = self
            .prefs_path
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        else {
            return;
        };
        let json = match serde_json::to_string_pretty(&*self.prefs()) {
            Ok(json) => json,
            Err(err) => {
                tracing::warn!(error = %err, "harness-prefs serialize failed");
                return;
            }
        };
        let tmp = path.with_extension("json.tmp");
        if let Err(err) = std::fs::write(&tmp, json).and_then(|()| std::fs::rename(&tmp, &path)) {
            tracing::warn!(error = %err, "harness-prefs save failed");
        }
    }

    pub fn title_settings(&self) -> TitleSettings {
        self.prefs().titles.clone()
    }

    pub fn set_title_settings(&self, mut settings: TitleSettings) -> Result<(), String> {
        if let Some(id) = settings.harness {
            if !paku_harness::supports_titles(id) || !self.enabled_set().contains(&id) {
                return Err("Choose an enabled harness that supports title generation".into());
            }
        } else if settings.model.is_some() {
            return Err("Choose a title harness before choosing a model".into());
        }
        settings.model = settings.model.filter(|model| !model.trim().is_empty());
        self.prefs().titles = settings;
        self.persist_prefs();
        Ok(())
    }

    pub fn register(&self, harness: Arc<dyn Harness>) {
        let id = harness.id();
        if self.slots().insert(id, Slot::Ready(harness)).is_none() {
            self.order().push(id);
        }
    }

    /// Register a slot resolved on first `resolve` (the factory result is
    /// cached). `installed` is the CLI-presence probe run per `descriptors()`
    /// call; it must never spawn.
    pub fn register_lazy(
        &self,
        descriptor: HarnessDescriptor,
        installed: InstalledProbe,
        factory: Factory,
    ) {
        let id = descriptor.id;
        if self
            .slots()
            .insert(
                id,
                Slot::Lazy {
                    descriptor,
                    installed,
                    factory,
                },
            )
            .is_none()
        {
            self.order().push(id);
        }
    }

    pub fn resolve(&self, id: HarnessId) -> Result<Arc<dyn Harness>, HarnessError> {
        let mut slots = self.slots();
        match slots.get(&id) {
            Some(Slot::Ready(harness)) => Ok(harness.clone()),
            Some(Slot::Lazy { factory, .. }) => {
                let harness = factory()?;
                slots.insert(id, Slot::Ready(harness.clone()));
                Ok(harness)
            }
            None => Err(HarnessError::NotInstalled(format!("{id:?}"))),
        }
    }

    /// Catalog for `ListHarnesses` — never forces a lazy resolve.
    pub fn descriptors(&self) -> Vec<HarnessDescriptor> {
        let enabled = self.enabled_set();
        let slots = self.slots();
        self.order()
            .iter()
            .filter_map(|id| {
                let mut descriptor = match slots.get(id) {
                    Some(Slot::Ready(harness)) => describe(harness.as_ref()),
                    Some(Slot::Lazy {
                        descriptor,
                        installed,
                        ..
                    }) => HarnessDescriptor {
                        installed: installed(),
                        ..descriptor.clone()
                    },
                    None => return None,
                };
                descriptor.enabled = Some(enabled.contains(id));
                descriptor.can_install = paku_harness::install::can_install(*id);
                Some(descriptor)
            })
            .collect()
    }
}

/// The production registry: only Pi, resolved lazily on the first run/model call.
/// Mock is available solely through explicit injection into a bare registry.
pub fn default_registry() -> HarnessRegistry {
    // Warm PATH discovery without delaying the first Pi request.
    paku_harness::shell_env::prewarm();
    let registry = HarnessRegistry::new();
    // Native Pi RPC. Thinking levels are discovered per model.
    registry.register_lazy(
        HarnessDescriptor {
            id: HarnessId::Pi,
            name: "Pi".into(),
            supports_steering: true,
            steering_mode: SteeringMode::StepBoundary,
            reasoning_levels: Vec::new(),
            installed: true,
            can_install: false,
            enabled: None,
        },
        Box::new(|| paku_harness::PiHarness::new().installed()),
        Box::new(|| Ok(Arc::new(paku_harness::PiHarness::new()) as Arc<dyn Harness>)),
    );
    registry
}

#[cfg(test)]
mod tests {
    use super::*;
    use paku_harness::mock::MockHarness;

    fn descriptor(id: HarnessId) -> HarnessDescriptor {
        HarnessDescriptor {
            id,
            name: format!("{id:?}"),
            supports_steering: true,
            steering_mode: SteeringMode::StepBoundary,
            reasoning_levels: vec![],
            installed: true,
            can_install: false,
            enabled: None,
        }
    }

    /// An injected slot with a fixed probe and no real subprocess.
    fn test_slot(registry: &HarnessRegistry, id: HarnessId, installed: bool) {
        registry.register_lazy(
            descriptor(id),
            Box::new(move || installed),
            Box::new(|| Err(HarnessError::NotInstalled("test slot".into()))),
        );
    }

    #[test]
    fn mid_turn_steering_requires_support_and_a_step_boundary() {
        let mut descriptor = descriptor(HarnessId::Mock);
        assert!(descriptor.steers_mid_turn());
        descriptor.steering_mode = SteeringMode::TurnBoundary;
        assert!(!descriptor.steers_mid_turn());
        descriptor.steering_mode = SteeringMode::StepBoundary;
        descriptor.supports_steering = false;
        assert!(!descriptor.steers_mid_turn());
    }

    #[test]
    fn lazy_slot_lists_without_resolving_and_caches_the_factory_result() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let registry = HarnessRegistry::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        registry.register_lazy(
            descriptor(HarnessId::Mock),
            Box::new(|| false),
            Box::new(move || {
                counted.fetch_add(1, Ordering::SeqCst);
                Ok(Arc::new(MockHarness { script: vec![] }))
            }),
        );
        let listed = registry.descriptors();
        assert_eq!(listed.len(), 1);
        assert!(!listed[0].installed);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let first = registry.resolve(HarnessId::Mock).unwrap();
        let second = registry.resolve(HarnessId::Mock).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn default_registry_registers_only_pi_and_rejects_mock() {
        let registry = default_registry();
        let ids: Vec<_> = registry.descriptors().into_iter().map(|d| d.id).collect();
        assert_eq!(ids, vec![HarnessId::Pi]);
        assert!(matches!(
            registry.resolve(HarnessId::Mock),
            Err(HarnessError::NotInstalled(_))
        ));
        let pi = registry.resolve(HarnessId::Pi).unwrap();
        assert_eq!(pi.id(), HarnessId::Pi);
        assert_eq!(pi.display_name(), "Pi");
        assert_eq!(pi.steering_mode(), SteeringMode::StepBoundary);
        assert!(pi.reasoning_levels().is_empty());
        assert_eq!(registry.descriptors().len(), 1);
    }

    #[test]
    fn mock_remains_explicitly_injectable_but_never_enabled() {
        let registry = HarnessRegistry::new();
        registry.register(Arc::new(MockHarness { script: vec![] }));
        assert_eq!(
            registry.resolve(HarnessId::Mock).unwrap().id(),
            HarnessId::Mock
        );
        assert!(registry.enabled_set().is_empty());
        let mock = &registry.descriptors()[0];
        assert!(mock.installed);
        assert_eq!(mock.enabled, Some(false));
        assert!(registry.set_enabled(HarnessId::Mock, true).is_err());
        test_slot(&registry, HarnessId::Pi, true);
        assert!(registry.set_enabled(HarnessId::Pi, false).is_err());
    }

    #[test]
    fn descriptor_without_new_fields_parses_with_fallbacks() {
        let pi: HarnessDescriptor = serde_json::from_str(
            r#"{"id":"pi","name":"Pi","supportsSteering":true,"steeringMode":"step-boundary","reasoningLevels":[]}"#,
        ).unwrap();
        assert!(pi.installed);
        assert!(!pi.can_install);
        assert_eq!(pi.enabled, None);
        assert!(descriptor_enabled(&pi));
        assert!(!descriptor_enabled(&HarnessDescriptor {
            installed: false,
            ..pi
        }));
        assert!(!descriptor_enabled(&descriptor(HarnessId::Mock)));
    }

    #[test]
    fn enablement_stamps_guards_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let registry = HarnessRegistry::new();
        registry.load_prefs(dir.path());
        test_slot(&registry, HarnessId::Pi, false);
        assert_eq!(registry.descriptors()[0].enabled, Some(false));
        assert!(registry.set_enabled(HarnessId::Pi, true).is_err());
        assert!(registry.set_enabled(HarnessId::Mock, true).is_err());
        registry.set_enabled(HarnessId::Pi, false).unwrap();

        test_slot(&registry, HarnessId::Pi, true);
        // A persisted opt-out may predate detection or last-harness protection.
        registry.prefs().disabled = vec![HarnessId::Pi];
        registry.persist_prefs();
        assert!(registry.enabled_set().is_empty());
        let reloaded = HarnessRegistry::new();
        test_slot(&reloaded, HarnessId::Pi, true);
        reloaded.load_prefs(dir.path());
        assert!(reloaded.enabled_set().is_empty());
        reloaded.set_enabled(HarnessId::Pi, true).unwrap();
        reloaded.set_enabled(HarnessId::Pi, true).unwrap();
        assert_eq!(reloaded.descriptors()[0].enabled, Some(true));
        assert!(reloaded.set_enabled(HarnessId::Pi, false).is_err());
        registry.load_prefs(dir.path());
        assert_eq!(registry.enabled_set(), vec![HarnessId::Pi]);
    }

    #[test]
    fn newly_found_pi_enables_itself_without_reviving_opt_outs() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let dir = tempfile::tempdir().unwrap();
        let registry = HarnessRegistry::new();
        registry.load_prefs(dir.path());
        let found = Arc::new(AtomicBool::new(false));
        let probe = found.clone();
        registry.register_lazy(
            descriptor(HarnessId::Pi),
            Box::new(move || probe.load(Ordering::SeqCst)),
            Box::new(|| Err(HarnessError::NotInstalled("test slot".into()))),
        );
        assert!(registry.enabled_set().is_empty());
        found.store(true, Ordering::SeqCst);
        assert_eq!(registry.enabled_set(), vec![HarnessId::Pi]);
        registry.prefs().disabled = vec![HarnessId::Pi];
        registry.persist_prefs();
        found.store(false, Ordering::SeqCst);
        found.store(true, Ordering::SeqCst);
        assert!(registry.enabled_set().is_empty());
        let reloaded = HarnessRegistry::new();
        test_slot(&reloaded, HarnessId::Pi, true);
        reloaded.load_prefs(dir.path());
        assert!(reloaded.enabled_set().is_empty());
    }

    #[test]
    fn legacy_allow_list_migrates_to_opt_outs() {
        for (legacy, expected) in [
            (r#"{"enabled":[]}"#, vec![]),
            (r#"{"enabled":["pi"]}"#, vec![HarnessId::Pi]),
        ] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("harness-prefs.json"), legacy).unwrap();
            let registry = HarnessRegistry::new();
            test_slot(&registry, HarnessId::Pi, true);
            test_slot(&registry, HarnessId::Mock, true);
            registry.load_prefs(dir.path());
            assert_eq!(registry.enabled_set(), expected);
            let text = std::fs::read_to_string(dir.path().join("harness-prefs.json")).unwrap();
            assert!(!text.contains("enabled"), "{text}");
            let saved: HarnessPrefsFile = serde_json::from_str(&text).unwrap();
            assert_eq!(
                saved.disabled,
                if expected.is_empty() {
                    vec![HarnessId::Pi]
                } else {
                    vec![]
                }
            );
        }
    }

    #[test]
    fn machine_without_clis_enables_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let registry = HarnessRegistry::new();
        registry.load_prefs(dir.path());
        test_slot(&registry, HarnessId::Pi, false);
        assert!(registry.enabled_set().is_empty());
        registry.set_enabled(HarnessId::Pi, false).unwrap();
        assert!(registry.enabled_set().is_empty());
        let reloaded = HarnessRegistry::new();
        reloaded.load_prefs(dir.path());
        assert!(reloaded.enabled_set().is_empty());
    }

    #[test]
    fn pi_lazy_descriptor_matches_resolved_harness() {
        let registry = default_registry();
        let before = registry.descriptors().remove(0);
        registry.resolve(HarnessId::Pi).unwrap();
        let after = registry.descriptors().remove(0);
        assert_eq!(before.name, after.name);
        assert_eq!(before.supports_steering, after.supports_steering);
        assert_eq!(before.steering_mode, after.steering_mode);
        assert_eq!(before.reasoning_levels, after.reasoning_levels);
    }

    #[test]
    fn title_preferences_persist_and_validate_harness_model_pairs() {
        let dir = tempfile::tempdir().unwrap();
        let registry = HarnessRegistry::new();
        registry.load_prefs(dir.path());
        test_slot(&registry, HarnessId::Pi, true);
        let settings = TitleSettings {
            harness: Some(HarnessId::Pi),
            model: Some("small-test".into()),
        };
        registry.set_title_settings(settings.clone()).unwrap();
        let reloaded = HarnessRegistry::new();
        reloaded.load_prefs(dir.path());
        assert_eq!(reloaded.title_settings(), settings);
        assert!(
            registry
                .set_title_settings(TitleSettings {
                    harness: None,
                    model: Some("orphan".into())
                })
                .is_err()
        );
        assert!(
            registry
                .set_title_settings(TitleSettings {
                    harness: Some(HarnessId::Mock),
                    model: None
                })
                .is_err()
        );
        assert_eq!(registry.title_settings(), settings);
        registry
            .set_title_settings(TitleSettings::default())
            .unwrap();
        reloaded.load_prefs(dir.path());
        assert_eq!(reloaded.title_settings(), TitleSettings::default());
    }

    #[test]
    fn old_harness_preferences_default_to_automatic_titles() {
        let prefs: HarnessPrefsFile = serde_json::from_str(r#"{"disabled":["pi"]}"#).unwrap();
        assert_eq!(prefs.titles, TitleSettings::default());
        assert_eq!(prefs.disabled, vec![HarnessId::Pi]);
    }
}

#[cfg(test)]
mod gate_tests {
    use super::*;
    use paku_proto::AgentEvent;

    struct DiscoveryHarness {
        started: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }
    #[async_trait::async_trait]
    impl Harness for DiscoveryHarness {
        fn id(&self) -> HarnessId {
            HarnessId::Pi
        }
        fn display_name(&self) -> &str {
            "Discovery fixture"
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
        async fn models(&self) -> Result<Vec<paku_proto::Model>, HarnessError> {
            self.started.notify_one();
            self.release.notified().await;
            Ok(vec![])
        }
        async fn commands(&self) -> Result<Vec<paku_proto::SlashCommand>, HarnessError> {
            self.started.notify_one();
            self.release.notified().await;
            Ok(vec![])
        }
        async fn run(
            &self,
            _: paku_proto::RunRequest,
            _: paku_harness::RunControls,
        ) -> Result<
            futures::stream::BoxStream<'static, Result<AgentEvent, HarnessError>>,
            HarnessError,
        > {
            unreachable!("discovery must not start a conversation")
        }
    }

    #[tokio::test]
    async fn discovery_waits_for_updates_and_keeps_lease_after_caller_cancellation() {
        use std::time::Duration;
        for commands in [false, true] {
            let registry = Arc::new(HarnessRegistry::new());
            let harness = Arc::new(DiscoveryHarness {
                started: tokio::sync::Notify::new(),
                release: tokio::sync::Notify::new(),
            });
            registry.register(harness.clone());
            registry.begin_update(HarnessId::Pi);
            let installing = registry.update_lease(HarnessId::Pi).await;
            let caller = tokio::spawn({
                let registry = registry.clone();
                async move {
                    if commands {
                        registry
                            .discover_commands(HarnessId::Pi, Path::new("/tmp"))
                            .await
                            .map(|_| ())
                    } else {
                        registry.discover_models(HarnessId::Pi).await.map(|_| ())
                    }
                }
            });
            assert!(
                tokio::time::timeout(Duration::from_millis(30), harness.started.notified())
                    .await
                    .is_err()
            );
            drop(installing);
            registry.end_update(HarnessId::Pi);
            tokio::time::timeout(Duration::from_secs(1), harness.started.notified())
                .await
                .unwrap();
            caller.abort();
            assert!(caller.await.unwrap_err().is_cancelled());
            registry.begin_update(HarnessId::Pi);
            let mut writer = tokio::spawn({
                let registry = registry.clone();
                async move { registry.update_lease(HarnessId::Pi).await }
            });
            assert!(
                tokio::time::timeout(Duration::from_millis(30), &mut writer)
                    .await
                    .is_err()
            );
            harness.release.notify_one();
            drop(
                tokio::time::timeout(Duration::from_secs(1), writer)
                    .await
                    .unwrap()
                    .unwrap(),
            );
            registry.end_update(HarnessId::Pi);
        }
    }

    #[tokio::test]
    async fn queued_update_writer_precedes_later_dispatches() {
        let registry = Arc::new(HarnessRegistry::new());
        let running = registry.execution_lease(HarnessId::Pi).await;
        registry.begin_update(HarnessId::Pi);
        assert!(registry.update_pending(HarnessId::Pi));

        let writer_registry = registry.clone();
        let (writer_acquired_tx, writer_acquired_rx) = tokio::sync::oneshot::channel();
        let (release_writer_tx, release_writer_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let _writer = writer_registry.update_lease(HarnessId::Pi).await;
            let _ = writer_acquired_tx.send(());
            let _ = release_writer_rx.await;
        });
        tokio::task::yield_now().await;

        let reader_registry = registry.clone();
        let (reader_acquired_tx, reader_acquired_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let _reader = reader_registry.execution_lease(HarnessId::Pi).await;
            let _ = reader_acquired_tx.send(());
        });

        drop(running);
        writer_acquired_rx.await.unwrap();
        let mut reader_acquired_rx = Box::pin(reader_acquired_rx);
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(20),
                &mut reader_acquired_rx
            )
            .await
            .is_err(),
            "later reader jumped ahead of the queued update writer"
        );
        let _ = release_writer_tx.send(());
        registry.end_update(HarnessId::Pi);
        reader_acquired_rx.await.unwrap();
        assert!(!registry.update_pending(HarnessId::Pi));
    }

    #[test]
    fn update_marker_rejects_new_boundary_actions() {
        let registry = HarnessRegistry::new();
        assert_eq!(registry.while_update_clear(HarnessId::Pi, || 42), Some(42));
        registry.begin_update(HarnessId::Pi);
        let mut called = false;
        assert_eq!(
            registry.while_update_clear(HarnessId::Pi, || called = true),
            None
        );
        assert!(!called);
        registry.end_update(HarnessId::Pi);
    }
}
