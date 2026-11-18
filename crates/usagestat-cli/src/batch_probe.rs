use anyhow::{Context, Result};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};
use usagestat_core::process::{self, CancellationToken};
use usagestat_core::{LoadedProvider, ProviderConfig, UsageSnapshot};
use usagestat_plugins::probe_provider;

pub fn probe_timeout_secs() -> u64 {
    std::env::var("USAGESTAT_PROBE_TIMEOUT_SEC")
        .or_else(|_| std::env::var("AI_USAGE_PROBE_TIMEOUT_SEC"))
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&s: &u64| s > 0)
        .unwrap_or(120)
}

/// Register native console or Unix shutdown notifications.
pub fn register_interrupt_flag() -> Result<Arc<AtomicBool>> {
    usagestat_core::signals::register().context("register shutdown notifications")
}

/// Run a probe with a wall-clock timeout. Checks the interrupt flag every 200ms.
///
/// Cancellation interrupts JavaScript execution and nested helper processes.
/// Blocking provider HTTP calls retain their own transport deadline.
pub fn run_probe_with_timeout(
    provider: &LoadedProvider,
    source_mode: &str,
    provider_config: Option<&ProviderConfig>,
    interrupt: Option<&Arc<AtomicBool>>,
) -> UsageSnapshot {
    let provider_id = provider.manifest.id.clone();
    let timeout_sec = probe_timeout_secs();
    let deadline = Instant::now() + Duration::from_secs(timeout_sec);

    let provider_thread = provider.clone();
    let source_thread = source_mode.to_string();
    let config_thread = provider_config.cloned();
    let cancellation = CancellationToken::with_interrupt(interrupt.cloned());
    let worker_cancellation = cancellation.clone();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let snapshot = process::with_cancellation(worker_cancellation, || {
            probe_provider(&provider_thread, &source_thread, config_thread.as_ref())
        });
        let _ = tx.send(snapshot);
    });

    const TICK: Duration = Duration::from_millis(200);

    loop {
        if let Some(flag) = interrupt {
            if flag.load(Ordering::SeqCst) {
                cancellation.cancel();
                // Give helper runners time to terminate and reap their trees
                // before process::exit bypasses Rust destructors.
                let _ = rx.recv_timeout(Duration::from_secs(2));
                eprintln!("\nusagestat: interrupted");
                std::process::exit(130);
            }
        }

        let now = Instant::now();
        if now >= deadline {
            cancellation.cancel();
            let _ = rx.recv_timeout(Duration::from_secs(2));
            eprintln!(
                "usagestat: probe timed out after {timeout_sec}s for `{provider_id}` \
                 (set USAGESTAT_PROBE_TIMEOUT_SEC to override)"
            );
            return UsageSnapshot::error(
                &provider_id,
                &provider.manifest.name,
                format!("Probe timed out after {timeout_sec}s."),
            );
        }

        let remaining = deadline.saturating_duration_since(now);
        match rx.recv_timeout(TICK.min(remaining)) {
            Ok(snapshot) => {
                // The helper may notice the signal and finish before this
                // thread's next polling tick. Preserve the CLI interrupt exit.
                if interrupt.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
                    eprintln!("\nusagestat: interrupted");
                    std::process::exit(130);
                }
                return snapshot;
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                return UsageSnapshot::error(
                    &provider_id,
                    &provider.manifest.name,
                    "Probe thread ended without a result (panic?).".to_string(),
                );
            }
        }
    }
}
