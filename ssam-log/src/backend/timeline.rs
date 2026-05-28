// Copyright 2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use anyhow::Context;

// ---------------------------------------------------------------------------
// Internal channel types
// ---------------------------------------------------------------------------

enum TimelineCommand {
    Record(serde_json::Value),
    GetAll(mpsc::SyncSender<Vec<serde_json::Value>>),
}

// ---------------------------------------------------------------------------
// TimelineBackend
// ---------------------------------------------------------------------------

pub struct TimelineBackend {
    sender: mpsc::Sender<TimelineCommand>,
    _thread: JoinHandle<()>,
}

impl TimelineBackend {
    /// # Errors
    ///
    /// Returns an error if the OS fails to spawn the accumulate thread.
    pub fn new() -> anyhow::Result<Self> {
        let (tx, rx) = mpsc::channel::<TimelineCommand>();
        let handle = thread::Builder::new()
            .name("ssam-timeline".into())
            .spawn(move || accumulate(&rx))
            .context("failed to spawn timeline accumulate thread")?;
        Ok(Self {
            sender: tx,
            _thread: handle,
        })
    }

    #[must_use]
    pub fn get_all(&self) -> Vec<serde_json::Value> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        if self.sender.send(TimelineCommand::GetAll(reply_tx)).is_err() {
            return Vec::new();
        }
        if let Ok(map) = reply_rx.recv() {
            map
        } else {
            log::warn!("timeline: accumulate thread is gone; returning empty map");
            Vec::new()
        }
    }

    fn record_event(&self, value: serde_json::Value) {
        if self.sender.send(TimelineCommand::Record(value)).is_err() {
            log::warn!("timeline: accumulate thread is gone; event dropped");
        }
    }
}

impl log::Log for TimelineBackend {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.target() == crate::TIMELINE_TARGET
    }

    fn log(&self, record: &log::Record) {
        let Some(value) = record.key_values().get(log::kv::Key::from("event")) else {
            return;
        };

        match serde_json::to_value(value) {
            Ok(value) => self.record_event(value),
            Err(e) => log::warn!("timeline: invalid event field: {e}"),
        }
    }

    fn flush(&self) {}
}

// ---------------------------------------------------------------------------
// Accumulate thread
// ---------------------------------------------------------------------------

fn accumulate(rx: &mpsc::Receiver<TimelineCommand>) {
    let mut store: Vec<serde_json::Value> = Vec::new();

    while let Ok(cmd) = rx.recv() {
        match cmd {
            TimelineCommand::Record(value) => {
                store.push(value);
            }
            TimelineCommand::GetAll(reply_tx) => {
                // Ignore send failure — caller may have timed out.
                let _ = reply_tx.send(store.clone());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_event_value(pkg: &str, phase: &str, duration_ns: u64, kind: &str) -> serde_json::Value {
        serde_json::json!({
            "pkg": pkg,
            "phase": phase,
            "duration_ns": duration_ns,
            "kind": kind,
        })
    }

    fn log_to_backend(
        backend: &TimelineBackend,
        pkg: &str,
        phase: &str,
        duration_ns: u64,
        kind: &str,
    ) {
        backend.record_event(make_event_value(pkg, phase, duration_ns, kind));
    }

    #[test]
    fn accumulates_started() {
        let backend = TimelineBackend::new().unwrap();
        log_to_backend(&backend, "pkg-a", "mount", 1_000_000, "started");
        let events = backend.get_all();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["pkg"], "pkg-a");
        assert_eq!(events[0]["phase"], "mount");
        assert_eq!(events[0]["kind"], "started");
        assert_eq!(events[0]["duration_ns"], 1_000_000u64);
    }

    #[test]
    fn accumulates_completed() {
        let backend = TimelineBackend::new().unwrap();
        log_to_backend(&backend, "pkg-b", "setup", 2_000_000, "completed");
        let events = backend.get_all();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["pkg"], "pkg-b");
        assert_eq!(events[0]["kind"], "completed");
    }

    #[test]
    fn multiple_packages() {
        let backend = TimelineBackend::new().unwrap();
        log_to_backend(&backend, "alpha", "mount", 100, "started");
        log_to_backend(&backend, "beta", "setup", 200, "completed");
        let events = backend.get_all();
        assert_eq!(events.len(), 2);
        assert!(events.iter().any(|e| e["pkg"] == "alpha"));
        assert!(events.iter().any(|e| e["pkg"] == "beta"));
    }

    #[test]
    fn non_destructive_read() {
        let backend = TimelineBackend::new().unwrap();
        log_to_backend(&backend, "pkg-c", "phase1", 500, "started");
        let first = backend.get_all();
        let second = backend.get_all();
        assert_eq!(first.len(), second.len());
        assert_eq!(first[0]["pkg"], second[0]["pkg"]);
    }

    #[test]
    fn invalid_log_without_event_field_ignored() {
        let backend = TimelineBackend::new().unwrap();
        log::Log::log(
            &backend,
            &log::Record::builder()
                .level(log::Level::Info)
                .target(crate::TIMELINE_TARGET)
                .args(format_args!("plain message"))
                .build(),
        );
        let events = backend.get_all();
        assert!(events.is_empty());
    }

    #[test]
    fn kv_event_is_accepted() {
        let backend = TimelineBackend::new().unwrap();
        let event = make_event_value("pkg-kv", "phase", 7, "started");
        let key_values = [("event", log::kv::Value::from_serde(&event))];
        log::Log::log(
            &backend,
            &log::Record::builder()
                .level(log::Level::Info)
                .target(crate::TIMELINE_TARGET)
                .key_values(&key_values)
                .args(format_args!(""))
                .build(),
        );

        let events = backend.get_all();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["pkg"], "pkg-kv");
    }

    #[test]
    fn unknown_shape_kv_event_is_stored_as_value() {
        let backend = TimelineBackend::new().unwrap();
        let payload = serde_json::json!({"foo": "bar"});
        let key_values = [("event", log::kv::Value::from_serde(&payload))];
        log::Log::log(
            &backend,
            &log::Record::builder()
                .level(log::Level::Info)
                .target(crate::TIMELINE_TARGET)
                .key_values(&key_values)
                .args(format_args!(""))
                .build(),
        );

        let events = backend.get_all();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn record_event_after_thread_exit_does_not_panic() {
        let (tx, rx) = std::sync::mpsc::channel::<TimelineCommand>();
        let handle = std::thread::Builder::new()
            .name("ssam-timeline-test".into())
            .spawn(move || {
                drop(rx);
            })
            .unwrap();
        handle.join().unwrap();
        let dead_backend = TimelineBackend {
            sender: tx,
            _thread: std::thread::Builder::new()
                .name("ssam-timeline-noop".into())
                .spawn(|| {})
                .unwrap(),
        };
        dead_backend.record_event(serde_json::json!({
            "pkg": "x",
            "phase": "y",
            "duration_ns": 0u64,
            "kind": "started",
        }));
    }
}
