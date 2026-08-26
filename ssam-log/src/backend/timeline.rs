// Copyright 2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use anyhow::Context;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TimelineEvent {
    pub pkg: String,
    pub phase: String,
    pub duration_ns: u64,
    pub kind: TimelineEventKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineEventKind {
    Started,
    Completed,
}

// ---------------------------------------------------------------------------
// Internal channel types
// ---------------------------------------------------------------------------

enum TimelineCommand {
    Record(TimelineEvent),
    GetAll(mpsc::SyncSender<Vec<TimelineEvent>>),
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
    pub fn get_all(&self) -> Vec<TimelineEvent> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        if self.sender.send(TimelineCommand::GetAll(reply_tx)).is_err() {
            return Vec::new();
        }
        if let Ok(events) = reply_rx.recv() {
            events
        } else {
            log::warn!("timeline: accumulate thread is gone; returning empty list");
            Vec::new()
        }
    }

    pub(crate) fn record_event(&self, event: TimelineEvent) {
        if self.sender.send(TimelineCommand::Record(event)).is_err() {
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

        let event = serde_json::to_value(value).and_then(serde_json::from_value);
        match event {
            Ok(event) => self.record_event(event),
            Err(e) => log::warn!("timeline: invalid event field: {e}"),
        }
    }

    fn flush(&self) {}
}

// ---------------------------------------------------------------------------
// Accumulate thread
// ---------------------------------------------------------------------------

fn accumulate(rx: &mpsc::Receiver<TimelineCommand>) {
    let mut store = Vec::new();

    while let Ok(cmd) = rx.recv() {
        match cmd {
            TimelineCommand::Record(event) => store.push(event),
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

    fn make_event(
        pkg: &str,
        phase: &str,
        duration_ns: u64,
        kind: TimelineEventKind,
    ) -> TimelineEvent {
        TimelineEvent {
            pkg: pkg.to_owned(),
            phase: phase.to_owned(),
            duration_ns,
            kind,
        }
    }

    #[test]
    fn accumulates_events() {
        let backend = TimelineBackend::new().unwrap();
        let started = make_event("pkg-a", "mount", 1_000_000, TimelineEventKind::Started);
        let completed = make_event("pkg-b", "setup", 2_000_000, TimelineEventKind::Completed);
        backend.record_event(started.clone());
        backend.record_event(completed.clone());

        assert_eq!(backend.get_all(), vec![started, completed]);
    }

    #[test]
    fn non_destructive_read() {
        let backend = TimelineBackend::new().unwrap();
        let event = make_event("pkg-c", "phase1", 500, TimelineEventKind::Started);
        backend.record_event(event.clone());

        assert_eq!(backend.get_all(), vec![event.clone()]);
        assert_eq!(backend.get_all(), vec![event]);
    }

    #[test]
    fn log_record_without_event_field_is_ignored() {
        let backend = TimelineBackend::new().unwrap();
        log::Log::log(
            &backend,
            &log::Record::builder()
                .level(log::Level::Info)
                .target(crate::TIMELINE_TARGET)
                .args(format_args!("plain message"))
                .build(),
        );

        assert!(backend.get_all().is_empty());
    }

    #[test]
    fn log_record_with_event_is_accepted() {
        let backend = TimelineBackend::new().unwrap();
        let event = serde_json::json!({
            "pkg": "pkg-kv",
            "phase": "phase",
            "duration_ns": 7,
            "kind": "started",
        });
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

        assert_eq!(
            backend.get_all(),
            vec![TimelineEvent {
                pkg: "pkg-kv".to_owned(),
                phase: "phase".to_owned(),
                duration_ns: 7,
                kind: TimelineEventKind::Started,
            }]
        );
    }

    #[test]
    fn record_event_after_thread_exit_does_not_panic() {
        let (tx, rx) = mpsc::channel::<TimelineCommand>();
        let handle = thread::Builder::new()
            .name("ssam-timeline-test".into())
            .spawn(move || drop(rx))
            .unwrap();
        handle.join().unwrap();
        let dead_backend = TimelineBackend {
            sender: tx,
            _thread: thread::Builder::new()
                .name("ssam-timeline-noop".into())
                .spawn(|| {})
                .unwrap(),
        };
        dead_backend.record_event(make_event("x", "y", 0, TimelineEventKind::Started));
    }
}
