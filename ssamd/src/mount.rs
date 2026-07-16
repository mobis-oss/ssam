// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use crate::configuration;
#[cfg(feature = "dm-verity")]
use crate::dm::VerityDevice;
use crate::package_volume::PackageFsMetadata;

use crate::utils::actor_supervisor::{SupervisedActor, spawn_with};
use anyhow::Context;
use derive_more::Deref;
use rsactor::{Actor, ActorRef, message_handlers};
use rustix::mount::{MountFlags, UnmountFlags};
use std::ffi::CString;
use std::os::fd::{AsFd, BorrowedFd};
use std::{
    io,
    path::Path,
    thread::sleep,
    time::{Duration, Instant},
};

pub(crate) mod loopdev_message {
    use std::path::PathBuf;

    pub(crate) struct AttachInfo {
        pub(crate) backing_file: PathBuf,
        pub(crate) offset: u64,
        pub(crate) length: u64,
    }

    pub(crate) struct MsgAttach {
        pub(crate) attach_info: AttachInfo,
    }
}

mod loop_attach {
    use linux_raw_sys::loop_device::{
        self, LOOP_CONFIGURE, loop_config as LoopConfig, loop_info64 as LoopInfo64,
    };
    use rustix::ioctl::{Setter, ioctl};
    use std::io;
    use std::os::fd::{AsRawFd, BorrowedFd};

    const LO_FLAGS_READ_ONLY: u32 = loop_device::LO_FLAGS_READ_ONLY as u32;

    trait LoopDeviceConfigurer<LoopDev> {
        fn loop_configure(&self, dev: &LoopDev, fd: u32, info: LoopInfo64) -> io::Result<()>;
    }

    struct DefaultLoopDeviceConfigurer;

    impl LoopDeviceConfigurer<loopdev::LoopDevice> for DefaultLoopDeviceConfigurer {
        fn loop_configure(
            &self,
            dev: &loopdev::LoopDevice,
            fd: u32,
            info: LoopInfo64,
        ) -> io::Result<()> {
            loop_configure(dev, fd, info)
        }
    }

    fn loop_configure(dev: &loopdev::LoopDevice, fd: u32, info: LoopInfo64) -> io::Result<()> {
        let config = LoopConfig {
            fd,
            block_size: 0,
            info,
            __reserved: [0; 8],
        };

        let raw_fd = dev.as_raw_fd();
        // SAFETY: raw_fd is valid for the duration of the ioctl call
        let borrowed_fd = unsafe { std::os::fd::BorrowedFd::borrow_raw(raw_fd) };
        // SAFETY: LoopConfig is a valid ioctl input structure for LOOP_CONFIGURE
        let setter = unsafe { Setter::<LOOP_CONFIGURE, LoopConfig>::new(config) };
        let result = unsafe { ioctl(borrowed_fd, setter) };
        result.map_err(|e| io::Error::from_raw_os_error(e.raw_os_error()))
    }

    fn default_loop_info64() -> LoopInfo64 {
        LoopInfo64 {
            lo_device: 0,
            lo_inode: 0,
            lo_rdevice: 0,
            lo_offset: 0,
            lo_sizelimit: 0,
            lo_number: 0,
            lo_encrypt_type: 0,
            lo_encrypt_key_size: 0,
            lo_flags: 0,
            lo_file_name: [0; 64],
            lo_crypt_name: [0; 64],
            lo_encrypt_key: [0; 32],
            lo_init: [0; 2],
        }
    }

    fn attach_loop_dev_configure_with<LoopDev, Configurer>(
        dev: &LoopDev,
        backing_file_fd: BorrowedFd<'_>,
        offset: u64,
        length: u64,
        ops: &Configurer,
    ) -> io::Result<()>
    where
        Configurer: LoopDeviceConfigurer<LoopDev>,
    {
        let info = LoopInfo64 {
            lo_offset: offset,
            lo_sizelimit: length,
            lo_flags: LO_FLAGS_READ_ONLY,
            ..default_loop_info64()
        };

        let fd =
            u32::try_from(backing_file_fd.as_raw_fd()).expect("Opened fd is always non-negative");

        ops.loop_configure(dev, fd, info)
    }

    pub(super) fn attach_loop_dev_configure(
        dev: &loopdev::LoopDevice,
        backing_file_fd: BorrowedFd<'_>,
        offset: u64,
        length: u64,
    ) -> io::Result<()> {
        attach_loop_dev_configure_with(
            dev,
            backing_file_fd,
            offset,
            length,
            &DefaultLoopDeviceConfigurer,
        )
    }

    pub(super) fn attach_loop_dev_with_fallback(
        dev: &loopdev::LoopDevice,
        backing_file_fd: BorrowedFd<'_>,
        offset: u64,
        length: u64,
    ) -> io::Result<()> {
        match attach_loop_dev_configure(dev, backing_file_fd, offset, length) {
            Ok(()) => Ok(()),
            Err(_) => dev
                .with()
                .offset(offset)
                .size_limit(length)
                .read_only(true)
                .attach_fd(backing_file_fd),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use rustix::io::Errno;
        use std::cell::RefCell;
        use std::os::fd::{AsFd, OwnedFd};
        use std::rc::Rc;

        #[derive(Clone, Copy, Debug)]
        struct MockFd(i32);

        impl AsRawFd for MockFd {
            fn as_raw_fd(&self) -> std::os::fd::RawFd {
                self.0
            }
        }

        #[derive(Debug, Default)]
        struct MockState {
            configure_requests: Vec<ConfigureCall>,
            legacy_fallback_count: usize,
        }

        #[derive(Clone, Debug)]
        struct ConfigureCall {
            device_fd: i32,
            file_fd: u32,
            info: LoopInfo64,
        }

        struct MockLoopDeviceConfigurer {
            state: Rc<RefCell<MockState>>,
            configure_result: Result<(), Errno>,
        }

        impl MockLoopDeviceConfigurer {
            fn new(state: Rc<RefCell<MockState>>, configure_result: Result<(), Errno>) -> Self {
                Self {
                    state,
                    configure_result,
                }
            }
        }

        impl LoopDeviceConfigurer<MockFd> for MockLoopDeviceConfigurer {
            fn loop_configure(&self, dev: &MockFd, fd: u32, info: LoopInfo64) -> io::Result<()> {
                self.state
                    .borrow_mut()
                    .configure_requests
                    .push(ConfigureCall {
                        device_fd: dev.as_raw_fd(),
                        file_fd: fd,
                        info,
                    });
                self.configure_result
                    .map_err(|errno| io::Error::from_raw_os_error(errno.raw_os_error()))
            }
        }

        fn mock_state() -> Rc<RefCell<MockState>> {
            Rc::new(RefCell::new(MockState::default()))
        }

        fn attach_loop_dev_with_fallback_with<LoopDev, Configurer, LegacyAttach>(
            dev: &LoopDev,
            backing_file_fd: BorrowedFd<'_>,
            offset: u64,
            length: u64,
            ops: &Configurer,
            legacy_attach: LegacyAttach,
        ) -> io::Result<()>
        where
            Configurer: LoopDeviceConfigurer<LoopDev>,
            LegacyAttach: FnOnce(&LoopDev, BorrowedFd<'_>, u64, u64) -> io::Result<()>,
        {
            match attach_loop_dev_configure_with(dev, backing_file_fd, offset, length, ops) {
                Ok(()) => Ok(()),
                Err(_) => legacy_attach(dev, backing_file_fd, offset, length),
            }
        }

        fn opened_fd() -> OwnedFd {
            rustix::fs::open(
                "/dev/null",
                rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .expect("open /dev/null")
        }

        #[test]
        fn attach_opens_backing_file_and_passes_expected_loop_config() {
            let state = mock_state();
            let fd = opened_fd();
            let ops = MockLoopDeviceConfigurer::new(state.clone(), Ok(()));
            let device = MockFd(23);

            attach_loop_dev_configure_with(&device, fd.as_fd(), 4096, 8192, &ops)
                .expect("attach should succeed");

            let state = state.borrow();
            assert_eq!(state.configure_requests.len(), 1);
            assert_eq!(state.configure_requests[0].device_fd, 23);
            assert!(state.configure_requests[0].file_fd > 0);
            assert_eq!(state.configure_requests[0].info.lo_offset, 4096);
            assert_eq!(state.configure_requests[0].info.lo_sizelimit, 8192);
            assert_eq!(
                state.configure_requests[0].info.lo_flags,
                LO_FLAGS_READ_ONLY
            );
        }

        #[test]
        fn attach_returns_configure_error_after_building_loop_info() {
            let state = mock_state();
            let fd = opened_fd();
            let ops = MockLoopDeviceConfigurer::new(state.clone(), Err(Errno::INVAL));
            let device = MockFd(23);

            let err = attach_loop_dev_configure_with(&device, fd.as_fd(), 512, 2048, &ops)
                .expect_err("configure should fail");

            let state = state.borrow();
            assert_eq!(err.raw_os_error(), Some(Errno::INVAL.raw_os_error()));
            assert_eq!(state.configure_requests.len(), 1);
            assert_eq!(state.configure_requests[0].info.lo_offset, 512);
            assert_eq!(state.configure_requests[0].info.lo_sizelimit, 2048);
            assert_eq!(
                state.configure_requests[0].info.lo_flags,
                LO_FLAGS_READ_ONLY
            );
        }

        #[test]
        fn attach_falls_back_to_legacy_after_configure_failure() {
            let state = mock_state();
            let fd = opened_fd();
            let ops = MockLoopDeviceConfigurer::new(state.clone(), Err(Errno::INVAL));
            let device = MockFd(23);

            attach_loop_dev_with_fallback_with(
                &device,
                fd.as_fd(),
                1024,
                4096,
                &ops,
                |_dev, _backing_file_fd, _offset, _length| {
                    state.borrow_mut().legacy_fallback_count += 1;
                    Ok(())
                },
            )
            .expect("legacy fallback should succeed");

            let state = state.borrow();
            assert_eq!(state.configure_requests.len(), 1);
            assert_eq!(state.legacy_fallback_count, 1);
        }
    }
}

trait LoopControlOpener: Send + 'static {
    fn open(&self) -> anyhow::Result<loopdev::LoopControl>;
}

struct DefaultLoopControlOpener;

impl LoopControlOpener for DefaultLoopControlOpener {
    fn open(&self) -> anyhow::Result<loopdev::LoopControl> {
        loopdev::LoopControl::open().context("Failed to open LoopControl")
    }
}

#[derive(Debug)]
struct LoopDeviceControlActor {
    control: loopdev::LoopControl,
}

impl Actor for LoopDeviceControlActor {
    type Args = Box<dyn LoopControlOpener>;

    type Error = anyhow::Error;

    async fn on_start(opener: Self::Args, _: &ActorRef<Self>) -> anyhow::Result<Self> {
        let control = opener.open()?;
        Ok(Self { control })
    }
}
#[cfg(not(test))]
use crate::utils::actor_supervisor::ExitOnFailure;

#[cfg(test)]
use crate::utils::actor_supervisor::IgnoreOnFailure;

impl SupervisedActor for LoopDeviceControlActor {
    #[cfg(not(test))]
    type FailurePolicy = ExitOnFailure;
    #[cfg(test)]
    type FailurePolicy = IgnoreOnFailure;
}

#[message_handlers]
impl LoopDeviceControlActor {
    #[handler]
    // rsactor #[handler] requires async fn signature even without await
    async fn handle_msg_attach(
        &mut self,
        msg: loopdev_message::MsgAttach,
        _: &ActorRef<Self>,
    ) -> anyhow::Result<LoopDevice> {
        // Yield to the async scheduler so that reply messages from previous
        // handler invocations are delivered before we block on synchronous I/O.
        tokio::task::yield_now().await;
        let loopdev_message::AttachInfo {
            backing_file,
            offset,
            length,
        } = msg.attach_info;

        self.attach_loop_dev_with_retry(&backing_file, offset, length)
            .map(LoopDevice)
            .with_context(|| {
                format!(
                    "Unable to attach: (backing_file={}, offset={}, length={})",
                    backing_file.display(),
                    offset,
                    length,
                )
            })
    }
}

trait LoopAttachBackend {
    type Dev: std::fmt::Debug;
    fn next_free(&mut self) -> anyhow::Result<Self::Dev>;
    fn try_attach(&mut self, dev: &Self::Dev) -> io::Result<()>;
}

fn attach_with_retry<B: LoopAttachBackend>(
    backend: &mut B,
    busy_max_retries: u32,
    wouldblock_max_retries: u32,
    retry_interval: Duration,
) -> anyhow::Result<B::Dev> {
    let mut busy_count: u32 = 0;
    let mut wouldblock_count: u32 = 0;

    loop {
        let dev = backend.next_free()?;

        log::info!(
            "Attaching loop device {dev:?} (busy={busy_count}, wouldblock={wouldblock_count})"
        );

        match backend.try_attach(&dev) {
            Ok(()) => break Ok(dev),
            // Since loop control doesn't atomically reserve devices, concurrent attach attempts
            // can fail with EBUSY when racing for the same device.
            // Retry on EBUSY or EAGAIN(EWOULDBLOCK) with separate limits.
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::ResourceBusy | io::ErrorKind::WouldBlock
                ) =>
            {
                let exhausted = if e.kind() == io::ErrorKind::WouldBlock {
                    wouldblock_count += 1;
                    wouldblock_count > wouldblock_max_retries
                } else {
                    busy_count += 1;
                    busy_count > busy_max_retries
                };

                if exhausted {
                    break Err(e.into());
                }

                log::warn!(
                    "Loop device {dev:?} err: ({}), retrying... \
                     (busy={busy_count}, wouldblock={wouldblock_count})",
                    e.kind()
                );
                sleep(retry_interval);
            }
            Err(e) => break Err(e.into()),
        }
    }
}

struct DefaultLoopAttachBackend<'a> {
    control: &'a loopdev::LoopControl,
    backing_file_fd: BorrowedFd<'a>,
    backing_file: &'a Path,
    offset: u64,
    length: u64,
}

impl LoopAttachBackend for DefaultLoopAttachBackend<'_> {
    type Dev = loopdev::LoopDevice;

    fn next_free(&mut self) -> anyhow::Result<loopdev::LoopDevice> {
        self.control.next_free().with_context(|| {
            format!(
                "Cannot get next free loop device while attaching {}",
                self.backing_file.display()
            )
        })
    }

    fn try_attach(&mut self, dev: &loopdev::LoopDevice) -> io::Result<()> {
        loop_attach::attach_loop_dev_with_fallback(
            dev,
            self.backing_file_fd,
            self.offset,
            self.length,
        )
    }
}

impl LoopDeviceControlActor {
    const ATTACH_BUSY_MAX_RETRIES: u32 = 2;
    const ATTACH_WOULDBLOCK_MAX_RETRIES: u32 = 50;
    const ATTACH_RETRY_INTERVAL: Duration = Duration::from_millis(10);

    fn attach_loop_dev_with_retry(
        &self,
        backing_file: impl AsRef<Path>,
        offset: u64,
        length: u64,
    ) -> anyhow::Result<loopdev::LoopDevice> {
        let backing_file = backing_file.as_ref();
        let backing_file_fd = rustix::fs::open(
            backing_file,
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(|errno| io::Error::from_raw_os_error(errno.raw_os_error()))
        .with_context(|| format!("Cannot open backing file {}", backing_file.display()))?;

        let mut backend = DefaultLoopAttachBackend {
            control: &self.control,
            backing_file_fd: backing_file_fd.as_fd(),
            backing_file,
            offset,
            length,
        };

        attach_with_retry(
            &mut backend,
            Self::ATTACH_BUSY_MAX_RETRIES,
            Self::ATTACH_WOULDBLOCK_MAX_RETRIES,
            Self::ATTACH_RETRY_INTERVAL,
        )
    }
}

#[async_trait::async_trait]
pub(crate) trait LoopDeviceAttacher {
    type Device: DevicePathBackend;
    async fn attach(
        &self,
        attach_info: loopdev_message::AttachInfo,
    ) -> anyhow::Result<Self::Device>;
}

#[derive(Debug, Clone)]
pub struct LoopDeviceControl {
    control_actor: ActorRef<LoopDeviceControlActor>,
}

impl LoopDeviceControl {
    #[must_use]
    pub fn new() -> Self {
        let control_actor =
            spawn_with::<LoopDeviceControlActor>(Box::new(DefaultLoopControlOpener));
        Self { control_actor }
    }
}

impl Default for LoopDeviceControl {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl LoopDeviceAttacher for LoopDeviceControl {
    type Device = LoopDevice;
    async fn attach(
        &self,
        attach_info: loopdev_message::AttachInfo,
    ) -> anyhow::Result<Self::Device> {
        let msg = loopdev_message::MsgAttach { attach_info };
        self.control_actor.ask(msg).await?
    }
}

#[derive(Debug, Deref)]
pub(crate) struct LoopDevice(loopdev::LoopDevice);

pub(crate) trait DevicePathBackend {
    fn path(&self) -> Option<std::path::PathBuf>;
}

impl DevicePathBackend for LoopDevice {
    fn path(&self) -> Option<std::path::PathBuf> {
        self.0.path()
    }
}

impl Drop for LoopDevice {
    fn drop(&mut self) {
        // Ignoring the error as we can't do much about it.
        if let Err(e) = self.detach() {
            log::warn!(
                "Failed to request deferred detach loop device {:?} with error {:?}",
                self.path(),
                e
            );
        } else {
            log::debug!(
                "Succeeded to request deferred detach loop device: {:?}",
                self.path()
            );
        }
    }
}

fn make_dirs(dirs: impl AsRef<Path>) -> anyhow::Result<()> {
    let dirs = dirs.as_ref();
    std::fs::create_dir_all(dirs).context(format!("Failed to create {} directory", dirs.display()))
}

fn do_unmount(target: impl AsRef<Path>) -> anyhow::Result<()> {
    let target = target.as_ref();
    rustix::mount::unmount(target, UnmountFlags::empty())
        .context(format!("Failed to unmount {}", target.display()))
}

// Safety: Using Debug format ({:?}) for PathBuf instead of Display to prevent
// log injection attacks via special characters in mount target paths.
#[allow(clippy::use_debug, clippy::unnecessary_debug_formatting)]
fn do_mount(
    source: impl AsRef<Path>,
    target: impl AsRef<Path>,
    fstype: &str,
    flags: MountFlags,
    data: Option<&str>,
) -> anyhow::Result<()> {
    let source = source.as_ref();
    let target = target.as_ref();

    if !target.exists() {
        make_dirs(target)?;
    }

    let data = data.map(CString::new).transpose().context(format!(
        "Invalid mount option string (may contain null byte): {data:?}"
    ))?;

    rustix::mount::mount(source, target, fstype, flags, data.as_deref()).context(format!(
        "Failed to mount {source:?} on {target:?} with filesystem type {fstype}",
    ))
}

/// Mount steps performed by `mount_device`, injectable to unit-test rollback.
trait DeviceMountBackend {
    fn mount_base(&self, blkdev: &Path) -> anyhow::Result<()>;
    fn mount_overlay(&self) -> anyhow::Result<()>;
    fn unmount(&self) -> anyhow::Result<()>;
}

struct DefaultDeviceMountBackend<'meta> {
    pkgfs_meta: &'meta PackageFsMetadata,
}

impl DeviceMountBackend for DefaultDeviceMountBackend<'_> {
    fn mount_base(&self, blkdev: &Path) -> anyhow::Result<()> {
        let fstype = self.pkgfs_meta.packagefs_info.fstype.to_string();
        do_mount(
            blkdev,
            &self.pkgfs_meta.mount_point,
            &fstype,
            MountFlags::RDONLY,
            None,
        )
    }

    fn mount_overlay(&self) -> anyhow::Result<()> {
        mount_overlayfs(self.pkgfs_meta)
    }

    fn unmount(&self) -> anyhow::Result<()> {
        do_unmount(&self.pkgfs_meta.mount_point)
    }
}

fn mount_device(blkdev: impl AsRef<Path>, pkgfs_meta: &PackageFsMetadata) -> anyhow::Result<()> {
    let overlay_enabled = !configuration::packages_overlayfs_root().is_empty();
    mount_device_with(
        blkdev.as_ref(),
        overlay_enabled,
        &DefaultDeviceMountBackend { pkgfs_meta },
    )
}

/// Mount the base package fs, then the overlay. Roll back the base if the
/// overlay step fails.
fn mount_device_with(
    blkdev: &Path,
    overlay_enabled: bool,
    backend: &impl DeviceMountBackend,
) -> anyhow::Result<()> {
    backend.mount_base(blkdev)?;
    if overlay_enabled && let Err(e) = backend.mount_overlay() {
        if let Err(unmount_err) = backend.unmount() {
            log::warn!("mount_device: base rollback after overlay failure failed: {unmount_err:?}");
        }
        return Err(e);
    }
    Ok(())
}

/// Escape metacharacters in a lowerdir path. Colon is the list separator; the
/// kernel unescapes `\:`, so a name cannot inject an extra lowerdir.
fn escape_overlay_opt(path: &str) -> String {
    path.replace('\\', "\\\\")
        .replace(',', "\\,")
        .replace(':', "\\:")
}

/// Only lowerdir is escaped: it is a colon-separated list the kernel unescapes.
/// upperdir/workdir are single paths passed verbatim (escaping would break them).
fn build_overlay_options(lowerdir: &str, upperdir: &str, workdir: &str) -> String {
    format!(
        "lowerdir={},upperdir={upperdir},workdir={workdir}",
        escape_overlay_opt(lowerdir),
    )
}

fn mount_overlayfs(pkgfs_meta: &PackageFsMetadata) -> anyhow::Result<()> {
    log::trace!("mounting overlayfs");
    let overlayfs_root = &pkgfs_meta.overlayfs_root;

    // As we are using overlayfs_root, it should not be empty.
    assert!(!overlayfs_root.as_os_str().is_empty());

    let mntpoint = pkgfs_meta
        .mount_point
        .to_str()
        .ok_or(anyhow::anyhow!("failed to convert to string"))?;

    let upperdir = overlayfs_root.join("upper");
    let workdir = overlayfs_root.join("work");
    let options = build_overlay_options(
        mntpoint,
        upperdir.to_str().ok_or(anyhow::anyhow!(
            "mount_overlayfs(): Failed to convert upperdir - \"{}\" to string",
            upperdir.to_string_lossy()
        ))?,
        workdir.to_str().ok_or(anyhow::anyhow!(
            "mount_overlayfs(): Failed to convert workdir - \"{}\" to string",
            workdir.to_string_lossy()
        ))?,
    );

    make_dirs(upperdir)?;
    make_dirs(workdir)?;

    do_mount(
        "overlay",
        &pkgfs_meta.mount_point,
        "overlay",
        MountFlags::empty(),
        Some(options.as_str()),
    )
}

pub(crate) fn unmount_pkgfs(pkgfs_meta: &PackageFsMetadata) -> anyhow::Result<()> {
    let unmount_time = Instant::now();
    let mount_point = &pkgfs_meta.mount_point;

    // Reverse of mount_device: an optional overlay sits on the pkgfs base at
    // the same mount point. mount_pkgfs's idempotency gate prevents restacking,
    // so at most these two layers exist; unmount top-down, each guard making an
    // already-unmounted layer a no-op during recovery.
    if !configuration::packages_overlayfs_root().is_empty() && is_exact_mount(mount_point) {
        do_unmount(mount_point).context(format!(
            "unmount_pkgfs({}): Failed while unmounting overlayfs",
            pkgfs_meta.path.display()
        ))?;
    }
    if is_exact_mount(mount_point) {
        do_unmount(mount_point).context(format!(
            "unmount_pkgfs({}): Failed while unmounting package filesystem",
            pkgfs_meta.path.display()
        ))?;
    }

    log::trace!(
        "unmount_pkgfs({}) elapsed: {:?}",
        pkgfs_meta.path.display(),
        unmount_time.elapsed()
    );
    Ok(())
}

pub(crate) async fn mount_pkgfs(
    pkg_name: &str,
    pkgfs_meta: &PackageFsMetadata,
    loop_control: &impl LoopDeviceAttacher,
) -> anyhow::Result<()> {
    // Idempotency gate: reuse a live mount left by an ungraceful crash instead
    // of stacking a second loop+dm-verity+mount chain (mount(2) succeeds on top
    // of an existing mount, leaking the old devices and mount-table entry).
    // Mount point logged via Debug to prevent log injection (ssamd/AGENTS.md).
    #[allow(clippy::unnecessary_debug_formatting)]
    if is_exact_mount(&pkgfs_meta.mount_point) {
        log::info!(
            "{pkg_name}: package fs already mounted at {:?}; skipping mount",
            pkgfs_meta.mount_point
        );
        return Ok(());
    }

    let pkgfs_path = pkgfs_meta.path.as_path();
    let pkgfs_info = &pkgfs_meta.packagefs_info;
    let total_time = Instant::now();

    // Attach a single loop device covering both the data and hash regions.
    let loop_dev_info = loopdev_message::AttachInfo {
        backing_file: pkgfs_path.to_path_buf(),
        offset: pkgfs_info.pkgfs_offset,
        length: pkgfs_info.length,
    };
    // As soon as this LoopDevice variable goes out of scope, LoopDevice::drop()
    // is called, which requests deferred detach of the loop device.
    // This is intentional as detaching is deferred until the underlying
    // device file gets closed.
    let loop_dev = loop_control.attach(loop_dev_info).await.with_context(|| {
        format!(
            "{pkg_name}: failed while attaching loop device for {}",
            pkgfs_path.display()
        )
    })?;
    // Safety: Debug format ({:?}) for paths prevents log injection via special characters.
    #[allow(clippy::unnecessary_debug_formatting)]
    let loop_dev_path = loop_dev.path().with_context(|| {
        format!("{pkg_name}: loop device for {pkgfs_path:?} has no associated path after attach")
    })?;

    #[cfg(feature = "dm-verity")]
    // This variable must be kept alive until mount_device() is finished.
    // Unless dm device would be removed by VerityDevice::drop().
    // Once mounted, VerityDevice::drop() can be safely called because
    // removing is done with the DM_DEFERRED_REMOVE flag.
    let veritydev = {
        let dm_setup_time = Instant::now();

        // Append a random suffix to make each device name unique. DM_DEFERRED_REMOVE
        // processes removal asynchronously (150-250ms via kdmremove workqueue), so a
        // stale device name may still exist when the same package is remounted quickly.
        // Old devices are always cleaned up after unmount triggers deferred removal,
        // so unique names do not leak resources.
        let suffix = uuid::Uuid::new_v4().simple();
        let verity_name = format!("ssam-{pkg_name}-verity-{suffix}");
        let veritydev = VerityDevice::new(&verity_name, &loop_dev_path, &pkgfs_info.verity_info)
            .with_context(|| {
                format!(
                    "{pkg_name}: failed while creating dm-verity device from {}",
                    loop_dev_path.display()
                )
            })?;

        log::trace!("dm-verity create elapsed: {:?}", dm_setup_time.elapsed());

        veritydev
    };

    let mount_src_dev = {
        #[cfg(feature = "dm-verity")]
        {
            veritydev.devnode()
        }
        #[cfg(not(feature = "dm-verity"))]
        {
            loop_dev_path
        }
    };

    let mount_time = Instant::now();
    mount_device(&mount_src_dev, pkgfs_meta).with_context(|| {
        format!(
            "{pkg_name}: failed while mounting attached device {}",
            mount_src_dev.display()
        )
    })?;
    log::trace!("mount device elapsed: {:?}", mount_time.elapsed());
    log::trace!("mount_pkgfs elapsed: {:?}", total_time.elapsed());
    Ok(())
}

// Internal implementation that can be mocked for testing
fn findmnt_impl(
    target: impl AsRef<Path>,
    mounts_fn: impl Fn() -> Result<Vec<procfs::MountEntry>, procfs::ProcError>,
) -> anyhow::Result<String> {
    let target_path = target.as_ref();

    if !target_path.is_dir() {
        anyhow::bail!(
            "directory '{}' does not exist or is not a directory.",
            target_path.display()
        );
    }

    let mounts = mounts_fn().context(
        "Failed to read system mounts. Ensure /proc is accessible and correctly formatted.",
    )?;

    // Canonicalize before matching: /proc/mounts reports resolved paths, so a
    // lexical `target_path` under a symlinked ancestor would false-negate a
    // live mount. Falls back to the raw path when canonicalize fails.
    let canonicalize_or_raw = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let canonical_target = canonicalize_or_raw(target_path);

    let best_match_mount_point = mounts
        .iter()
        .filter_map(|mount_info| {
            let current_mount_path = Path::new(&mount_info.fs_file);
            let canonical_mount = canonicalize_or_raw(current_mount_path);
            if canonical_target.starts_with(&canonical_mount) {
                Some((mount_info, canonical_mount.as_os_str().len()))
            } else {
                None
            }
        })
        .max_by_key(|&(_, len)| len)
        .map(|(entry, _)| entry.fs_file.as_str());

    best_match_mount_point
        .map(ToOwned::to_owned)
        .ok_or(anyhow::anyhow!(
            "No matching mount point found. Ensure that the {} is mounted correctly.",
            target_path.display()
        ))
}

pub(crate) fn findmnt(target: impl AsRef<Path>) -> anyhow::Result<String> {
    findmnt_impl(target, procfs::mounts)
}

/// Whether `mount_point` is the exact target of a live mount (canonical-path
/// compared, so a symlinked ancestor is not a false negative). A `findmnt`
/// failure (missing dir, no `/proc/mounts` entry) is `false`. Ground truth for
/// "is this package's filesystem already mounted", shared by `mount_pkgfs`'s
/// idempotency gate and `unmount_pkgfs`'s guarded teardown.
fn is_exact_mount(mount_point: &Path) -> bool {
    let canonical = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    matches!(
        findmnt(mount_point),
        Ok(found) if canonical(Path::new(&found)) == canonical(mount_point)
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn escape_overlay_opt_escapes_metachars() {
        assert_eq!(escape_overlay_opt("/mnt/pkg"), "/mnt/pkg");
        assert_eq!(escape_overlay_opt("a,b"), "a\\,b");
        assert_eq!(escape_overlay_opt("a:b"), "a\\:b");
        assert_eq!(escape_overlay_opt("a\\b"), "a\\\\b");
        // Backslash escaped first, so an injected "\," stays two escaped chars.
        assert_eq!(escape_overlay_opt("x\\,y"), "x\\\\\\,y");
    }

    #[test]
    fn build_overlay_options_escapes_only_lowerdir() {
        // Colon escaped in lowerdir (list separator), raw in upperdir/workdir.
        let opts = build_overlay_options("/mnt/a:b", "/ov/a:b/upper", "/ov/a:b/work");
        assert_eq!(
            opts,
            "lowerdir=/mnt/a\\:b,upperdir=/ov/a:b/upper,workdir=/ov/a:b/work"
        );
    }

    // Overlay failure must roll back the base mount, and only then.
    #[derive(Default)]
    struct MockMountCalls {
        base_mounted: bool,
        overlay_attempted: bool,
        unmounted: bool,
        // Call order, for sequencing asserts.
        order: Vec<&'static str>,
    }

    struct MockDeviceMountBackend {
        calls: RefCell<MockMountCalls>,
        base_ok: bool,
        overlay_ok: bool,
        unmount_ok: bool,
    }

    impl DeviceMountBackend for MockDeviceMountBackend {
        fn mount_base(&self, _blkdev: &Path) -> anyhow::Result<()> {
            let mut calls = self.calls.borrow_mut();
            calls.base_mounted = true;
            calls.order.push("base");
            if self.base_ok {
                Ok(())
            } else {
                anyhow::bail!("injected base mount failure")
            }
        }
        fn mount_overlay(&self) -> anyhow::Result<()> {
            let mut calls = self.calls.borrow_mut();
            calls.overlay_attempted = true;
            calls.order.push("overlay");
            if self.overlay_ok {
                Ok(())
            } else {
                anyhow::bail!("injected overlay mount failure")
            }
        }
        fn unmount(&self) -> anyhow::Result<()> {
            let mut calls = self.calls.borrow_mut();
            calls.unmounted = true;
            calls.order.push("unmount");
            if self.unmount_ok {
                Ok(())
            } else {
                anyhow::bail!("injected unmount failure")
            }
        }
    }

    fn mock_backend(overlay_ok: bool) -> MockDeviceMountBackend {
        MockDeviceMountBackend {
            calls: RefCell::new(MockMountCalls::default()),
            base_ok: true,
            overlay_ok,
            unmount_ok: true,
        }
    }

    #[test]
    fn mount_device_rolls_back_base_on_overlay_failure() {
        let backend = mock_backend(false);
        let res = mount_device_with(Path::new("/dev/fake"), true, &backend);
        assert!(res.is_err());
        let calls = backend.calls.borrow();
        assert!(calls.base_mounted);
        assert!(calls.overlay_attempted);
        assert!(
            calls.unmounted,
            "base must be unmounted after overlay failure"
        );
        assert_eq!(
            calls.order,
            ["base", "overlay", "unmount"],
            "must mount base, attempt overlay, then roll back"
        );
    }

    #[test]
    fn mount_device_returns_overlay_error_when_rollback_unmount_fails() {
        // Overlay and rollback-unmount both fail: caller still gets the overlay
        // error (unmount error is only logged).
        let backend = MockDeviceMountBackend {
            calls: RefCell::new(MockMountCalls::default()),
            base_ok: true,
            overlay_ok: false,
            unmount_ok: false,
        };
        let err = mount_device_with(Path::new("/dev/fake"), true, &backend).unwrap_err();
        assert!(
            err.to_string().contains("injected overlay mount failure"),
            "must return the overlay error, not the rollback unmount error: {err}"
        );
        assert!(
            backend.calls.borrow().unmounted,
            "rollback must be attempted"
        );
    }

    #[test]
    fn mount_device_stops_on_base_failure() {
        // Base mount fails: overlay and rollback must not run.
        let backend = MockDeviceMountBackend {
            calls: RefCell::new(MockMountCalls::default()),
            base_ok: false,
            overlay_ok: true,
            unmount_ok: true,
        };
        let err = mount_device_with(Path::new("/dev/fake"), true, &backend).unwrap_err();
        assert!(err.to_string().contains("injected base mount failure"));
        let calls = backend.calls.borrow();
        assert!(calls.base_mounted);
        assert!(
            !calls.overlay_attempted,
            "overlay must not run after base failure"
        );
        assert!(
            !calls.unmounted,
            "nothing to roll back when base itself failed"
        );
    }

    #[test]
    fn mount_device_no_rollback_on_success() {
        let backend = mock_backend(true);
        let res = mount_device_with(Path::new("/dev/fake"), true, &backend);
        assert!(res.is_ok());
        assert!(
            !backend.calls.borrow().unmounted,
            "success must not unmount"
        );
    }

    #[test]
    fn mount_device_skips_overlay_when_disabled() {
        let backend = mock_backend(false);
        let res = mount_device_with(Path::new("/dev/fake"), false, &backend);
        assert!(res.is_ok());
        let calls = backend.calls.borrow();
        assert!(calls.base_mounted);
        assert!(
            !calls.overlay_attempted,
            "overlay must not run when disabled"
        );
        assert!(!calls.unmounted);
    }

    struct FailingLoopControlOpener;

    impl LoopControlOpener for FailingLoopControlOpener {
        fn open(&self) -> anyhow::Result<loopdev::LoopControl> {
            anyhow::bail!("injected LoopControl::open failure")
        }
    }

    #[tokio::test]
    async fn loop_control_open_failure_surfaces_as_onstart_failure() {
        // Spawn directly rather than through spawn_with: the supervisor consumes
        // the JoinHandle, so a direct spawn is the only way to observe the
        // ActorResult instead of the supervisor's logging side effect.
        let (_actor_ref, handle) =
            rsactor::spawn::<LoopDeviceControlActor>(Box::new(FailingLoopControlOpener));

        match handle.await.expect("supervised actor task must not panic") {
            rsactor::ActorResult::Failed { phase, .. } => {
                assert_eq!(phase, rsactor::FailurePhase::OnStart);
            }
            rsactor::ActorResult::Completed { .. } => {
                panic!("expected on_start failure, got successful completion")
            }
        }
    }

    mod findmnt_test {
        use super::*;
        use std::fs;
        use tempfile::TempDir;
        // Helper function to create a temporary directory for testing
        fn create_test_dir() -> TempDir {
            tempfile::tempdir().expect("Failed to create temp directory")
        }

        // Helper function to create mock mount entries
        fn create_mock_mount_entry(mount_point: &str) -> procfs::MountEntry {
            use std::collections::HashMap;
            let mut mnt_ops = HashMap::new();
            mnt_ops.insert("rw".to_string(), None);
            mnt_ops.insert("relatime".to_string(), None);

            procfs::MountEntry {
                fs_spec: "/dev/sda1".to_string(),
                fs_file: mount_point.to_string(),
                fs_vfstype: "ext4".to_string(),
                fs_mntops: mnt_ops,
                fs_freq: 0,
                fs_passno: 0,
            }
        }

        fn create_mock_mount_entries(entries: &[procfs::MountEntry]) -> Vec<procfs::MountEntry> {
            entries.to_vec()
        }

        #[test]
        fn test_findmnt_happy_case_root_mount() {
            let temp_dir = create_test_dir();
            let target_path = temp_dir.path();

            // Mock procfs::mounts() to return root mount point
            let mounts_fn = move || Ok(vec![create_mock_mount_entry("/")]);

            let result = findmnt_impl(target_path, mounts_fn);

            assert!(result.is_ok());
            assert_eq!(result.unwrap(), "/");
        }

        #[test]
        fn test_findmnt_happy_case_specific_mount() {
            let temp_dir = create_test_dir();
            let target_path = temp_dir.path();

            // Create a subdirectory that matches our mock mount point
            let mount_point = temp_dir.path().to_string_lossy().to_string();
            let mock_mounts = vec![
                create_mock_mount_entry("/"),
                create_mock_mount_entry(&mount_point),
            ];
            let mounts_fn = move || Ok(create_mock_mount_entries(&mock_mounts));

            let result = findmnt_impl(target_path, mounts_fn);

            assert!(result.is_ok());
            assert_eq!(result.unwrap(), mount_point);
        }

        #[test]
        fn test_findmnt_best_match_longest_path() {
            let temp_dir = create_test_dir();

            // Create nested mount points - should return the longest matching path
            let base_mount = temp_dir.path().to_string_lossy().to_string();
            let nested_mount = format!("{base_mount}/nested");

            // Create the nested directory
            fs::create_dir_all(&nested_mount).expect("Failed to create nested dir");
            let nested_target = Path::new(&nested_mount);

            let mock_mounts = vec![
                create_mock_mount_entry("/"),
                create_mock_mount_entry(&base_mount),
                create_mock_mount_entry(&nested_mount),
            ];
            let mounts_fn = move || Ok(create_mock_mount_entries(&mock_mounts));

            let result = findmnt_impl(nested_target, mounts_fn);

            assert!(result.is_ok());
            assert_eq!(result.unwrap(), nested_mount);
        }

        #[test]
        fn test_findmnt_multiple_mounts_chooses_best_match() {
            let temp_dir = create_test_dir();
            let target_path = temp_dir.path();

            // Multiple mount points, but we want the one that matches our target best
            let mount_point = temp_dir.path().to_string_lossy().to_string();
            let mock_mounts = vec![
                create_mock_mount_entry("/"),
                create_mock_mount_entry("/usr"),
                create_mock_mount_entry("/var"),
                create_mock_mount_entry(&mount_point),
                create_mock_mount_entry("/home"),
            ];
            let mounts_fn = move || Ok(create_mock_mount_entries(&mock_mounts));

            let result = findmnt_impl(target_path, mounts_fn);

            assert!(result.is_ok());
            assert_eq!(result.unwrap(), mount_point);
        }

        #[test]
        fn test_findmnt_error_case_non_existent_directory() {
            let non_existent_path = Path::new("/this/path/does/not/exist");

            // Mock function shouldn't be called since path validation fails first
            let mounts_fn = || Ok(vec![create_mock_mount_entry("/")]);

            let result = findmnt_impl(non_existent_path, mounts_fn);

            assert!(result.is_err());
            let error_msg = result.unwrap_err().to_string();
            assert!(error_msg.contains("does not exist or is not a directory"));
        }

        #[test]
        fn test_findmnt_error_case_not_a_directory() {
            let temp_dir = create_test_dir();
            let file_path = temp_dir.path().join("testfile");

            // Create a file (not a directory)
            fs::write(&file_path, "test").expect("Failed to create test file");

            let mounts_fn = || Ok(vec![create_mock_mount_entry("/")]);

            let result = findmnt_impl(&file_path, mounts_fn);

            assert!(result.is_err());
            let error_msg = result.unwrap_err().to_string();
            assert!(error_msg.contains("does not exist or is not a directory"));
        }

        #[test]
        fn test_findmnt_error_case_no_matching_mount() {
            let temp_dir = create_test_dir();
            let target_path = temp_dir.path();

            // Mock mounts that don't include our target path
            let mock_mounts = vec![
                create_mock_mount_entry("/usr"),
                create_mock_mount_entry("/var"),
                create_mock_mount_entry("/home"),
            ];
            let mounts_fn = move || Ok(create_mock_mount_entries(&mock_mounts));

            let result = findmnt_impl(target_path, mounts_fn);

            assert!(result.is_err());
            let error_msg = result.unwrap_err().to_string();
            assert!(error_msg.contains("No matching mount point found"));
        }

        #[test]
        fn test_findmnt_error_case_procfs_failure() {
            let temp_dir = create_test_dir();
            let target_path = temp_dir.path();

            // Mock procfs failure
            let mounts_fn = move || {
                Err(procfs::ProcError::NotFound(Some(
                    "proc not available".into(),
                )))
            };

            let result = findmnt_impl(target_path, mounts_fn);

            assert!(result.is_err());
            let error_msg = result.unwrap_err().to_string();
            assert!(error_msg.contains("Failed to read system mounts"));
        }

        #[test]
        fn test_findmnt_edge_case_empty_mounts() {
            let temp_dir = create_test_dir();
            let target_path = temp_dir.path();

            // Empty mounts list
            let mock_mounts = vec![];
            let mounts_fn = move || Ok(create_mock_mount_entries(&mock_mounts));

            let result = findmnt_impl(target_path, mounts_fn);

            assert!(result.is_err());
            let error_msg = result.unwrap_err().to_string();
            assert!(error_msg.contains("No matching mount point found"));
        }

        #[test]
        fn test_findmnt_edge_case_root_directory() {
            // Testing with root directory "/"
            let root_path = Path::new("/");

            let mock_mounts = vec![
                create_mock_mount_entry("/"),
                create_mock_mount_entry("/usr"),
                create_mock_mount_entry("/var"),
            ];
            let mounts_fn = move || Ok(create_mock_mount_entries(&mock_mounts));

            let result = findmnt_impl(root_path, mounts_fn);

            assert!(result.is_ok());
            assert_eq!(result.unwrap(), "/");
        }

        #[test]
        fn test_findmnt_edge_case_mount_point_with_spaces() {
            let temp_dir = create_test_dir();

            // Mount point with spaces in the name
            let mount_point = temp_dir.path().to_string_lossy().to_string();
            let mut mock_mount = create_mock_mount_entry(&mount_point);
            mock_mount.fs_file = format!("{mount_point}/path with spaces");

            // Create the actual directory with spaces
            let spaced_dir = temp_dir.path().join("path with spaces");
            fs::create_dir_all(&spaced_dir).expect("Failed to create spaced dir");

            let mock_mounts = vec![create_mock_mount_entry("/"), mock_mount];
            let expected_mount = format!("{mount_point}/path with spaces");
            let mounts_fn = move || Ok(create_mock_mount_entries(&mock_mounts));

            let result = findmnt_impl(&spaced_dir, mounts_fn);

            assert!(result.is_ok());
            assert_eq!(result.unwrap(), expected_mount);
        }

        #[test]
        fn test_findmnt_edge_case_symlink_directory() {
            let temp_dir = create_test_dir();
            let real_dir = temp_dir.path().join("real_dir");
            let symlink_dir = temp_dir.path().join("symlink_dir");

            fs::create_dir(&real_dir).expect("Failed to create real dir");
            std::os::unix::fs::symlink(&real_dir, &symlink_dir).expect("Failed to create symlink");

            let mount_point = real_dir.to_string_lossy().to_string();
            let mock_mounts = vec![
                create_mock_mount_entry("/"),
                create_mock_mount_entry(&mount_point),
            ];
            let mounts_fn = move || Ok(create_mock_mount_entries(&mock_mounts));

            // Test using the symlink path
            let result = findmnt_impl(&symlink_dir, mounts_fn);

            // Should work with symlink since Path operations handle it
            assert!(result.is_ok());
        }

        #[test]
        fn test_findmnt_integration_with_real_procfs() {
            // Integration test using real procfs::mounts() function
            // This tests the actual function without mocking
            let root_path = Path::new("/");

            let result = findmnt(root_path);

            // This should succeed on any Linux system
            assert!(result.is_ok());
            let mount_point = result.unwrap();
            // Root should always be mounted at "/"
            assert_eq!(mount_point, "/");
        }
    }

    #[test]
    fn test_pkgfs_info_length_matches_metadata() {
        use crate::package_volume::PackageFsMetadata;
        use crate::package_volume::tests::package_fs_metadata_test::{
            create_test_ssam_package_file, default_test_verity,
        };
        use libssam::ssam_package::{PackageFilesystem, PackageFsVerityInfo};
        use libssam::superblock::FsType;
        use std::path::PathBuf;

        let test_path = PathBuf::from("/test/package/path");

        // Default mock: offset 1024, length 8192 + 2048
        let pkg_file = create_test_ssam_package_file();
        let meta = PackageFsMetadata::new(&test_path, &pkg_file).unwrap();
        assert_eq!(meta.packagefs_info.length, 8192 + 2048);

        // Override payload size — length must follow the mock, not be recomputed.
        let mut pkg_file2 = create_test_ssam_package_file();
        let verity = PackageFsVerityInfo {
            hash_offset: 16_384,
            hash_size: 4_096,
            ..default_test_verity()
        };
        pkg_file2.pkgfs = PackageFilesystem::new(2048, 16_384 + 4_096, FsType::Ext4, verity);
        let meta2 = PackageFsMetadata::new(&test_path, &pkg_file2).unwrap();
        assert_eq!(meta2.packagefs_info.length, 16_384 + 4_096);
    }
}

#[cfg(test)]
mod attach_retry_tests {
    use super::*;

    const NO_DELAY: Duration = Duration::ZERO;

    struct MockLoopAttachBackend<F> {
        call_count: u32,
        try_attach_fn: F,
        next_free_fails: bool,
    }

    impl<F> MockLoopAttachBackend<F> {
        fn new(try_attach_fn: F) -> Self {
            Self {
                call_count: 0,
                try_attach_fn,
                next_free_fails: false,
            }
        }

        fn with_failing_next_free(try_attach_fn: F) -> Self {
            Self {
                call_count: 0,
                try_attach_fn,
                next_free_fails: true,
            }
        }
    }

    impl<F: FnMut(u32) -> io::Result<()>> LoopAttachBackend for MockLoopAttachBackend<F> {
        type Dev = u32;

        fn next_free(&mut self) -> anyhow::Result<u32> {
            if self.next_free_fails {
                return Err(anyhow::anyhow!("no free devices"));
            }
            Ok(self.call_count)
        }

        fn try_attach(&mut self, _dev: &u32) -> io::Result<()> {
            let n = self.call_count;
            self.call_count += 1;
            (self.try_attach_fn)(n)
        }
    }

    fn wouldblock_error() -> io::Error {
        io::Error::new(io::ErrorKind::WouldBlock, "would block")
    }

    fn busy_error() -> io::Error {
        io::Error::new(io::ErrorKind::ResourceBusy, "device busy")
    }

    #[test]
    fn succeeds_on_first_attempt() {
        let mut backend = MockLoopAttachBackend::new(|_| Ok(()));
        let result = attach_with_retry(&mut backend, 2, 50, NO_DELAY);
        assert_eq!(result.unwrap(), 0);
    }

    #[test]
    fn wouldblock_retries_then_succeeds() {
        let mut backend = MockLoopAttachBackend::new(|n| {
            if n < 3 {
                Err(wouldblock_error())
            } else {
                Ok(())
            }
        });
        let result = attach_with_retry(&mut backend, 2, 50, NO_DELAY);
        assert!(result.is_ok());
        assert_eq!(backend.call_count, 4);
    }

    #[test]
    fn busy_retries_then_succeeds() {
        let mut backend =
            MockLoopAttachBackend::new(|n| if n < 2 { Err(busy_error()) } else { Ok(()) });
        let result = attach_with_retry(&mut backend, 2, 50, NO_DELAY);
        assert!(result.is_ok());
        assert_eq!(backend.call_count, 3);
    }

    #[test]
    fn wouldblock_exhaustion_returns_error() {
        let mut backend = MockLoopAttachBackend::new(|_| Err(wouldblock_error()));
        let result = attach_with_retry(&mut backend, 2, 5, NO_DELAY);
        assert!(result.is_err());
        assert_eq!(
            result
                .unwrap_err()
                .downcast_ref::<io::Error>()
                .unwrap()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(backend.call_count, 6);
    }

    #[test]
    fn busy_exhaustion_returns_error() {
        let mut backend = MockLoopAttachBackend::new(|_| Err(busy_error()));
        let result = attach_with_retry(&mut backend, 2, 50, NO_DELAY);
        assert!(result.is_err());
        assert_eq!(
            result
                .unwrap_err()
                .downcast_ref::<io::Error>()
                .unwrap()
                .kind(),
            io::ErrorKind::ResourceBusy
        );
        assert_eq!(backend.call_count, 3);
    }

    #[test]
    fn busy_exhausts_independently_of_wouldblock() {
        let mut backend = MockLoopAttachBackend::new(|n| {
            if n == 0 {
                Err(wouldblock_error())
            } else {
                Err(busy_error())
            }
        });
        let result = attach_with_retry(&mut backend, 2, 50, NO_DELAY);
        assert!(result.is_err());
        assert_eq!(
            result
                .unwrap_err()
                .downcast_ref::<io::Error>()
                .unwrap()
                .kind(),
            io::ErrorKind::ResourceBusy
        );
        assert_eq!(backend.call_count, 4);
    }

    #[test]
    fn other_error_returns_immediately() {
        let mut backend = MockLoopAttachBackend::new(|_| {
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied"))
        });
        let result = attach_with_retry(&mut backend, 2, 50, NO_DELAY);
        assert!(result.is_err());
        assert_eq!(backend.call_count, 1);
    }

    #[test]
    fn next_free_error_propagates() {
        let mut backend = MockLoopAttachBackend::with_failing_next_free(|_: u32| Ok(()));
        let result = attach_with_retry(&mut backend, 2, 50, NO_DELAY);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("no free devices"));
    }
}
