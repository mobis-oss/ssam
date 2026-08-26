// Copyright 2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

pub mod backend;
mod logger;
pub mod router;

use std::sync::{Arc, OnceLock};

use logger::SsamLogger;
use router::Router;

use backend::timeline::TimelineBackend;

pub use backend::timeline::{TimelineEvent, TimelineEventKind};

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
    let timeline = Arc::new(TimelineBackend::new()?);
    TIMELINE_BACKEND
        .set(Arc::clone(&timeline))
        .map_err(|_| anyhow::anyhow!("TIMELINE_BACKEND already initialized"))?;
    let router = Router::new(Arc::clone(&timeline), level);
    log::set_boxed_logger(Box::new(SsamLogger::with_router(router)))
        .map_err(|e| anyhow::anyhow!("Failed to set logger: {e}"))?;
    log::set_max_level(level);
    Ok(())
}

/// Returns a snapshot of all recorded timeline events.
#[must_use]
pub fn get_timelines() -> Vec<TimelineEvent> {
    TIMELINE_BACKEND
        .get()
        .map_or_else(Vec::new, |b| b.get_all())
}

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

        let backend = TimelineBackend::new().unwrap();
        let event = TimelineEvent {
            pkg: "test-pkg".to_owned(),
            phase: "mount".to_owned(),
            duration_ns: 42,
            kind: TimelineEventKind::Started,
        };
        backend.record_event(event.clone());
        let events = backend.get_all();
        assert_eq!(events, vec![event]);
    }

    #[test]
    fn timeline_macro_records_event_in_global_backend() {
        let _ = init(log::LevelFilter::max());
        let event = TimelineEvent {
            pkg: "test-pkg".to_owned(),
            phase: "mount".to_owned(),
            duration_ns: 42,
            kind: TimelineEventKind::Started,
        };
        timeline!(event.clone());

        assert!(get_timelines().iter().any(|candidate| candidate == &event));
    }
}
