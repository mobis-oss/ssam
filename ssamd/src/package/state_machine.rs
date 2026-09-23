// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use anyhow::Context as _;
use async_trait::async_trait;

use super::{InvalidTransition, PackageStatus, is_wipeable_status};

#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub(super) trait LifecycleBackend: Send + Sync + std::fmt::Debug {
    fn name(&self) -> &str;
    async fn provision(&mut self) -> anyhow::Result<()>;
    async fn start(&self) -> PackageStatus;
    async fn stop(&self) -> PackageStatus;
    async fn teardown(&mut self) -> anyhow::Result<()>;
    async fn wipe_data(&self) -> anyhow::Result<()>;
}

enum TransitionResult {
    Proceeded(PackageStatus),
    Ignored,
    Invalid,
}

pub(in crate::package) mod transition_messages {
    use super::PackageStatus;

    pub(in crate::package) struct RequestTransition {
        pub(in crate::package) target: PackageStatus,
    }

    pub(in crate::package) struct Teardown;

    pub(in crate::package) struct GetStatus;

    pub(in crate::package) struct WipeData;
}

#[derive(Debug, Clone)]
pub(super) struct TransitionManager {
    transition_mgr: rsactor::ActorRef<TransitionManagerActor>,
}

impl TransitionManager {
    pub(super) fn new(backend: impl LifecycleBackend + 'static) -> Self {
        let (transition_mgr, _) =
            rsactor::spawn::<TransitionManagerActor>(TransitionManagerActor::new(backend));
        Self { transition_mgr }
    }

    pub(super) async fn transition_to(&self, target: PackageStatus) -> anyhow::Result<()> {
        self.transition_mgr
            .ask(transition_messages::RequestTransition { target })
            .await
            .context("TransitionManager actor might have been stopped.")?
    }

    pub(super) async fn request_setup(&self) -> anyhow::Result<()> {
        self.transition_to(PackageStatus::SettingUp)
            .await
            .context("Setup request has failed")
    }

    pub(super) async fn request_cleanup(&self) -> anyhow::Result<()> {
        self.transition_to(PackageStatus::CleaningUp)
            .await
            .context("Cleanup request has failed")
    }

    pub(super) async fn request_start(&self) -> anyhow::Result<()> {
        self.transition_to(PackageStatus::StartRequested)
            .await
            .context("Start request has failed")
    }

    pub(super) async fn request_stop(&self) -> anyhow::Result<()> {
        self.transition_to(PackageStatus::StopRequested)
            .await
            .context("Stop request has failed")
    }

    pub(super) async fn prepare_upgrade(&self) -> anyhow::Result<()> {
        self.transition_to(PackageStatus::Upgrading)
            .await
            .context("Upgrade request has failed")
    }

    pub(super) async fn teardown(&self) -> anyhow::Result<()> {
        self.transition_mgr
            .ask(transition_messages::Teardown)
            .await
            .context("TransitionManager actor might have been stopped.")?
            .context("Teardown request has failed")
    }

    pub(super) async fn get_status(&self) -> anyhow::Result<PackageStatus> {
        self.transition_mgr
            .ask(transition_messages::GetStatus)
            .await
            .context("TransitionManager actor might have been stopped.")
    }

    pub(super) async fn wipe_data(&self) -> anyhow::Result<()> {
        self.transition_mgr
            .ask(transition_messages::WipeData)
            .await
            .context("TransitionManager actor might have been stopped.")?
    }
}

#[derive(Debug, rsactor::Actor)]
pub(in crate::package) struct TransitionManagerActor {
    status: PackageStatus,
    backend: Box<dyn LifecycleBackend>,
}

impl TransitionManagerActor {
    pub(in crate::package) fn new(backend: impl LifecycleBackend + 'static) -> Self {
        Self {
            status: PackageStatus::Verified,
            backend: Box::new(backend),
        }
    }

    async fn transition_to(&mut self, target: PackageStatus) -> anyhow::Result<()> {
        if target == self.status {
            log::debug!("No transition needed as the status is already {target}");
            return Ok(());
        }

        let name = self.backend.name().to_owned();
        match self.try_transition(&target).await {
            TransitionResult::Ignored => {
                log::debug!(
                    "Ignoring transition from {} to {target} for package {name}",
                    self.status
                );
                Ok(())
            }
            TransitionResult::Invalid => Err(anyhow::Error::new(InvalidTransition {
                from: self.status.clone(),
                to: target,
            })),
            TransitionResult::Proceeded(next) => {
                self.status = next;
                match &self.status {
                    status @ PackageStatus::Error(_) => {
                        let error =
                            anyhow::anyhow!("Package {name} got into error state: {status}");
                        log::error!("{error}");
                        Err(error)
                    }
                    status => {
                        log::debug!(
                            "Transition finished for package {name}, current status is now {status}"
                        );
                        Ok(())
                    }
                }
            }
        }
    }

    /// Keep the match on `target` wildcard-free so a new `PackageStatus` variant
    /// fails to compile until it is handled.
    async fn try_transition(&mut self, target: &PackageStatus) -> TransitionResult {
        use PackageStatus as S;
        use TransitionResult::{Ignored, Invalid, Proceeded};

        let current = &self.status;

        match target {
            S::SettingUp => match current {
                S::Verified | S::Cleaned => Proceeded(match self.backend.provision().await {
                    Ok(()) => S::Ready,
                    Err(error) => S::Error(format!("{error:#}")),
                }),
                _ => Invalid,
            },

            S::StartRequested => match current {
                S::Ready => Proceeded(self.backend.start().await),
                S::Running(state) if state == "failed" => Proceeded(self.backend.start().await),
                S::Running(_) => Ignored,
                _ => Invalid,
            },

            S::StopRequested => match current {
                S::Ready => Ignored,
                S::Running(_) => Proceeded(self.backend.stop().await),
                _ => Invalid,
            },

            S::CleaningUp => match current {
                S::Ready => Proceeded(self.release(S::Cleaned).await),
                _ => Invalid,
            },

            S::Upgrading => match current {
                S::Verified => Proceeded(S::Upgrading),
                S::Ready | S::Running(_) => Proceeded(self.release(S::Upgrading).await),
                _ => Invalid,
            },

            S::Running(state) => match current {
                S::Ready | S::Running(_) => Proceeded(S::Running(state.clone())),
                _ => Invalid,
            },

            S::Ready => match current {
                S::Running(_) => Proceeded(S::Ready),
                _ => Invalid,
            },

            // Forced error from any status; the backend keeps its runtime for teardown.
            S::Error(reason) => Proceeded(S::Error(reason.clone())),

            // Never requested: `Verified` is the origin, `Cleaned` follows `CleaningUp`,
            // and `Broken` is synthesized by the package manager.
            S::Verified | S::Cleaned | S::Broken(_) => Invalid,
        }
    }

    /// Tear down; on failure the backend keeps the runtime so teardown can retry.
    async fn release(&mut self, released: PackageStatus) -> PackageStatus {
        match self.backend.teardown().await {
            Ok(()) => released,
            Err(error) => PackageStatus::Error(format!("{error:#}")),
        }
    }
}

#[rsactor::message_handlers]
impl TransitionManagerActor {
    #[handler]
    async fn handle_request_transition(
        &mut self,
        msg: transition_messages::RequestTransition,
        _actor_ref: &rsactor::ActorRef<Self>,
    ) -> anyhow::Result<()> {
        self.transition_to(msg.target).await
    }

    #[handler]
    async fn handle_teardown(
        &mut self,
        _msg: transition_messages::Teardown,
        _actor_ref: &rsactor::ActorRef<Self>,
    ) -> anyhow::Result<()> {
        self.backend.teardown().await
    }

    #[handler]
    // rsactor #[handler] requires async fn signature even without await
    #[allow(clippy::unused_async)]
    async fn handle_get_status(
        &mut self,
        _msg: transition_messages::GetStatus,
        _actor_ref: &rsactor::ActorRef<Self>,
    ) -> PackageStatus {
        self.status.clone()
    }

    #[handler]
    async fn handle_wipe_data(
        &mut self,
        _msg: transition_messages::WipeData,
        _actor_ref: &rsactor::ActorRef<Self>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            is_wipeable_status(&self.status),
            "Package '{}' is not in a wipeable state (current: {}); \
             stop it before wiping data",
            self.backend.name(),
            self.status,
        );
        self.backend
            .wipe_data()
            .await
            .context("Failed to wipe package data")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backend() -> MockLifecycleBackend {
        let mut backend = MockLifecycleBackend::new();
        backend.expect_name().return_const(String::new());
        backend
    }

    fn provisions(backend: &mut MockLifecycleBackend, times: usize) {
        backend.expect_provision().times(times).returning(|| Ok(()));
    }

    fn actor(backend: MockLifecycleBackend) -> TransitionManagerActor {
        TransitionManagerActor::new(backend)
    }

    #[tokio::test]
    async fn setup_start_stop_cleanup_flow() {
        let mut backend = backend();
        provisions(&mut backend, 1);
        backend
            .expect_start()
            .times(1)
            .returning(|| PackageStatus::Running("running".to_owned()));
        backend
            .expect_stop()
            .times(1)
            .returning(|| PackageStatus::Ready);
        backend.expect_teardown().times(1).returning(|| Ok(()));
        let mut actor = actor(backend);

        actor
            .transition_to(PackageStatus::SettingUp)
            .await
            .expect("setup");
        assert_eq!(actor.status, PackageStatus::Ready);

        actor
            .transition_to(PackageStatus::StartRequested)
            .await
            .expect("start");
        assert!(matches!(actor.status, PackageStatus::Running(_)));

        actor
            .transition_to(PackageStatus::StopRequested)
            .await
            .expect("stop");
        assert_eq!(actor.status, PackageStatus::Ready);

        actor
            .transition_to(PackageStatus::CleaningUp)
            .await
            .expect("cleanup");
        assert_eq!(actor.status, PackageStatus::Cleaned);
    }

    #[tokio::test]
    async fn setup_failure_sets_error() {
        let mut backend = backend();
        backend
            .expect_provision()
            .times(1)
            .returning(|| Err(anyhow::anyhow!("Setup failed")));
        let mut actor = actor(backend);

        assert!(actor.transition_to(PackageStatus::SettingUp).await.is_err());
        assert!(matches!(actor.status, PackageStatus::Error(_)));
    }

    #[tokio::test]
    async fn command_failures_report_error() {
        let mut backend = backend();
        provisions(&mut backend, 1);
        backend
            .expect_start()
            .times(1)
            .returning(|| PackageStatus::Error("Start failed".to_owned()));
        let mut start_actor = actor(backend);
        start_actor
            .transition_to(PackageStatus::SettingUp)
            .await
            .expect("setup");
        assert!(
            start_actor
                .transition_to(PackageStatus::StartRequested)
                .await
                .is_err()
        );
        assert_eq!(
            start_actor.status,
            PackageStatus::Error("Start failed".to_owned())
        );

        let mut backend = self::backend();
        provisions(&mut backend, 1);
        backend
            .expect_teardown()
            .times(1)
            .returning(|| Err(anyhow::anyhow!("Teardown failed")));
        let mut cleanup_actor = actor(backend);
        cleanup_actor
            .transition_to(PackageStatus::SettingUp)
            .await
            .expect("setup");
        assert!(
            cleanup_actor
                .transition_to(PackageStatus::CleaningUp)
                .await
                .is_err()
        );
        assert!(matches!(cleanup_actor.status, PackageStatus::Error(_)));
    }

    #[tokio::test]
    async fn actor_messages_preserve_lifecycle_api() {
        let mut backend = backend();
        provisions(&mut backend, 1);
        backend.expect_teardown().times(1).returning(|| Ok(()));
        let (actor_ref, _) = rsactor::spawn::<TransitionManagerActor>(actor(backend));

        assert_eq!(
            actor_ref
                .ask(transition_messages::GetStatus)
                .await
                .expect("status"),
            PackageStatus::Verified
        );
        actor_ref
            .ask(transition_messages::RequestTransition {
                target: PackageStatus::SettingUp,
            })
            .await
            .expect("request")
            .expect("setup");
        assert_eq!(
            actor_ref
                .ask(transition_messages::GetStatus)
                .await
                .expect("status"),
            PackageStatus::Ready
        );
        actor_ref
            .ask(transition_messages::Teardown)
            .await
            .expect("teardown")
            .expect("cleanup");
    }

    #[tokio::test]
    async fn wipe_data_is_serialized_by_status() {
        let mut backend = backend();
        provisions(&mut backend, 1);
        backend
            .expect_start()
            .times(1)
            .returning(|| PackageStatus::Running("running".to_owned()));
        backend.expect_wipe_data().times(1).returning(|| Ok(()));
        let manager = TransitionManager::new(backend);
        manager.wipe_data().await.expect("verified is wipeable");
        manager.request_setup().await.expect("setup");
        manager.request_start().await.expect("start");
        assert!(manager.wipe_data().await.is_err());
    }

    #[tokio::test]
    async fn invalid_and_ignored_transitions_keep_status() {
        let mut backend = backend();
        provisions(&mut backend, 1);
        backend.expect_stop().never();
        let mut actor = actor(backend);
        assert!(
            actor
                .transition_to(PackageStatus::Running("active".to_owned()))
                .await
                .is_err()
        );
        assert_eq!(actor.status, PackageStatus::Verified);

        actor
            .transition_to(PackageStatus::SettingUp)
            .await
            .expect("setup");
        actor
            .transition_to(PackageStatus::StopRequested)
            .await
            .expect("stop from ready ignored");
        assert_eq!(actor.status, PackageStatus::Ready);
    }

    #[tokio::test]
    async fn upgrade_releases_resources_and_verified_upgrade_is_passthrough() {
        let mut verified = actor(backend());
        verified
            .transition_to(PackageStatus::Upgrading)
            .await
            .expect("verified upgrade");
        assert_eq!(verified.status, PackageStatus::Upgrading);

        let mut backend = backend();
        provisions(&mut backend, 1);
        backend.expect_teardown().times(1).returning(|| Ok(()));
        let mut ready = actor(backend);
        ready
            .transition_to(PackageStatus::SettingUp)
            .await
            .expect("setup");
        ready
            .transition_to(PackageStatus::Upgrading)
            .await
            .expect("ready upgrade");
        assert_eq!(ready.status, PackageStatus::Upgrading);
    }

    #[tokio::test]
    async fn start_restarts_failed_unit_and_ignores_active_one() {
        let mut backend = backend();
        provisions(&mut backend, 1);
        backend
            .expect_start()
            .times(1)
            .returning(|| PackageStatus::Running("running".to_owned()));
        let mut actor = actor(backend);
        actor
            .transition_to(PackageStatus::SettingUp)
            .await
            .expect("setup");

        actor
            .transition_to(PackageStatus::Running("active".to_owned()))
            .await
            .expect("monitor active");
        actor
            .transition_to(PackageStatus::StartRequested)
            .await
            .expect("start while active is ignored");
        assert_eq!(actor.status, PackageStatus::Running("active".to_owned()));

        actor
            .transition_to(PackageStatus::Running("failed".to_owned()))
            .await
            .expect("monitor failed");
        actor
            .transition_to(PackageStatus::StartRequested)
            .await
            .expect("restart failed unit");
        assert_eq!(actor.status, PackageStatus::Running("running".to_owned()));
    }

    #[tokio::test]
    async fn cleaned_package_can_be_set_up_again_but_not_upgraded() {
        let mut backend = backend();
        provisions(&mut backend, 2);
        backend.expect_teardown().times(1).returning(|| Ok(()));
        let mut actor = actor(backend);
        actor
            .transition_to(PackageStatus::SettingUp)
            .await
            .expect("setup");
        actor
            .transition_to(PackageStatus::CleaningUp)
            .await
            .expect("cleanup");

        assert!(actor.transition_to(PackageStatus::Upgrading).await.is_err());
        assert_eq!(actor.status, PackageStatus::Cleaned);

        actor
            .transition_to(PackageStatus::SettingUp)
            .await
            .expect("setup again");
        assert_eq!(actor.status, PackageStatus::Ready);
    }
}
