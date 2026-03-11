//! Dependency health circuit breaker.
//!
//! Lock-free per-dependency tracking using `AtomicU8` for state and
//! `AtomicU64` for timestamps. Hot read path is a single atomic load.

use std::collections::HashMap;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;

/// Dependencies tracked by the circuit breaker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DependencyId {
    Ollama,
    LanceDb,
    Embedding,
}

impl DependencyId {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ollama => "ollama",
            Self::LanceDb => "lancedb",
            Self::Embedding => "embedding",
        }
    }

    const ALL: [DependencyId; 3] = [Self::Ollama, Self::LanceDb, Self::Embedding];
}

impl std::fmt::Display for DependencyId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Reported status of a dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyStatus {
    Healthy,
    Down,
    HalfOpen,
}

impl DependencyStatus {
    const HEALTHY: u8 = 0;
    const DOWN: u8 = 1;
    const HALF_OPEN: u8 = 2;

    fn from_u8(v: u8) -> Self {
        match v {
            Self::DOWN => Self::Down,
            Self::HALF_OPEN => Self::HalfOpen,
            _ => Self::Healthy,
        }
    }
}

/// Snapshot of a single dependency's health.
#[derive(Debug, Clone, Serialize)]
pub struct DependencyInfo {
    pub status: DependencyStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_checked_secs_ago: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Per-dependency atomic state.
struct DepState {
    state: AtomicU8,
    /// Nanos since `epoch` when the dependency was last checked.
    last_checked_nanos: AtomicU64,
    /// Last error message (written rarely, read on status).
    last_error: RwLock<Option<String>>,
}

impl DepState {
    fn new() -> Self {
        Self {
            state: AtomicU8::new(DependencyStatus::HEALTHY),
            last_checked_nanos: AtomicU64::new(0),
            last_error: RwLock::new(None),
        }
    }
}

/// Lock-free circuit breaker for dependency health.
pub struct DependencyHealth {
    deps: HashMap<DependencyId, DepState>,
    recovery_timeout: Duration,
    epoch: Instant,
}

impl DependencyHealth {
    /// Create a new health tracker with the given recovery timeout.
    pub fn new(recovery_timeout: Duration) -> Self {
        let mut deps = HashMap::new();
        for id in DependencyId::ALL {
            deps.insert(id, DepState::new());
        }
        Self {
            deps,
            recovery_timeout,
            epoch: Instant::now(),
        }
    }

    /// Create with default 1-second recovery timeout.
    pub fn with_defaults() -> Self {
        Self::new(Duration::from_secs(1))
    }

    fn nanos_since_epoch(&self) -> u64 {
        self.epoch.elapsed().as_nanos() as u64
    }

    fn dep(&self, id: DependencyId) -> &DepState {
        &self.deps[&id]
    }

    /// Check whether a dependency is available. Returns `Ok(())` if healthy
    /// or half-open (probe allowed). Returns `Err` if down and recovery
    /// timeout has not elapsed.
    pub fn check(&self, id: DependencyId) -> crate::error::Result<()> {
        let dep = self.dep(id);
        let state = DependencyStatus::from_u8(dep.state.load(Ordering::Acquire));

        match state {
            DependencyStatus::Healthy => Ok(()),
            DependencyStatus::HalfOpen => Ok(()),
            DependencyStatus::Down => {
                let last_nanos = dep.last_checked_nanos.load(Ordering::Acquire);
                let now_nanos = self.nanos_since_epoch();
                let elapsed = Duration::from_nanos(now_nanos.saturating_sub(last_nanos));

                if elapsed >= self.recovery_timeout {
                    // Transition to half-open: allow one probe
                    let _ = dep.state.compare_exchange(
                        DependencyStatus::DOWN,
                        DependencyStatus::HALF_OPEN,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    );
                    Ok(())
                } else {
                    let error_msg = dep
                        .last_error
                        .read()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone()
                        .unwrap_or_else(|| "unavailable".into());
                    Err(crate::error::MemcanError::DependencyUnavailable {
                        dependency: id.as_str().to_string(),
                        message: error_msg,
                    })
                }
            }
        }
    }

    /// Record a failure for a dependency. Transitions to Down.
    pub fn report_failure(&self, id: DependencyId, error: &str) {
        let dep = self.dep(id);
        dep.state.store(DependencyStatus::DOWN, Ordering::Release);
        dep.last_checked_nanos
            .store(self.nanos_since_epoch(), Ordering::Release);
        *dep.last_error.write().unwrap_or_else(|e| e.into_inner()) = Some(error.to_string());
    }

    /// Record a success for a dependency. Transitions to Healthy.
    pub fn report_success(&self, id: DependencyId) {
        let dep = self.dep(id);
        dep.state
            .store(DependencyStatus::HEALTHY, Ordering::Release);
        dep.last_checked_nanos
            .store(self.nanos_since_epoch(), Ordering::Release);
        *dep.last_error.write().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// Snapshot of all dependency statuses.
    pub fn status(&self) -> HashMap<String, DependencyInfo> {
        let now_nanos = self.nanos_since_epoch();
        let mut result = HashMap::new();

        for id in DependencyId::ALL {
            let dep = self.dep(id);
            let state = DependencyStatus::from_u8(dep.state.load(Ordering::Acquire));
            let last_nanos = dep.last_checked_nanos.load(Ordering::Acquire);

            let last_checked_secs_ago = if last_nanos == 0 {
                None
            } else {
                let elapsed_nanos = now_nanos.saturating_sub(last_nanos);
                Some(elapsed_nanos as f64 / 1_000_000_000.0)
            };

            let error = dep
                .last_error
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone();

            result.insert(
                id.as_str().to_string(),
                DependencyInfo {
                    status: state,
                    last_checked_secs_ago,
                    error,
                },
            );
        }

        result
    }

    /// Returns true if all dependencies are healthy.
    pub fn all_healthy(&self) -> bool {
        DependencyId::ALL.iter().all(|id| {
            DependencyStatus::from_u8(self.dep(*id).state.load(Ordering::Acquire))
                == DependencyStatus::Healthy
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initially_all_healthy() {
        let health = DependencyHealth::with_defaults();
        assert!(health.all_healthy());
        assert!(health.check(DependencyId::Ollama).is_ok());
        assert!(health.check(DependencyId::LanceDb).is_ok());
        assert!(health.check(DependencyId::Embedding).is_ok());
    }

    #[test]
    fn failure_marks_dependency_down() {
        let health = DependencyHealth::with_defaults();
        health.report_failure(DependencyId::Ollama, "connection refused");

        assert!(!health.all_healthy());
        let result = health.check(DependencyId::Ollama);
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert!(err.is_dependency_unavailable());
        assert!(err.to_string().contains("connection refused"));
    }

    #[test]
    fn failure_on_one_does_not_affect_others() {
        let health = DependencyHealth::with_defaults();
        health.report_failure(DependencyId::Ollama, "down");

        assert!(health.check(DependencyId::LanceDb).is_ok());
        assert!(health.check(DependencyId::Embedding).is_ok());
    }

    #[test]
    fn success_after_failure_restores_healthy() {
        let health = DependencyHealth::with_defaults();
        health.report_failure(DependencyId::LanceDb, "disk full");
        assert!(health.check(DependencyId::LanceDb).is_err());

        health.report_success(DependencyId::LanceDb);
        assert!(health.check(DependencyId::LanceDb).is_ok());
        assert!(health.all_healthy());
    }

    #[test]
    fn half_open_after_timeout() {
        let health = DependencyHealth::new(Duration::from_millis(10));
        health.report_failure(DependencyId::Embedding, "model load failed");

        // Immediately should be down
        assert!(health.check(DependencyId::Embedding).is_err());

        // Wait for recovery timeout
        std::thread::sleep(Duration::from_millis(15));

        // Should transition to half-open and allow probe
        assert!(health.check(DependencyId::Embedding).is_ok());

        // State should be half-open now
        let status = health.status();
        assert_eq!(status["embedding"].status, DependencyStatus::HalfOpen);
    }

    #[test]
    fn half_open_success_returns_to_healthy() {
        let health = DependencyHealth::new(Duration::from_millis(10));
        health.report_failure(DependencyId::Ollama, "timeout");

        std::thread::sleep(Duration::from_millis(15));
        assert!(health.check(DependencyId::Ollama).is_ok()); // half-open

        health.report_success(DependencyId::Ollama);
        let status = health.status();
        assert_eq!(status["ollama"].status, DependencyStatus::Healthy);
        assert!(status["ollama"].error.is_none());
    }

    #[test]
    fn half_open_failure_returns_to_down() {
        let health = DependencyHealth::new(Duration::from_millis(10));
        health.report_failure(DependencyId::Ollama, "timeout");

        std::thread::sleep(Duration::from_millis(15));
        assert!(health.check(DependencyId::Ollama).is_ok()); // half-open

        health.report_failure(DependencyId::Ollama, "still broken");

        // Should be down again, immediate check fails
        assert!(health.check(DependencyId::Ollama).is_err());
    }

    #[test]
    fn status_snapshot_includes_all_deps() {
        let health = DependencyHealth::with_defaults();
        let status = health.status();

        assert!(status.contains_key("ollama"));
        assert!(status.contains_key("lancedb"));
        assert!(status.contains_key("embedding"));

        for info in status.values() {
            assert_eq!(info.status, DependencyStatus::Healthy);
            assert!(info.last_checked_secs_ago.is_none()); // never checked
            assert!(info.error.is_none());
        }
    }

    #[test]
    fn status_includes_error_message_when_down() {
        let health = DependencyHealth::with_defaults();
        health.report_failure(DependencyId::LanceDb, "connection refused");

        let status = health.status();
        let lance = &status["lancedb"];
        assert_eq!(lance.status, DependencyStatus::Down);
        assert_eq!(lance.error.as_deref(), Some("connection refused"));
        assert!(lance.last_checked_secs_ago.is_some());
    }

    #[test]
    fn status_last_checked_updates_on_success() {
        let health = DependencyHealth::with_defaults();
        health.report_success(DependencyId::Ollama);

        let status = health.status();
        assert!(status["ollama"].last_checked_secs_ago.is_some());
    }

    #[test]
    fn concurrent_access_is_safe() {
        use std::sync::Arc;

        let health = Arc::new(DependencyHealth::new(Duration::from_millis(5)));
        let mut handles = vec![];

        for _ in 0..10 {
            let h = Arc::clone(&health);
            handles.push(std::thread::spawn(move || {
                for _ in 0..100 {
                    h.report_failure(DependencyId::Ollama, "err");
                    let _ = h.check(DependencyId::Ollama);
                    h.report_success(DependencyId::Ollama);
                    let _ = h.status();
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        // Should not panic or deadlock; final state is valid
        let status = health.status();
        assert!(
            status["ollama"].status == DependencyStatus::Healthy
                || status["ollama"].status == DependencyStatus::Down
        );
    }

    #[test]
    fn dependency_id_display() {
        assert_eq!(DependencyId::Ollama.to_string(), "ollama");
        assert_eq!(DependencyId::LanceDb.to_string(), "lancedb");
        assert_eq!(DependencyId::Embedding.to_string(), "embedding");
    }

    #[test]
    fn dependency_unavailable_error_variant() {
        let err = crate::error::MemcanError::DependencyUnavailable {
            dependency: "ollama".into(),
            message: "connection refused".into(),
        };
        assert!(err.is_dependency_unavailable());
        assert!(!err.is_llm_error());
        assert!(err.to_string().contains("ollama"));
        assert!(err.to_string().contains("connection refused"));
    }
}
