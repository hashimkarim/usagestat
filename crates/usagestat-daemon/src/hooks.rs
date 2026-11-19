//! Daemon side of event hooks: per-hook transition detectors fed by the
//! poller, and a small bounded worker pool that delivers events so a slow or
//! failing hook never blocks polling.

use chrono::{DateTime, Utc};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread;
use usagestat_core::UsageSnapshot;
use usagestat_core::budgets::{BudgetConfig, BudgetTracker};
use usagestat_core::hooks::{HookConfig, HookDetector, HookEvent, HookPayload};

const WORKERS: usize = 2;
const QUEUE: usize = 32;

type Job = (usize, Arc<HookConfig>, HookPayload);

/// One active hook: its position among `[[hooks]]` (used in logs, matching
/// `usagestat hooks test`), its config and its transition state.
struct Entry {
    index: usize,
    hook: Arc<HookConfig>,
    detector: HookDetector,
    budgets: BudgetTracker,
}

/// Detectors for the configured hooks plus the delivery queue.
pub struct Hooks {
    entries: Vec<Entry>,
    configured: Vec<HookConfig>,
    queue: Option<SyncSender<Job>>,
}

impl Hooks {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            configured: Vec::new(),
            queue: None,
        }
    }

    /// Adopt a (re)loaded hook list. Unchanged hooks keep their baselines;
    /// new or edited hooks start fresh so an edit never fires for a
    /// crossing that spanned it.
    pub fn sync(&mut self, configured: &[HookConfig]) {
        if self.configured == configured {
            return;
        }
        let mut previous = std::mem::take(&mut self.entries);
        for (index, hook) in configured.iter().enumerate() {
            if !hook.enabled {
                continue;
            }
            if let Err(error) = hook.validate() {
                log::warn!("hook {index} is skipped: {error}");
                continue;
            }
            let (detector, budgets) = previous
                .iter()
                .position(|old| old.hook.as_ref() == hook)
                .map(|position| {
                    let old = previous.swap_remove(position);
                    (old.detector, old.budgets)
                })
                .unwrap_or_else(|| {
                    (
                        HookDetector::new(hook.threshold_list()),
                        BudgetTracker::default(),
                    )
                });
            self.entries.push(Entry {
                index,
                hook: Arc::new(hook.clone()),
                detector,
                budgets,
            });
        }
        self.configured = configured.to_vec();
        if !self.entries.is_empty() && self.queue.is_none() {
            self.queue = Some(start_workers(|hook, payload| {
                usagestat_plugins::hooks::deliver(hook, payload)
            }));
        }
    }

    /// Whether any active hook wants budget events; budgets cost nothing otherwise.
    pub fn wants_budgets(&self) -> bool {
        self.entries
            .iter()
            .any(|e| e.hook.subscribes(HookEvent::BudgetThreshold))
    }

    /// Feed month-to-date spend for each valid budget to subscribed hooks.
    pub fn observe_budgets(&mut self, spent: &[(BudgetConfig, f64)], now: DateTime<Utc>) {
        let month = now.format("%Y-%m").to_string();
        for entry in &mut self.entries {
            if !entry.hook.subscribes(HookEvent::BudgetThreshold) {
                continue;
            }
            let thresholds = entry.hook.threshold_list();
            for (budget, amount) in spent {
                if let Some(level) = entry.budgets.observe(budget, &month, *amount, &thresholds) {
                    let payload = HookPayload::budget(budget, *amount, level, now);
                    Self::enqueue(&self.queue, entry.index, &entry.hook, payload);
                }
            }
        }
    }

    fn enqueue(
        queue: &Option<SyncSender<Job>>,
        index: usize,
        hook: &Arc<HookConfig>,
        payload: HookPayload,
    ) {
        let Some(queue) = queue else { return };
        let event = payload.event;
        match queue.try_send((index, Arc::clone(hook), payload)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                log::warn!("hook {index} dropped {event} event: delivery queue is full")
            }
            Err(TrySendError::Disconnected(_)) => {
                log::warn!("hook {index} dropped {event} event: delivery stopped")
            }
        }
    }

    /// Feed one fresh snapshot to every hook and queue the events it caused.
    pub fn observe(&mut self, plugin_id: &str, snapshot: &UsageSnapshot, now: DateTime<Utc>) {
        for Entry {
            index,
            hook,
            detector,
            ..
        } in &mut self.entries
        {
            let index = *index;
            if !hook.watches(&snapshot.provider_id, plugin_id) {
                continue;
            }
            for payload in detector.observe(plugin_id, snapshot, now) {
                if hook.subscribes(payload.event) {
                    Self::enqueue(&self.queue, index, hook, payload);
                }
            }
        }
    }
}

/// Workers are detached: shutdown never waits on a slow webhook, and command
/// hooks are stopped by the process helper's shutdown handling.
fn start_workers(
    deliver: impl Fn(&HookConfig, &HookPayload) -> Result<(), String> + Send + Sync + 'static,
) -> SyncSender<Job> {
    let (sender, receiver) = sync_channel::<Job>(QUEUE);
    let receiver = Arc::new(Mutex::new(receiver));
    let deliver = Arc::new(deliver);
    for worker in 0..WORKERS {
        let receiver: Arc<Mutex<Receiver<Job>>> = Arc::clone(&receiver);
        let deliver = Arc::clone(&deliver);
        let spawned = thread::Builder::new()
            .name(format!("usagestat-hook-{worker}"))
            .spawn(move || {
                loop {
                    let job = match receiver.lock() {
                        Ok(receiver) => receiver.recv(),
                        Err(_) => return,
                    };
                    let Ok((index, hook, payload)) = job else {
                        return;
                    };
                    let event = payload.event;
                    let provider = payload.provider_id.clone();
                    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        deliver(&hook, &payload)
                    }));
                    match outcome {
                        Ok(Ok(())) => log::info!("hook {index} delivered {event} for {provider}"),
                        Ok(Err(error)) => {
                            log::warn!("hook {index} failed for {event} ({provider}): {error}")
                        }
                        Err(_) => log::warn!("hook {index} panicked for {event} ({provider})"),
                    }
                }
            });
        if spawned.is_err() {
            log::warn!("hook worker {worker} could not be started");
        }
    }
    sender
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};
    use usagestat_core::hooks::HookEvent;

    #[test]
    fn full_queue_drops_events_instead_of_blocking() {
        let started = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&started);
        let gate = Arc::new(Mutex::new(()));
        let held = gate.lock().unwrap();
        let worker_gate = Arc::clone(&gate);
        let sender = start_workers(move |_, _| {
            counter.fetch_add(1, Ordering::SeqCst);
            drop(worker_gate.lock());
            Ok(())
        });
        let hook = Arc::new(HookConfig::default());
        let payload = HookPayload::sample(HookEvent::QuotaReset, "claude", Utc::now());
        let begin = Instant::now();
        let mut rejected = 0;
        for _ in 0..(QUEUE + WORKERS + 10) {
            if sender
                .try_send((0, Arc::clone(&hook), payload.clone()))
                .is_err()
            {
                rejected += 1;
            }
        }
        assert!(begin.elapsed() < Duration::from_secs(1));
        assert!(rejected >= 1);
        drop(held);
        let deadline = Instant::now() + Duration::from_secs(5);
        while started.load(Ordering::SeqCst) < QUEUE && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(started.load(Ordering::SeqCst) >= QUEUE);
    }

    #[test]
    fn sync_keeps_baselines_for_unchanged_hooks_and_skips_invalid_ones() {
        let valid = HookConfig {
            enabled: true,
            events: vec!["quota_threshold".into()],
            command: vec!["true".into()],
            ..HookConfig::default()
        };
        let invalid = HookConfig {
            enabled: true,
            events: vec!["nope".into()],
            command: vec!["true".into()],
            ..HookConfig::default()
        };
        let mut hooks = Hooks::new();
        hooks.sync(&[valid.clone(), invalid.clone()]);
        assert_eq!(hooks.entries.len(), 1);
        assert_eq!(hooks.entries[0].index, 0);
        let mut snapshot = UsageSnapshot::error("claude", "Claude", "boom");
        snapshot.fetched_at = Utc::now();
        hooks.observe("claude", &snapshot, Utc::now());
        let baseline = format!("{:?}", hooks.entries[0].detector);
        let mut edited = valid.clone();
        edited.providers = vec!["codex".into()];
        hooks.sync(&[valid.clone(), invalid, edited]);
        assert_eq!(hooks.entries.len(), 2);
        assert_eq!(format!("{:?}", hooks.entries[0].detector), baseline);
        assert_ne!(format!("{:?}", hooks.entries[1].detector), baseline);
        assert_eq!(hooks.entries[1].index, 2, "logs use the config position");
    }

    #[test]
    fn budget_crossings_queue_only_for_subscribed_hooks() {
        let budget_hook = HookConfig {
            enabled: true,
            events: vec!["budget_threshold".into()],
            command: vec!["true".into()],
            ..HookConfig::default()
        };
        let quota_hook = HookConfig {
            events: vec!["quota_threshold".into()],
            ..budget_hook.clone()
        };
        let mut hooks = Hooks::new();
        hooks.sync(&[quota_hook.clone()]);
        assert!(!hooks.wants_budgets());
        hooks.sync(&[quota_hook, budget_hook]);
        assert!(hooks.wants_budgets());
        let (sender, receiver) = sync_channel(8);
        hooks.queue = Some(sender);
        let budget = BudgetConfig {
            project: Some("app".into()),
            monthly_usd: 100.0,
            ..BudgetConfig::default()
        };
        let now = Utc::now();
        hooks.observe_budgets(&[(budget.clone(), 10.0)], now);
        hooks.observe_budgets(&[(budget.clone(), 92.0)], now);
        let (index, _, payload) = receiver.try_recv().unwrap();
        assert_eq!(index, 1);
        assert_eq!(payload.event, HookEvent::BudgetThreshold);
        assert_eq!(payload.threshold, Some(90.0));
        assert_eq!(payload.project.as_deref(), Some("app"));
        assert_eq!(payload.spent_usd, Some(92.0));
        assert!(receiver.try_recv().is_err());
    }
}
