// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use super::{ExecutionResult, PackageStatus};
use std::collections::HashMap;

/// Operations interface for package state transitions.
///
/// This trait defines the operations required by the state machine to perform
/// transitions between package states. Implementors provide the concrete
/// behavior for setup, cleanup, start, stop, and teardown operations.
///
/// # Relationship to Other Components
/// - Implemented by [`DefaultPackageTransitioner`](crate::package::DefaultPackageTransitioner)
/// - Used by [`Transition`] implementations to perform state changes
/// - Consumed by state machine actors for lifecycle orchestration
///
/// # Method Categories
///
/// **Lifecycle Operations** (async):
/// - `setup()`: Mount filesystem, prepare for execution
/// - `cleanup()`: Unmount filesystem after execution
/// - `start()`: Start OCI container
/// - `stop()`: Stop OCI container
/// - `teardown()`: Final cleanup (reserved for future use)
///
/// **Query Operations** (sync):
/// - `get_name()`: Package identifier
#[async_trait::async_trait]
pub(crate) trait PackageTransitioner: Send + Sync + std::fmt::Debug {
    async fn setup(&self) -> anyhow::Result<()>;
    async fn cleanup(&self) -> anyhow::Result<()>;
    async fn start(&self) -> anyhow::Result<ExecutionResult>;
    async fn stop(&self) -> anyhow::Result<ExecutionResult>;
    async fn teardown(&self) -> anyhow::Result<()>;

    fn get_name(&self) -> &str;
}

// Trait for handling state transitions
#[async_trait::async_trait]
pub(crate) trait TransitionHandler: std::fmt::Debug {
    /// Check if this transition is applicable from the given current state
    fn is_applicable(&self, current: &PackageStatus) -> bool;

    /// Whether to keep the current state without transition
    /// Default is false (perform the transition)
    fn ignore_without_error(&self, _current: &PackageStatus) -> bool {
        false
    }

    /// Handle the transition and return the resulting state
    /// Return Some if the transition was handled, None if the transition
    /// does not need to be handled (e.g., passthrough)
    /// Default is None
    async fn handle(
        &self,
        _current: &PackageStatus,
        _ops: &dyn PackageTransitioner,
    ) -> Option<PackageStatus> {
        None
    }

    /// Get a description of this transition for logging/debugging
    fn description(&self) -> &'static str;
}

#[derive(Debug)]
struct ToSettingUp;

#[derive(Debug)]
struct ToStartRequested;

#[derive(Debug)]
struct ToCleaningUp;

#[derive(Debug)]
struct ToStopRequested;

#[derive(Debug)]
struct ToReady;

#[derive(Debug)]
struct ToRunning;

#[derive(Debug)]
struct ToCleaned;

#[derive(Debug)]
struct ToError;

#[derive(Debug)]
struct ToBroken;

#[derive(Debug)]
struct ToUpgrading;

// ==== Transition Implementations ====
#[async_trait::async_trait]
impl TransitionHandler for ToSettingUp {
    fn is_applicable(&self, current: &PackageStatus) -> bool {
        matches!(current, PackageStatus::Verified | PackageStatus::Cleaned)
    }

    async fn handle(
        &self,
        _current: &PackageStatus,
        ops: &dyn PackageTransitioner,
    ) -> Option<PackageStatus> {
        let result = match ops.setup().await {
            Ok(()) => PackageStatus::Ready,
            Err(e) => {
                log::warn!("Package setup failed: {e}");
                if let Err(ce) = ops.cleanup().await {
                    log::warn!("Compensation cleanup after setup failure also failed: {ce:#}");
                }
                PackageStatus::Error(format!("{e:#}"))
            }
        };
        Some(result)
    }

    fn description(&self) -> &'static str {
        "Verified/Cleaned -> SettingUp (setup package)"
    }
}

#[async_trait::async_trait]
impl TransitionHandler for ToStartRequested {
    fn is_applicable(&self, current: &PackageStatus) -> bool {
        matches!(current, PackageStatus::Ready | PackageStatus::Running(_))
    }

    async fn handle(
        &self,
        _current: &PackageStatus,
        ops: &dyn PackageTransitioner,
    ) -> Option<PackageStatus> {
        let result = match ops.start().await {
            Ok(result) => result.into(),
            Err(e) => {
                log::warn!("Failed to start package: {e:#}");
                PackageStatus::Error(format!("{e:#}"))
            }
        };
        Some(result)
    }

    fn ignore_without_error(&self, current: &PackageStatus) -> bool {
        match current {
            PackageStatus::Running(state) => state != "failed",
            _ => false,
        }
    }

    fn description(&self) -> &'static str {
        "Ready/Running(failed) -> StartRequested (start package)"
    }
}

#[async_trait::async_trait]
impl TransitionHandler for ToCleaningUp {
    fn is_applicable(&self, current: &PackageStatus) -> bool {
        matches!(current, PackageStatus::Ready)
    }

    async fn handle(
        &self,
        _current: &PackageStatus,
        ops: &dyn PackageTransitioner,
    ) -> Option<PackageStatus> {
        let result = match ops.cleanup().await {
            Ok(()) => PackageStatus::Cleaned,
            Err(e) => {
                log::warn!("Failed to cleanup package: {e:#}");
                PackageStatus::Error(format!("{e:#}"))
            }
        };
        Some(result)
    }

    fn description(&self) -> &'static str {
        "Ready -> CleaningUp (cleanup package)"
    }
}

#[async_trait::async_trait]
impl TransitionHandler for ToStopRequested {
    fn is_applicable(&self, current: &PackageStatus) -> bool {
        matches!(
            current,
            PackageStatus::StartRequested | PackageStatus::Running(_) | PackageStatus::Ready
        )
    }

    async fn handle(
        &self,
        _current: &PackageStatus,
        ops: &dyn PackageTransitioner,
    ) -> Option<PackageStatus> {
        let result = match ops.stop().await {
            Ok(result) => result.into(),
            Err(e) => {
                log::warn!("Failed to stop package: {e:#}");
                PackageStatus::Error(format!("{e:#}"))
            }
        };
        Some(result)
    }

    fn ignore_without_error(&self, current: &PackageStatus) -> bool {
        current == &PackageStatus::Ready
    }

    fn description(&self) -> &'static str {
        "StartRequested/Running -> StopRequested (stop package)"
    }
}

#[async_trait::async_trait]
impl TransitionHandler for ToReady {
    fn is_applicable(&self, current: &PackageStatus) -> bool {
        matches!(
            current,
            PackageStatus::SettingUp | PackageStatus::StopRequested | PackageStatus::Running(_)
        )
    }

    fn description(&self) -> &'static str {
        "SettingUp/StopRequested -> Ready (setup/stop package)"
    }
}

#[async_trait::async_trait]
impl TransitionHandler for ToRunning {
    fn is_applicable(&self, current: &PackageStatus) -> bool {
        matches!(
            current,
            PackageStatus::Ready
                | PackageStatus::StartRequested
                | PackageStatus::Running(_)
                | PackageStatus::StopRequested
        )
    }

    fn description(&self) -> &'static str {
        "StartRequested -> Running (start package)"
    }
}

#[async_trait::async_trait]
impl TransitionHandler for ToCleaned {
    fn is_applicable(&self, current: &PackageStatus) -> bool {
        matches!(current, PackageStatus::CleaningUp)
    }

    fn description(&self) -> &'static str {
        "CleaningUp -> Cleaned (cleanup package)"
    }
}

#[async_trait::async_trait]
impl TransitionHandler for ToError {
    fn is_applicable(&self, _: &PackageStatus) -> bool {
        // can always transition to Error from any state
        true
    }

    fn description(&self) -> &'static str {
        "Any -> Error (error occurred)"
    }
}

#[async_trait::async_trait]
impl TransitionHandler for ToBroken {
    fn is_applicable(&self, _current: &PackageStatus) -> bool {
        // can always transition to Broken from any state
        true
    }

    fn description(&self) -> &'static str {
        "Any -> Broken (package broken)"
    }
}

#[async_trait::async_trait]
impl TransitionHandler for ToUpgrading {
    fn is_applicable(&self, current: &PackageStatus) -> bool {
        matches!(
            current,
            PackageStatus::Verified
                | PackageStatus::Ready
                | PackageStatus::Running(_)
                | PackageStatus::StartRequested
        )
    }

    async fn handle(
        &self,
        current: &PackageStatus,
        ops: &dyn PackageTransitioner,
    ) -> Option<PackageStatus> {
        if matches!(current, PackageStatus::Verified) {
            return None;
        }

        match ops.teardown().await {
            Ok(()) => None,
            Err(e) => {
                log::warn!("Package upgrade teardown failed: {e:#}");
                Some(PackageStatus::Error(format!("{e:#}")))
            }
        }
    }

    fn description(&self) -> &'static str {
        "Verified/Ready/Running/StartRequested -> Upgrading (stop + unmount for upgrade)"
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum TransitionTo {
    Verified,
    SettingUp,
    Ready,
    StartRequested,
    Running,
    StopRequested,
    CleaningUp,
    Cleaned,
    Upgrading,
    Error,
    Broken,
}

impl From<&PackageStatus> for &TransitionTo {
    fn from(status: &PackageStatus) -> Self {
        match status {
            PackageStatus::Verified => &TransitionTo::Verified,
            PackageStatus::SettingUp => &TransitionTo::SettingUp,
            PackageStatus::Ready => &TransitionTo::Ready,
            PackageStatus::StartRequested => &TransitionTo::StartRequested,
            PackageStatus::Running(_) => &TransitionTo::Running,
            PackageStatus::StopRequested => &TransitionTo::StopRequested,
            PackageStatus::CleaningUp => &TransitionTo::CleaningUp,
            PackageStatus::Cleaned => &TransitionTo::Cleaned,
            PackageStatus::Upgrading => &TransitionTo::Upgrading,
            PackageStatus::Error(_) => &TransitionTo::Error,
            PackageStatus::Broken(_) => &TransitionTo::Broken,
        }
    }
}

type TransitionMap = HashMap<TransitionTo, Box<dyn TransitionHandler + Send + Sync>>;

#[derive(Debug)]
// Transition store to manage all possible transitions
pub(crate) struct TransitionStore {
    transitions: TransitionMap,
}

impl TransitionStore {
    pub(crate) fn new() -> Self {
        let transitions = HashMap::from([
            (
                TransitionTo::SettingUp,
                Box::new(ToSettingUp) as Box<dyn TransitionHandler + Send + Sync>,
            ),
            (TransitionTo::StartRequested, Box::new(ToStartRequested)),
            (TransitionTo::CleaningUp, Box::new(ToCleaningUp)),
            (TransitionTo::StopRequested, Box::new(ToStopRequested)),
            (TransitionTo::Ready, Box::new(ToReady)),
            (TransitionTo::Cleaned, Box::new(ToCleaned)),
            (TransitionTo::Running, Box::new(ToRunning)),
            (TransitionTo::Broken, Box::new(ToBroken)),
            (TransitionTo::Error, Box::new(ToError)),
            (TransitionTo::Upgrading, Box::new(ToUpgrading)),
        ]);
        Self { transitions }
    }

    pub(crate) fn find_transition(
        &self,
        current: &PackageStatus,
        target: &PackageStatus,
    ) -> Option<&(dyn TransitionHandler + Send + Sync)> {
        // Look up transition by target status
        if let Some(transition) = self.transitions.get(target.into()) {
            // Check if the transition is applicable for the current state
            if transition.is_applicable(current) {
                return Some(transition.as_ref());
            }
        }
        None
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    mod transition_tests {
        use super::super::TransitionHandler;
        use super::*;
        use libssam::ssam_package::ssam_pkg_info::BrokenReason;

        impl From<TransitionTo> for PackageStatus {
            fn from(transition: TransitionTo) -> Self {
                match transition {
                    TransitionTo::Verified => PackageStatus::Verified,
                    TransitionTo::SettingUp => PackageStatus::SettingUp,
                    TransitionTo::Ready => PackageStatus::Ready,
                    TransitionTo::StartRequested => PackageStatus::StartRequested,
                    TransitionTo::Running => PackageStatus::Running("active".to_string()),
                    TransitionTo::StopRequested => PackageStatus::StopRequested,
                    TransitionTo::CleaningUp => PackageStatus::CleaningUp,
                    TransitionTo::Cleaned => PackageStatus::Cleaned,
                    TransitionTo::Upgrading => PackageStatus::Upgrading,
                    TransitionTo::Error => PackageStatus::Error("unknown".to_string()),
                    TransitionTo::Broken => {
                        PackageStatus::Broken(BrokenReason::new("unknown", "unknown"))
                    }
                }
            }
        }

        // Helper functions for testing transitions
        fn is_transition_valid(
            transition_store: &TransitionStore,
            current: &PackageStatus,
            target: &PackageStatus,
        ) -> bool {
            transition_store.find_transition(current, target).is_some()
        }

        fn get_possible_transitions(
            transition_store: &TransitionStore,
            current: &PackageStatus,
        ) -> Vec<PackageStatus> {
            let mut targets = Vec::new();
            for (target_status, transition) in &transition_store.transitions {
                if transition.is_applicable(current) {
                    targets.push((*target_status).clone().into());
                }
            }
            targets
        }

        fn get_transition_description(
            transition_store: &TransitionStore,
            current: &PackageStatus,
            target: &PackageStatus,
        ) -> Option<&'static str> {
            transition_store
                .find_transition(current, target)
                .map(TransitionHandler::description)
        }

        #[test]
        fn test_transition_store_creation() {
            let store = TransitionStore::new();

            // Test basic transitions
            assert!(is_transition_valid(
                &store,
                &PackageStatus::Verified,
                &PackageStatus::SettingUp
            ));
            assert!(is_transition_valid(
                &store,
                &PackageStatus::Ready,
                &PackageStatus::StartRequested
            ));
            assert!(is_transition_valid(
                &store,
                &PackageStatus::Ready,
                &PackageStatus::CleaningUp
            ));

            // Test invalid transitions
            assert!(!is_transition_valid(
                &store,
                &PackageStatus::Verified,
                &PackageStatus::Running("active".to_string())
            ));
            assert!(!is_transition_valid(
                &store,
                &PackageStatus::Cleaned,
                &PackageStatus::Running("active".to_string())
            ));
        }

        #[test]
        fn test_get_possible_transitions() {
            let transition_store = TransitionStore::new();

            // Test possible transitions from Ready state
            let ready_transitions =
                get_possible_transitions(&transition_store, &PackageStatus::Ready);
            assert!(ready_transitions.contains(&PackageStatus::StartRequested));
            assert!(ready_transitions.contains(&PackageStatus::CleaningUp));

            // Test possible transitions from Verified state
            let verified_transitions =
                get_possible_transitions(&transition_store, &PackageStatus::Verified);
            assert!(verified_transitions.contains(&PackageStatus::SettingUp));
            assert!(!verified_transitions.contains(&PackageStatus::StartRequested));
        }

        #[test]
        fn test_transition_descriptions() {
            let transition_store = TransitionStore::new();

            // Test getting descriptions for valid transitions
            let desc = get_transition_description(
                &transition_store,
                &PackageStatus::Verified,
                &PackageStatus::SettingUp,
            );
            assert!(desc.is_some());
            assert_eq!(
                desc.unwrap(),
                "Verified/Cleaned -> SettingUp (setup package)"
            );

            let desc = get_transition_description(
                &transition_store,
                &PackageStatus::Ready,
                &PackageStatus::StartRequested,
            );
            assert!(desc.is_some());
            assert_eq!(
                desc.unwrap(),
                "Ready/Running(failed) -> StartRequested (start package)"
            );

            // Test getting descriptions for invalid transitions
            let desc = get_transition_description(
                &transition_store,
                &PackageStatus::Verified,
                &PackageStatus::Running("active".to_string()),
            );
            assert!(desc.is_none());
        }

        #[test]
        fn test_special_running_state_transitions() {
            let transition_store = TransitionStore::new();

            // Test restart failed transition
            assert!(is_transition_valid(
                &transition_store,
                &PackageStatus::Running("failed".to_string()),
                &PackageStatus::StartRequested
            ));

            // Test that Running("active") -> StartRequested is valid
            // Only failed running states can transition to StartRequested
            assert!(is_transition_valid(
                &transition_store,
                &PackageStatus::Running("active".to_string()),
                &PackageStatus::StartRequested
            ));

            // Test stop transitions - these should be valid
            assert!(is_transition_valid(
                &transition_store,
                &PackageStatus::Running("active".to_string()),
                &PackageStatus::StopRequested
            ));
            assert!(is_transition_valid(
                &transition_store,
                &PackageStatus::Running("failed".to_string()),
                &PackageStatus::StopRequested
            ));
        }

        #[test]
        fn test_ignore_functionality() {
            let transition_store = TransitionStore::new();

            // Test keep_current behavior for StartRequested transition
            // When already in Running("failed") state, should keep current state
            let transition = transition_store
                .find_transition(
                    &PackageStatus::Running("failed".to_string()),
                    &PackageStatus::StartRequested,
                )
                .expect("Transition from Running('failed') to StartRequested should exist");
            assert!(transition.ignore_without_error(&PackageStatus::Running("active".to_string())));
            assert!(!transition.ignore_without_error(&PackageStatus::Ready));
            assert!(
                !transition.ignore_without_error(&PackageStatus::Running("failed".to_string()))
            );

            // Test keep_current behavior for StopRequested transition
            // When already in Ready state, should keep current state
            let transition = transition_store
                .find_transition(&PackageStatus::Ready, &PackageStatus::StopRequested)
                .expect("Transition from Ready to StopRequested should exist");
            assert!(transition.ignore_without_error(&PackageStatus::Ready));
            assert!(
                !transition.ignore_without_error(&PackageStatus::Running("active".to_string()))
            );
        }

        #[test]
        fn test_transition_handle_implementation() {
            let transition_store = TransitionStore::new();

            // Create longer-lived values to avoid temporary value issues
            let running_active = PackageStatus::Running("active".to_string());
            let error_test = PackageStatus::Error("test".to_string());
            let broken_test = PackageStatus::Broken(BrokenReason::new("test", "test"));
            // Test that certain transitions exist and can be found
            // These transitions have handle implementations (return Some)
            let implemented_transitions = vec![
                (&PackageStatus::Verified, &PackageStatus::SettingUp),
                (&PackageStatus::Ready, &PackageStatus::StartRequested),
                (&PackageStatus::Ready, &PackageStatus::CleaningUp),
                (
                    &PackageStatus::StartRequested,
                    &PackageStatus::StopRequested,
                ),
            ];

            for (from, to) in implemented_transitions {
                let transition = transition_store.find_transition(from, to);
                assert!(
                    transition.is_some(),
                    "Transition from {from} to {to} should exist and have handle implementation"
                );
            }

            // Test that passthrough transitions exist (these use default handle which returns None)
            let passthrough_transitions = vec![
                (&PackageStatus::SettingUp, &PackageStatus::Ready),
                (&PackageStatus::StopRequested, &PackageStatus::Ready),
                (&PackageStatus::StartRequested, &running_active),
                (&PackageStatus::Ready, &running_active),
                (&PackageStatus::Verified, &broken_test),
                (&PackageStatus::Ready, &error_test),
            ];

            for (from, to) in passthrough_transitions {
                assert!(
                    transition_store.find_transition(from, to).is_some(),
                    "Passthrough transition from {from} to {to} should exist"
                );
            }
        }

        #[test]
        fn test_transition_applicability() {
            let transition_store = TransitionStore::new();

            // Test that transitions are only applicable for correct states

            // SettingUp transition should only be applicable from Verified or Cleaned
            let setup_transition = transition_store
                .find_transition(&PackageStatus::Verified, &PackageStatus::SettingUp)
                .expect("Setup transition should exist");

            assert!(setup_transition.is_applicable(&PackageStatus::Verified));
            assert!(setup_transition.is_applicable(&PackageStatus::Cleaned));
            assert!(!setup_transition.is_applicable(&PackageStatus::Ready));
            assert!(!setup_transition.is_applicable(&PackageStatus::Running("active".to_string())));

            // StartRequested transition should be applicable from Ready and failed Running states
            let start_transition = transition_store
                .find_transition(&PackageStatus::Ready, &PackageStatus::StartRequested)
                .expect("Start transition should exist");

            assert!(start_transition.is_applicable(&PackageStatus::Ready));
            assert!(start_transition.is_applicable(&PackageStatus::Running("failed".to_string())));
            assert!(start_transition.is_applicable(&PackageStatus::Running("active".to_string())));
            assert!(!start_transition.is_applicable(&PackageStatus::Verified));

            // CleaningUp transition should only be applicable from Ready
            let cleanup_transition = transition_store
                .find_transition(&PackageStatus::Ready, &PackageStatus::CleaningUp)
                .expect("Cleanup transition should exist");

            assert!(cleanup_transition.is_applicable(&PackageStatus::Ready));
            assert!(
                !cleanup_transition.is_applicable(&PackageStatus::Running("active".to_string()))
            );
            assert!(!cleanup_transition.is_applicable(&PackageStatus::Verified));

            // StopRequested transition should be applicable from StartRequested and Running states
            let stop_transition = transition_store
                .find_transition(
                    &PackageStatus::StartRequested,
                    &PackageStatus::StopRequested,
                )
                .expect("Stop transition should exist");

            assert!(stop_transition.is_applicable(&PackageStatus::StartRequested));
            assert!(stop_transition.is_applicable(&PackageStatus::Running("active".to_string())));
            assert!(stop_transition.is_applicable(&PackageStatus::Running("failed".to_string())));
            assert!(stop_transition.is_applicable(&PackageStatus::Ready));
            assert!(!stop_transition.is_applicable(&PackageStatus::Verified));
        }

        #[test]
        fn test_upgrading_transition_applicability() {
            let transition_store = TransitionStore::new();

            let upgrading_transition = transition_store
                .find_transition(&PackageStatus::Verified, &PackageStatus::Upgrading)
                .expect("Transition from Verified to Upgrading should exist");

            assert!(upgrading_transition.is_applicable(&PackageStatus::Verified));
            assert!(upgrading_transition.is_applicable(&PackageStatus::Ready));
            assert!(
                upgrading_transition.is_applicable(&PackageStatus::Running("active".to_string()))
            );
            assert!(upgrading_transition.is_applicable(&PackageStatus::StartRequested));

            assert!(!upgrading_transition.is_applicable(&PackageStatus::Cleaned));
            assert!(!upgrading_transition.is_applicable(&PackageStatus::StopRequested));
            assert!(!upgrading_transition.is_applicable(&PackageStatus::SettingUp));
            assert!(!upgrading_transition.is_applicable(&PackageStatus::CleaningUp));
        }
    }
}
