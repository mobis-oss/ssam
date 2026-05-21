// Copyright 2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

pub mod backend;
mod logger;
pub mod router;

use std::sync::{Arc, OnceLock};

use logger::SsamLogger;
use router::Router;

use backend::timeline::TimelineBackend;

pub const TIMELINE_TARGET: &str = "timeline";

static TIMELINE_BACKEND: OnceLock<Arc<TimelineBackend>> = OnceLock::new();

/// Initialize the ssam-log global logger with the given maximum level.
///
/// # Errors
///
/// Returns an error if `log::set_boxed_logger` fails (e.g., already initialized).
///
/// # Examples
///
/// ```no_run
/// let _ = ssam_log::init(log::LevelFilter::Info);
/// ```
pub fn init(level: log::LevelFilter) -> anyhow::Result<()> {
    let timeline = Arc::new(TimelineBackend::new());
    TIMELINE_BACKEND
        .set(Arc::clone(&timeline))
        .map_err(|_| anyhow::anyhow!("TIMELINE_BACKEND already initialized"))?;
    let router = Router::new(timeline, level);
    log::set_boxed_logger(Box::new(SsamLogger::with_router(router)))
        .map_err(|e| anyhow::anyhow!("Failed to set logger: {e}"))?;
    log::set_max_level(level);
    Ok(())
}

/// Returns a snapshot of all recorded timeline events.
#[must_use]
pub fn get_timelines() -> Vec<serde_json::Value> {
    TIMELINE_BACKEND
        .get()
        .map_or_else(Vec::new, |b| b.get_all())
}

/// Record a `TimelineEvent` via the global logger.
///
/// The event is emitted as a structured key-value with `target: TIMELINE_TARGET`.
#[macro_export]
macro_rules! timeline {
    ($event:expr) => {{
        ::log::info!(target: $crate::TIMELINE_TARGET, event:serde = &$event; "");
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_twice_returns_err_without_panic() {
        let _ = init(log::LevelFilter::max());
        let second = init(log::LevelFilter::max());
        assert!(second.is_err());
    }

    #[test]
    fn timeline_backend_records_and_retrieves_events() {
        use crate::backend::timeline::TimelineBackend;

        let backend = TimelineBackend::new();
        let event = serde_json::json!({
            "pkg": "test-pkg",
            "phase": "mount",
            "duration_ns": 42u64,
            "kind": "started",
        });
        let key_values = [("event", log::kv::Value::from_serde(&event))];
        log::Log::log(
            &backend,
            &log::Record::builder()
                .level(log::Level::Info)
                .target(TIMELINE_TARGET)
                .key_values(&key_values)
                .args(format_args!(""))
                .build(),
        );

        std::thread::sleep(std::time::Duration::from_millis(50));
        let events = backend.get_all();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["pkg"], "test-pkg");
        assert_eq!(events[0]["phase"], "mount");
        assert_eq!(events[0]["kind"], "started");
    }
}
