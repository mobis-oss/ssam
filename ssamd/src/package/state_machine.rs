// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use anyhow::Context as _;

use super::transition::{PackageTransitioner, TransitionStore};
use super::{InvalidTransition, PackageStatus};
use crate::executor::{ExecutionResult, ExecutionStatus};

pub(in crate::package) mod transition_messages {
    #[derive(Debug)]
    pub(in crate::package) struct RequestTransition {
        pub(in crate::package) target: super::PackageStatus,
    }

    #[derive(Debug)]
    pub(in crate::package) struct Teardown;

    #[derive(Debug)]
    pub(in crate::package) struct GetStatus;
}

#[derive(Debug, rsactor::Actor)]
pub(in crate::package) struct TransitionManagerActor {
    ops: Box<dyn PackageTransitioner + Send + Sync>,
    pub(in crate::package) status: PackageStatus,
    transition_store: TransitionStore,
}

#[rsactor::message_handlers]
impl TransitionManagerActor {
    #[handler]
    async fn handle_request_transition(
        &mut self,
        msg: transition_messages::RequestTransition,
        _actor_ref: &rsactor::ActorRef<TransitionManagerActor>,
    ) -> anyhow::Result<()> {
        self.transition_to(msg.target).await
    }

    #[handler]
    async fn handle_teardown(
        &mut self,
        _msg: transition_messages::Teardown,
        _actor_ref: &rsactor::ActorRef<TransitionManagerActor>,
    ) -> anyhow::Result<()> {
        self.ops.teardown().await
    }

    #[handler]
    // rsactor #[handler] requires async fn signature even without await
    #[allow(clippy::unused_async)]
    async fn handle_get_status(
        &mut self,
        _msg: transition_messages::GetStatus,
        _actor_ref: &rsactor::ActorRef<TransitionManagerActor>,
    ) -> PackageStatus {
        self.status.clone()
    }
}

impl TransitionManagerActor {
    pub(in crate::package) fn new(ops: Box<dyn PackageTransitioner + Send + Sync>) -> Self {
        let transition_store = TransitionStore::new();
        let status = PackageStatus::Verified;

        Self {
            ops,
            status,
            transition_store,
        }
    }

    pub(in crate::package) async fn transition_to(
        &mut self,
        next: PackageStatus,
    ) -> anyhow::Result<()> {
        let current = self.status.clone();
        if next == current {
            log::debug!("No transition needed as the status is already {next}");
            return Ok(());
        }

        let pkg_name = self.ops.get_name();

        let next = {
            if let Some(transition) = self.transition_store.find_transition(&current, &next) {
                // Check if this is an ignore transition (keep current state)
                if transition.ignore_without_error(&current) {
                    log::debug!(
                        "Ignoring transition from {current} to {next} for package {pkg_name}"
                    );
                    current
                } else {
                    log::debug!("Set status to {next} for package {pkg_name}");
                    self.status = next.clone();

                    log::debug!(
                        "Executing transition: {} for package {pkg_name}",
                        transition.description(),
                    );
                    transition
                        .handle(&current, &*self.ops)
                        .await
                        .unwrap_or(next)
                }
            } else {
                anyhow::bail!(InvalidTransition {
                    from: current,
                    to: next,
                });
            }
        };

        let result = if let PackageStatus::Error(_) = next {
            log::error!("Package {pkg_name} got into error state: {next}");
            Err(anyhow::anyhow!(
                "Package {pkg_name} got into error state: {next}"
            ))
        } else {
            Ok(())
        };

        log::debug!("Transition finished for package {pkg_name}, current status is now {next}");
        self.status = next;
        result
    }
}

impl From<ExecutionResult> for PackageStatus {
    fn from(state: ExecutionResult) -> Self {
        match state {
            ExecutionResult::Success(state) | ExecutionResult::Canceled(state) => match state {
                ExecutionStatus::Active(state) => PackageStatus::Running(state.to_string()),
                ExecutionStatus::Inactive(_) => PackageStatus::Ready,
            },
            ExecutionResult::Failure(reason) => PackageStatus::Error(reason),
            ExecutionResult::Timeout => PackageStatus::Error("timeout".to_string()),
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct TransitionManager {
    transition_mgr: rsactor::ActorRef<TransitionManagerActor>,
}

impl TransitionManager {
    pub(super) fn new(ops: Box<dyn PackageTransitioner + Send + Sync + 'static>) -> Self {
        let (transition_mgr, _) =
            rsactor::spawn::<TransitionManagerActor>(TransitionManagerActor::new(ops));
        Self { transition_mgr }
    }

    pub(super) async fn transition_to(&self, target: PackageStatus) -> anyhow::Result<()> {
        let msg: transition_messages::RequestTransition =
            transition_messages::RequestTransition { target };

        self.transition_mgr
            .ask(msg)
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
        let msg = transition_messages::Teardown;

        let response = self
            .transition_mgr
            .ask(msg)
            .await
            .context("TransitionManager actor might have been stopped.")?;
        response.context("Teardown request has failed")
    }

    pub(super) async fn get_status(&self) -> anyhow::Result<PackageStatus> {
        let msg = transition_messages::GetStatus;

        self.transition_mgr
            .ask(msg)
            .await
            .context("TransitionManager actor might have been stopped.")
    }
}
