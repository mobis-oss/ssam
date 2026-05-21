// Copyright 2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::sync::Arc;

use crate::backend::{simplelog::SimpleLogBackend, timeline::TimelineBackend};

pub struct Router {
    routes: HashMap<&'static str, Arc<dyn log::Log + Send + Sync>>,
    default: Arc<dyn log::Log + Send + Sync>,
}

impl Router {
    #[must_use]
    pub fn new(timeline: Arc<TimelineBackend>, level: log::LevelFilter) -> Self {
        let mut routes: HashMap<&'static str, Arc<dyn log::Log + Send + Sync>> = HashMap::new();
        routes.insert(crate::TIMELINE_TARGET, timeline);
        Self {
            routes,
            default: Arc::new(SimpleLogBackend::new(level)),
        }
    }

    pub fn route(&self, record: &log::Record) {
        let logger = self.routes.get(record.target()).unwrap_or(&self.default);
        logger.log(record);
    }

    pub fn flush_all(&self) {
        for logger in self.routes.values() {
            logger.flush();
        }
        self.default.flush();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn make_router() -> (Arc<TimelineBackend>, Router) {
        let backend = Arc::new(TimelineBackend::new());
        let router = Router::new(Arc::clone(&backend), log::LevelFilter::max());
        (backend, router)
    }

    fn route_record(router: &Router, event: &serde_json::Value) {
        let key_values = [("event", log::kv::Value::from_serde(event))];
        router.route(
            &log::Record::builder()
                .level(log::Level::Info)
                .target(crate::TIMELINE_TARGET)
                .key_values(&key_values)
                .args(format_args!(""))
                .build(),
        );
    }

    #[test]
    fn routes_timeline_target_to_timeline_backend() {
        let (backend, router) = make_router();
        let event = serde_json::json!({
            "pkg": "p",
            "phase": "q",
            "duration_ns": 1u64,
            "kind": "started",
        });
        route_record(&router, &event);
        let map = backend.get_all();
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn non_timeline_target_does_not_reach_timeline_backend() {
        let (backend, router) = make_router();
        router.route(
            &log::Record::builder()
                .level(log::Level::Info)
                .target("some_other_target")
                .args(format_args!("unrelated log"))
                .build(),
        );
        let events = backend.get_all();
        assert!(events.is_empty());
    }

    #[test]
    fn flush_all_does_not_panic() {
        let (_backend, router) = make_router();
        router.flush_all();
    }
}
