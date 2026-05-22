// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

#[allow(non_camel_case_types)]
#[allow(dead_code)]
#[allow(clippy::pedantic)]
pub(crate) mod defs;

use anyhow::Context;
use defs::{
    DM_BUFFER_FULL_FLAG, DM_DEFERRED_REMOVE, DM_DEV_CREATE_CMD, DM_DEV_REMOVE_CMD,
    DM_DEV_SUSPEND_CMD, DM_IOCTL, DM_LIST_DEVICES_CMD, DM_READONLY_FLAG, DM_TABLE_LOAD_CMD,
    dm_ioctl, dm_name_list, dm_target_spec,
};
use rustix::io::Result;
use rustix::ioctl::{Ioctl, IoctlOutput, Opcode, ioctl as rustix_ioctl, opcode};
use std::fs::File;
use std::mem::size_of;
use std::ops::Deref;
use std::os::fd::AsFd;
use std::os::raw::c_char;

// The device-mapper ioctl commands are mapped to the minimum ioctl
// interface version required, based on the _cmd_data_v4 table defined
// in the libdm/ioctl/libdm-iface.c file from the lvm2/libdevmapper
// sources.
// - DM_VERSION_CMD: 4.0.0,
// - DM_REMOVE_ALL_CMD: 4.0.0
// - DM_LIST_DEVICES_CMD: 4.0.0
// - DM_DEV_CREATE_CMD: 4.0.0
// - DM_DEV_REMOVE_CMD: 4.0.0
// - DM_DEV_RENAME_CMD: 4.0.0
// - DM_DEV_SUSPEND_CMD: 4.0.0
// - DM_DEV_STATUS_CMD: 4.0.0
// - DM_DEV_WAIT_CMD: 4.0.0
// - DM_TABLE_LOAD_CMD: 4.0.0
// - DM_TABLE_CLEAR_CMD: 4.0.0
// - DM_TABLE_DEPS_CMD: 4.0.0
// - DM_TABLE_STATUS_CMD: 4.0.0
// - DM_LIST_VERSIONS_CMD: 4.1.0
// - DM_TARGET_MSG_CMD: 4.2.0
// - DM_DEV_SET_GEOMETRY_CMD: 4.6.0
// libdevmapper sets DM_DEV_ARM_POLL to (4, 36, 0) however the command was
// added after 4.36.0: depend on 4.37 to reliably access ARM_POLL.
// - DM_DEV_ARM_POLL_CMD: 4.37.0
// - DM_GET_TARGET_VERSION_CMD: 4.41.0
const DM_VERSION: [u32; 3] = [4, 0, 0];

/// Control path for user space to pass IOCTL to kernel DM
const DM_CONTROL_PATH: &str = "/dev/mapper/control";

/// Start with a large buffer to make `DM_BUFFER_FULL` rare. Libdm does this too.
const DEF_BUF_SIZE: usize = 16 * 1024;

// ioctl ABI constants: u32 opcode values are defined by kernel ABI to fit u8
// DM_IOCTL and the CMD constants are u32 values guaranteed to fit u8 by kernel ABI.
#[allow(clippy::cast_possible_truncation)]
mod dm_abi_opcodes {
    use super::{
        DM_DEV_CREATE_CMD, DM_DEV_REMOVE_CMD, DM_DEV_SUSPEND_CMD, DM_IOCTL, DM_LIST_DEVICES_CMD,
        DM_TABLE_LOAD_CMD,
    };
    pub(super) const DM_IOCTL_U8: u8 = DM_IOCTL as u8;
    pub(super) const DM_DEV_CREATE_U8: u8 = DM_DEV_CREATE_CMD as u8;
    pub(super) const DM_DEV_REMOVE_U8: u8 = DM_DEV_REMOVE_CMD as u8;
    pub(super) const DM_DEV_SUSPEND_U8: u8 = DM_DEV_SUSPEND_CMD as u8;
    pub(super) const DM_LIST_DEVICES_U8: u8 = DM_LIST_DEVICES_CMD as u8;
    pub(super) const DM_TABLE_LOAD_U8: u8 = DM_TABLE_LOAD_CMD as u8;
}
#[repr(u32)]
#[derive(strum_macros::FromRepr, strum_macros::Display, Debug)]
enum DmOpcodes {
    DevCreate = opcode::read_write::<dm_ioctl>(
        dm_abi_opcodes::DM_IOCTL_U8,
        dm_abi_opcodes::DM_DEV_CREATE_U8,
    ),
    DevRemove = opcode::read_write::<dm_ioctl>(
        dm_abi_opcodes::DM_IOCTL_U8,
        dm_abi_opcodes::DM_DEV_REMOVE_U8,
    ),
    DevSuspend = opcode::read_write::<dm_ioctl>(
        dm_abi_opcodes::DM_IOCTL_U8,
        dm_abi_opcodes::DM_DEV_SUSPEND_U8,
    ),
    ListDevices = opcode::read_write::<dm_ioctl>(
        dm_abi_opcodes::DM_IOCTL_U8,
        dm_abi_opcodes::DM_LIST_DEVICES_U8,
    ),
    TableLoad = opcode::read_write::<dm_ioctl>(
        dm_abi_opcodes::DM_IOCTL_U8,
        dm_abi_opcodes::DM_TABLE_LOAD_U8,
    ),
}

// Kernel guarantees buffer is aligned to dm_name_list requirements
#[allow(clippy::cast_ptr_alignment)]
fn get_list_devices(buf: &[c_char]) -> Vec<String> {
    let mut list_devs = Vec::new();
    let mut list_devs_data = buf;
    while !list_devs_data.is_empty() {
        let device = unsafe {
            (list_devs_data.as_ptr().cast::<dm_name_list>())
                .as_ref()
                .expect("Failed to cast to dm_name_list")
        };

        let ptr = device.name.as_ptr().cast::<c_char>();

        let name = unsafe {
            std::ffi::CStr::from_ptr(ptr)
                .to_str()
                .expect("Failed to convert CStr to &str")
        };

        list_devs.push(name.to_owned());

        if device.next == 0 {
            break;
        }

        list_devs_data = &list_devs_data[device.next as usize..];
    }
    list_devs
}

fn gen_io_hdr(
    device_name: Option<&str>,
    device_uuid: Option<&str>,
    flag: u32,
    dm_data: &DMData,
) -> anyhow::Result<DMIoctl> {
    let mut io_hdr = DMIoctl::new()
        .set_flags(flag)
        .set_target_count(dm_data.target_count())
        .set_data_size(dm_data.data_size()?);
    if let Some(name) = device_name {
        io_hdr = io_hdr.set_name(name);
    }
    if let Some(uuid) = device_uuid {
        io_hdr = io_hdr.set_uuid(uuid);
    }

    Ok(io_hdr)
}

#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) struct DeviceNum {
    major: u32,
    minor: u32,
}

#[allow(dead_code)]
impl DeviceNum {
    pub(crate) fn new(major: u32, minor: u32) -> Self {
        Self { major, minor }
    }

    pub(crate) fn from_dm_ioctl(ioctl: &dm_ioctl) -> Self {
        Self::from_kdev_t(ioctl.dev)
    }

    // kdev_t fields are masked to their valid bit ranges before cast
    #[allow(clippy::cast_possible_truncation)]
    pub(crate) fn from_kdev_t(kdev: u64) -> Self {
        let major = ((kdev & 0xfff00) >> 8) as u32;
        let minor = ((kdev & 0xff) | ((kdev >> 12) & 0xf_ff00)) as u32;
        Self { major, minor }
    }

    pub(crate) fn as_kdev_t(&self) -> u64 {
        let minor = u64::from(self.minor);
        let major = u64::from(self.major);
        minor & 0xff | (major << 8) | ((minor & !0xff) << 12)
    }

    pub(crate) fn minor(&self) -> u32 {
        self.minor
    }

    pub(crate) fn major(&self) -> u32 {
        self.major
    }
}

#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct DMDevice {
    dev_no: DeviceNum,
    device_name: String,
}

#[allow(dead_code)]
impl DMDevice {
    pub(crate) fn new(dev_no: DeviceNum, device_name: String) -> Self {
        Self {
            dev_no,
            device_name,
        }
    }

    pub(crate) fn dev_no(&self) -> &DeviceNum {
        &self.dev_no
    }

    pub(crate) fn device_name(&self) -> String {
        self.device_name.clone()
    }
}

struct DMIoctl {
    inner: dm_ioctl,
}

impl DMIoctl {
    // size_of::<dm_ioctl>() is a compile-time constant that fits in u32
    #[allow(clippy::cast_possible_truncation)]
    fn new() -> Self {
        Self {
            inner: dm_ioctl {
                version: DM_VERSION,
                data_start: size_of::<dm_ioctl>() as u32,
                ..dm_ioctl::default()
            },
        }
    }

    fn set_name(mut self, name: &str) -> Self {
        let name: Vec<c_char> = name
            .as_bytes()
            .iter()
            .map(|&b| c_char::from_ne_bytes([b]))
            .collect();
        let max_name_len = self.inner.name.len() - 1;
        let name_len = name.len().min(max_name_len);
        self.inner.name[..name_len].copy_from_slice(name.as_ref());
        self.inner.name[name_len] = 0;
        self
    }

    fn set_uuid(mut self, uuid: &str) -> Self {
        let uuid: Vec<c_char> = uuid
            .as_bytes()
            .iter()
            .map(|&b| c_char::from_ne_bytes([b]))
            .collect();
        let max_uuid_len = self.inner.uuid.len() - 1;
        let uuid_len = uuid.len().min(max_uuid_len);
        self.inner.uuid[..uuid_len].copy_from_slice(uuid.as_ref());
        self.inner.uuid[uuid_len] = 0;
        self
    }

    fn set_flags(mut self, flags: u32) -> Self {
        self.inner.flags = flags;
        self
    }

    fn set_target_count(mut self, count: u32) -> Self {
        self.inner.target_count = count;
        self
    }

    // data_size input is u32; max(DEF_BUF_SIZE, ...) result bounded to addressable memory
    #[allow(clippy::cast_possible_truncation)]
    fn set_data_size(mut self, size: u32) -> Self {
        let data_size = std::cmp::max(DEF_BUF_SIZE, size_of::<dm_ioctl>() + size as usize);

        self.inner.data_size = data_size as u32;
        self
    }
}

impl Deref for DMIoctl {
    type Target = dm_ioctl;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

struct DMData {
    data: Vec<c_char>,
    target_count: u32,
}

impl DMData {
    fn empty_data() -> Self {
        let data: Vec<c_char> = vec![0; DEF_BUF_SIZE];
        Self {
            data,
            target_count: 0,
        }
    }

    fn from_target_info(targets: &[DMTargetInfo]) -> anyhow::Result<Self> {
        let mut target_spec_data: Vec<c_char> = Vec::new();
        for target in targets {
            let mut target_spec = dm_target_spec {
                sector_start: target.sector_start,
                length: target.sector_size,
                ..Default::default()
            };
            let target_type: Vec<c_char> = target
                .target_type
                .as_bytes()
                .iter()
                .map(|&b| c_char::from_ne_bytes([b]))
                .collect();
            let target_type_len = target.target_type.len().min(15usize);
            target_spec.target_type[..target_type_len].copy_from_slice(target_type.as_ref());
            target_spec.next =
                u32::try_from(size_of::<dm_target_spec>() + target.params.len() + 1usize)
                    .context("dm_target_spec next offset overflows u32")?;
            target_spec_data.extend(unsafe {
                std::slice::from_raw_parts(
                    (&raw const target_spec).cast::<c_char>(),
                    size_of::<dm_target_spec>(),
                )
            });

            target_spec_data.extend(
                target
                    .params
                    .as_bytes()
                    .iter()
                    .map(|&b| c_char::from_ne_bytes([b])),
            );
            target_spec_data.push(0);
        }
        Ok(Self {
            data: target_spec_data,
            target_count: u32::try_from(targets.len()).context("target count overflows u32")?,
        })
    }

    fn as_raw_slice(&self) -> &[c_char] {
        &self.data
    }

    fn data_size(&self) -> anyhow::Result<u32> {
        u32::try_from(self.data.len()).context("dm data size overflows u32")
    }

    fn target_count(&self) -> u32 {
        self.target_count
    }

    fn resize_mul(&mut self, multiplier: usize) -> anyhow::Result<()> {
        if multiplier > 1 {
            let cap = self.data.capacity();

            let new_cap = cap.saturating_mul(multiplier);
            let additional = new_cap.saturating_sub(cap);

            // To avoid potential overflow in Vec allocation
            if additional > 0 {
                self.data
                    .try_reserve_exact(additional)
                    .context("Failed to reserve additional space for DMData")?;
            }
            self.data.resize(new_cap, 0);
        } else {
            log::warn!("Multiplier must be greater than 1; no resizing performed.");
        }
        Ok(())
    }
}

struct DmIoctlPayload {
    inner: Vec<c_char>,
}

impl DmIoctlPayload {
    fn new(io_hdr: &dm_ioctl, data: &DMData) -> Self {
        let mut ioctl = *io_hdr;
        let mut buf = Vec::with_capacity(ioctl.data_size as usize);
        let hdr = (&raw mut ioctl).cast::<c_char>();
        let len = size_of::<dm_ioctl>();
        buf.extend_from_slice(unsafe { std::slice::from_raw_parts(hdr, len) });
        buf.extend_from_slice(data.as_raw_slice());
        buf.resize(buf.capacity(), 0);

        Self { inner: buf }
    }

    fn as_mut_ptr(&mut self) -> *mut c_char {
        self.inner.as_mut_ptr()
    }

    // Buffer is allocated with dm_ioctl as the leading type; alignment guaranteed
    #[allow(clippy::cast_ptr_alignment)]
    fn as_dm_ioctl(&self) -> &dm_ioctl {
        unsafe { &*(self.inner.as_ptr().cast::<dm_ioctl>()) }
    }

    pub(crate) fn as_data_slice<T>(&self) -> &[T] {
        let ioctl = self.as_dm_ioctl();
        let start = ioctl.data_start as usize;
        let end = ioctl.data_size as usize;
        let data = &self.inner[start..end];
        let len = data.len() / size_of::<T>();
        unsafe { std::slice::from_raw_parts(data.as_ptr().cast::<T>(), len) }
    }
}

struct IoctlPayload<const OPCODE: Opcode> {
    payload: DmIoctlPayload,
}

impl<const OPCODE: Opcode> IoctlPayload<OPCODE> {
    fn new(payload: DmIoctlPayload) -> Self {
        Self { payload }
    }
}

unsafe impl<const OPCODE: Opcode> Ioctl for IoctlPayload<OPCODE> {
    type Output = DmIoctlPayload;

    const IS_MUTATING: bool = false;

    fn opcode(&self) -> Opcode {
        OPCODE
    }

    fn as_ptr(&mut self) -> *mut rustix::ffi::c_void {
        self.payload.as_mut_ptr().cast::<rustix::ffi::c_void>()
    }

    unsafe fn output_from_ptr(
        _out: IoctlOutput,
        extract_output: *mut rustix::ffi::c_void,
    ) -> Result<Self::Output> {
        // extract_output points to the start of the buffer (dm_ioctl)
        let ioctl_ptr = extract_output as *const dm_ioctl;
        let ioctl = unsafe { &*ioctl_ptr };
        let total_len = ioctl.data_size as usize;
        let src = extract_output as *const c_char;
        let slice = unsafe { std::slice::from_raw_parts(src, total_len) };
        Ok(DmIoctlPayload {
            inner: slice.to_vec(),
        })
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct DMTargetInfo {
    sector_start: u64,
    sector_size: u64,
    target_type: String,
    params: String,
}

impl DMTargetInfo {
    pub(crate) fn new(
        sector_start: u64,
        sector_size: u64,
        target_type: String,
        params: String,
    ) -> Self {
        Self {
            sector_start,
            sector_size,
            target_type,
            params,
        }
    }
}

#[derive(Debug)]
pub(crate) struct DMControl {
    file: File,
}

impl DMControl {
    pub(crate) fn new() -> anyhow::Result<Self> {
        let file = File::options()
            .read(true)
            .write(true)
            .open(DM_CONTROL_PATH)
            .with_context(|| format!("Failed to open DM control device {DM_CONTROL_PATH}"))?;

        Ok(Self { file })
    }

    fn ioctl<I: Ioctl>(&self, cmd: I) -> anyhow::Result<I::Output> {
        let opcode = cmd.opcode();
        // Implementing From<u32> for DmOpcodes is more idiomatic and reusable.
        let opcode = DmOpcodes::from_repr(opcode).context("Invalid opcode for device mapper")?;
        let ret = unsafe { rustix_ioctl(self.file.as_fd(), cmd) };
        ret.context(format!("Failed to perform ioctl ({opcode})"))
    }

    fn send_cmd<const OPCODE: u32>(
        &self,
        io_hdr: DMIoctl,
        dm_data: DMData,
    ) -> anyhow::Result<DmIoctlPayload> {
        let mut mul_num = 1usize;
        let mut io_hdr = io_hdr;
        let mut dm_data = dm_data;

        let mut result;
        loop {
            let payload = DmIoctlPayload::new(&io_hdr, &dm_data);
            let ioctl_cmd = IoctlPayload::<{ OPCODE }>::new(payload);
            result = self.ioctl(ioctl_cmd)?;
            if (result.as_dm_ioctl().flags & DM_BUFFER_FULL_FLAG) == 0 {
                break;
            }
            log::warn!(
                "Resizing buffer to {} bytes\n",
                dm_data.data.capacity() * mul_num
            );
            mul_num = mul_num.saturating_mul(2);
            dm_data
                .resize_mul(mul_num)
                .context("Failed to resize buffer")?;
            io_hdr = io_hdr.set_data_size(dm_data.data_size()?);
        }

        Ok(result)
    }

    pub(crate) fn create_device(&self, device_name: String) -> anyhow::Result<DMDevice> {
        let dm_data = DMData::empty_data();
        let io_hdr = gen_io_hdr(Some(&device_name), None, DM_READONLY_FLAG, &dm_data)?;
        let result = self.send_cmd::<{ DmOpcodes::DevCreate as u32 }>(io_hdr, dm_data)?;
        let dev_num = DeviceNum::from_dm_ioctl(result.as_dm_ioctl());

        Ok(DMDevice::new(dev_num, device_name))
    }

    pub(crate) fn load_table(
        &self,
        device_name: Option<&str>,
        device_uuid: Option<&str>,
        targets: &[DMTargetInfo],
    ) -> anyhow::Result<()> {
        let dm_data = DMData::from_target_info(targets)?;
        let io_hdr = gen_io_hdr(device_name, device_uuid, DM_READONLY_FLAG, &dm_data)?;
        let _result = self.send_cmd::<{ DmOpcodes::TableLoad as u32 }>(io_hdr, dm_data)?;

        Ok(())
    }

    pub(crate) fn resume_device(
        &self,
        device_name: Option<&str>,
        device_uuid: Option<&str>,
    ) -> anyhow::Result<()> {
        let dm_data = DMData::empty_data();
        let io_hdr = gen_io_hdr(device_name, device_uuid, DM_READONLY_FLAG, &dm_data)?;
        let _result = self.send_cmd::<{ DmOpcodes::DevSuspend as u32 }>(io_hdr, dm_data)?;

        Ok(())
    }

    pub(crate) fn remove_device(
        &self,
        device_name: Option<&str>,
        device_uuid: Option<&str>,
    ) -> anyhow::Result<()> {
        let dm_data = DMData::empty_data();
        let io_hdr = gen_io_hdr(device_name, device_uuid, DM_DEFERRED_REMOVE, &dm_data)?;
        let _result = self.send_cmd::<{ DmOpcodes::DevRemove as u32 }>(io_hdr, dm_data)?;

        Ok(())
    }

    pub(crate) fn list_devices(&self) -> anyhow::Result<Vec<String>> {
        let dm_data = DMData::empty_data();
        let io_hdr = gen_io_hdr(None, None, DM_READONLY_FLAG, &dm_data)?;
        let result = self.send_cmd::<{ DmOpcodes::ListDevices as u32 }>(io_hdr, dm_data)?;

        let list_devices = get_list_devices(result.as_data_slice::<c_char>());
        Ok(list_devices)
    }
}
#[cfg(test)]
// Test helpers cast C struct i8/u8 fields and ioctl sizes intentionally.
#[allow(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_ptr_alignment,
    clippy::borrow_as_ptr,
    clippy::ref_as_ptr,
    clippy::similar_names
)]
mod tests {
    use super::*;

    // Grouped former standalone test modules as submodules for organizational consistency.
    mod device_num_tests {
        use super::*;
        #[test]
        fn test_device_num_new() {
            let dev_num = DeviceNum::new(8, 0);
            assert_eq!(dev_num.major(), 8);
            assert_eq!(dev_num.minor(), 0);
        }
        #[test]
        fn test_device_num_from_kdev_t() {
            let kdev = 0x0800; // major=8, minor=0
            let dev_num = DeviceNum::from_kdev_t(kdev);
            assert_eq!(dev_num.major(), 8);
            assert_eq!(dev_num.minor(), 0);
            let kdev = 0x8FF; // major=8, minor=255
            let dev_num = DeviceNum::from_kdev_t(kdev);
            assert_eq!(dev_num.major(), 8);
            assert_eq!(dev_num.minor(), 255);
        }
        #[test]
        fn test_device_num_as_kdev_t() {
            let dev_num = DeviceNum::new(8, 0);
            let kdev = dev_num.as_kdev_t();
            assert_eq!(kdev, 0x0800);
            let dev_num = DeviceNum::new(8, 255);
            let kdev = dev_num.as_kdev_t();
            let reconstructed = DeviceNum::from_kdev_t(kdev);
            assert_eq!(reconstructed.major(), 8);
            assert_eq!(reconstructed.minor(), 255);
        }
        #[test]
        fn test_device_num_roundtrip() {
            let original = DeviceNum::new(253, 1024);
            let kdev = original.as_kdev_t();
            let reconstructed = DeviceNum::from_kdev_t(kdev);
            assert_eq!(original.major(), reconstructed.major());
            assert_eq!(original.minor(), reconstructed.minor());
        }
        #[test]
        fn test_device_num_from_dm_ioctl() {
            let mut dm_ioctl = dm_ioctl {
                dev: 0x0800,
                ..Default::default()
            };
            let dev_num = DeviceNum::from_dm_ioctl(&dm_ioctl);
            assert_eq!(dev_num.major(), 8);
            assert_eq!(dev_num.minor(), 0);
            dm_ioctl.dev = 0xFDFF; // major=253, minor=255
            let dev_num = DeviceNum::from_dm_ioctl(&dm_ioctl);
            assert_eq!(dev_num.major(), 253);
            assert_eq!(dev_num.minor(), 255);
        }
    }

    mod dm_device_tests {
        use super::*;
        #[test]
        fn test_dm_device_new() {
            let dev_num = DeviceNum::new(253, 0);
            let device_name = "test-device".to_string();
            let dm_device = DMDevice::new(dev_num, device_name.clone());
            assert_eq!(dm_device.device_name(), device_name);
            assert_eq!(dm_device.dev_no().major(), 253);
            assert_eq!(dm_device.dev_no().minor(), 0);
        }
        #[test]
        fn test_dm_device_getters() {
            let dev_num = DeviceNum::new(253, 1);
            let device_name = "my-dm-device".to_string();
            let dm_device = DMDevice::new(dev_num, device_name.clone());
            let returned_dev_no = dm_device.dev_no();
            assert_eq!(returned_dev_no.major(), 253);
            assert_eq!(returned_dev_no.minor(), 1);
            let returned_name = dm_device.device_name();
            assert_eq!(returned_name, device_name);
        }
    }

    mod dm_ioctl_tests {
        use super::*;
        #[test]
        fn test_dm_ioctl_new() {
            let ioctl = DMIoctl::new();
            assert_eq!(ioctl.version, DM_VERSION);
            assert_eq!(ioctl.data_start, size_of::<dm_ioctl>() as u32);
            assert_eq!(ioctl.target_count, 0);
            assert_eq!(ioctl.flags, 0);
        }
        #[test]
        fn test_dm_ioctl_set_name() {
            let test_name = "test-device";
            let ioctl = DMIoctl::new().set_name(test_name);
            // SAFETY: set_name() guarantees null-termination of the name field.
            let name_str = unsafe { std::ffi::CStr::from_ptr(ioctl.name.as_ptr()) }
                .to_str()
                .unwrap();
            assert_eq!(name_str, test_name);
        }
        #[test]
        fn test_dm_ioctl_set_uuid() {
            let test_uuid = "test-uuid-12345";
            let ioctl = DMIoctl::new().set_uuid(test_uuid);
            // SAFETY: set_uuid() guarantees null-termination of the uuid field.
            let uuid_str = unsafe { std::ffi::CStr::from_ptr(ioctl.uuid.as_ptr()) }
                .to_str()
                .unwrap();
            assert_eq!(uuid_str, test_uuid);
        }
        #[test]
        fn test_dm_ioctl_set_flags() {
            let test_flags = DM_READONLY_FLAG;
            let ioctl = DMIoctl::new().set_flags(test_flags);
            assert_eq!(ioctl.flags, test_flags);
        }
        #[test]
        fn test_dm_ioctl_set_target_count() {
            let test_count = 3;
            let ioctl = DMIoctl::new().set_target_count(test_count);
            assert_eq!(ioctl.target_count, test_count);
        }
        #[test]
        fn test_dm_ioctl_set_data_size() {
            let test_size = 1024;
            let ioctl = DMIoctl::new().set_data_size(test_size);
            let expected_size =
                std::cmp::max(DEF_BUF_SIZE, size_of::<dm_ioctl>() + test_size as usize);
            assert_eq!(ioctl.data_size, expected_size as u32);
        }
        #[test]
        fn test_dm_ioctl_chaining() {
            let ioctl = DMIoctl::new()
                .set_name("chain-test")
                .set_flags(DM_READONLY_FLAG)
                .set_target_count(2)
                .set_data_size(2048);
            assert_eq!(ioctl.flags, DM_READONLY_FLAG);
            assert_eq!(ioctl.target_count, 2);
            // SAFETY: set_name() guarantees null-termination of the name field.
            let name_str = unsafe { std::ffi::CStr::from_ptr(ioctl.name.as_ptr()) }
                .to_str()
                .unwrap();
            assert_eq!(name_str, "chain-test");
        }
    }

    mod dm_data_tests {
        use super::*;
        #[test]
        fn test_dm_data_empty_data() {
            let dm_data = DMData::empty_data();
            assert_eq!(dm_data.target_count(), 0);
            assert_eq!(dm_data.data_size().unwrap(), DEF_BUF_SIZE as u32);
            assert_eq!(dm_data.data.len(), DEF_BUF_SIZE);
        }
        #[test]
        fn test_dm_data_from_target_info_single() {
            let target =
                DMTargetInfo::new(0, 1024, "linear".to_string(), "/dev/sda1 0".to_string());
            let targets = vec![target];
            let dm_data = DMData::from_target_info(&targets).unwrap();
            assert_eq!(dm_data.target_count(), 1);
            assert!(dm_data.data_size().unwrap() > 0);
        }
        #[test]
        fn test_dm_data_from_target_info_multiple() {
            let target1 =
                DMTargetInfo::new(0, 512, "linear".to_string(), "/dev/sda1 0".to_string());
            let target2 =
                DMTargetInfo::new(512, 512, "linear".to_string(), "/dev/sda2 0".to_string());
            let targets = vec![target1, target2];
            let dm_data = DMData::from_target_info(&targets).unwrap();
            assert_eq!(dm_data.target_count(), 2);
            assert!(dm_data.data_size().unwrap() > 0);
        }
        #[test]
        fn test_dm_data_resize_mul() {
            let mut dm_data = DMData::empty_data();
            let original_len = dm_data.data.len();
            dm_data.resize_mul(2).unwrap();
            assert_eq!(dm_data.data.len(), original_len * 2);
            dm_data.resize_mul(3).unwrap();
            assert_eq!(dm_data.data.len(), original_len * 2 * 3);
        }
        #[test]
        fn test_dm_data_resize_mul_success() {
            let mut dm_data = DMData::empty_data();
            let original_len = dm_data.data.len();
            let result = dm_data.resize_mul(2);
            assert!(result.is_ok());
            assert_eq!(dm_data.data.len(), original_len * 2);
            let result = dm_data.resize_mul(3);
            assert!(result.is_ok());
            assert_eq!(dm_data.data.len(), original_len * 2 * 3);
        }
        #[test]
        fn test_dm_data_as_raw_slice() {
            let dm_data = DMData::empty_data();
            let slice = dm_data.as_raw_slice();
            assert_eq!(slice.len(), DEF_BUF_SIZE);
        }
        #[test]
        fn test_dm_data_resize_mul_capacity_exceeded() {
            let mut dm_data = DMData::empty_data();
            let original_len = dm_data.data.len();
            let result = dm_data.resize_mul(usize::MAX);
            assert!(result.is_err());
            assert_eq!(dm_data.data.len(), original_len);
        }
        #[test]
        fn test_dm_data_resize_mul_edge_cases() {
            let mut dm_data = DMData::empty_data();
            let original_len = dm_data.data.len();
            let result = dm_data.resize_mul(0);
            assert!(result.is_ok());
            assert_eq!(dm_data.data.len(), original_len);
            let result = dm_data.resize_mul(1);
            assert!(result.is_ok());
            assert_eq!(dm_data.data.len(), original_len);
        }
    }

    mod dm_target_info_tests {
        use super::*;
        #[test]
        fn test_dm_target_info_new() {
            let sector_start = 0;
            let sector_size = 1024;
            let target_type = "linear".to_string();
            let params = "/dev/sda1 0".to_string();
            let target_info = DMTargetInfo::new(
                sector_start,
                sector_size,
                target_type.clone(),
                params.clone(),
            );
            assert_eq!(target_info.sector_start, sector_start);
            assert_eq!(target_info.sector_size, sector_size);
            assert_eq!(target_info.target_type, target_type);
            assert_eq!(target_info.params, params);
        }
        #[test]
        fn test_dm_target_info_default() {
            let target_info = DMTargetInfo::default();
            assert_eq!(target_info.sector_start, 0);
            assert_eq!(target_info.sector_size, 0);
            assert_eq!(target_info.target_type, "");
            assert_eq!(target_info.params, "");
        }
        #[test]
        fn test_dm_target_info_clone() {
            let original = DMTargetInfo::new(
                100,
                2048,
                "verity".to_string(),
                "hash_device metadata".to_string(),
            );
            let cloned = original.clone();
            assert_eq!(original.sector_start, cloned.sector_start);
            assert_eq!(original.sector_size, cloned.sector_size);
            assert_eq!(original.target_type, cloned.target_type);
            assert_eq!(original.params, cloned.params);
        }
        #[test]
        fn test_dm_target_info_different_types() {
            let linear_target =
                DMTargetInfo::new(0, 1024, "linear".to_string(), "/dev/sda1 0".to_string());
            let verity_target = DMTargetInfo::new(
                1024,
                2048,
                "verity".to_string(),
                "1 /dev/sda2 /dev/sda3 4096 4096 256 256 sha256".to_string(),
            );
            assert_ne!(linear_target.target_type, verity_target.target_type);
            assert_ne!(linear_target.params, verity_target.params);
            assert_ne!(linear_target.sector_start, verity_target.sector_start);
        }
    }

    mod get_list_devices_tests {
        use super::*;
        use std::os::raw::c_char;
        fn create_test_dm_name_list(name: &str, next_offset: u32, dev: u64) -> Vec<c_char> {
            let mut buffer = Vec::new();
            buffer.extend_from_slice(unsafe {
                std::slice::from_raw_parts((&dev as *const u64).cast::<c_char>(), 8)
            });
            buffer.extend_from_slice(unsafe {
                std::slice::from_raw_parts((&next_offset as *const u32).cast::<c_char>(), 4)
            });
            buffer.extend(name.as_bytes().iter().map(|&b| b as c_char));
            buffer.push(0);
            while buffer.len() < next_offset as usize && next_offset > 0 {
                buffer.push(0);
            }
            buffer
        }
        #[test]
        fn test_get_list_devices_single_device() {
            let test_buffer = create_test_dm_name_list("test-device", 0, 0x0800);
            let devices = get_list_devices(&test_buffer);
            assert_eq!(devices.len(), 1);
            assert_eq!(devices[0], "test-device");
        }
        #[test]
        fn test_get_list_devices_multiple_devices() {
            let mut buffer = Vec::new();
            let first_device = create_test_dm_name_list("device1", 32, 0x0800);
            buffer.extend_from_slice(&first_device);
            while buffer.len() < 32 {
                buffer.push(0);
            }
            let second_device = create_test_dm_name_list("device2", 0, 0x0801);
            buffer.extend_from_slice(&second_device);
            let devices = get_list_devices(&buffer);
            assert_eq!(devices.len(), 2);
            assert_eq!(devices[0], "device1");
            assert_eq!(devices[1], "device2");
        }
        #[test]
        fn test_get_list_devices_empty_buffer() {
            let empty_buffer: Vec<c_char> = Vec::new();
            let devices = get_list_devices(&empty_buffer);
            assert_eq!(devices.len(), 0);
        }
    }

    mod gen_io_hdr_tests {
        use super::*;
        use std::mem::size_of;
        #[test]
        fn test_gen_io_hdr_with_name_only() {
            let dm_data = DMData::empty_data();
            let device_name = "test-device";
            let io_hdr = gen_io_hdr(Some(device_name), None, DM_READONLY_FLAG, &dm_data).unwrap();
            assert_eq!(io_hdr.flags, DM_READONLY_FLAG);
            assert_eq!(io_hdr.target_count, 0);
            let expected_size = std::cmp::max(
                DEF_BUF_SIZE,
                size_of::<dm_ioctl>() + dm_data.data_size().unwrap() as usize,
            ) as u32;
            assert_eq!(io_hdr.data_size, expected_size);
            // SAFETY: gen_io_hdr() guarantees null-termination of name and uuid fields.
            let name_str = unsafe { std::ffi::CStr::from_ptr(io_hdr.name.as_ptr()) }
                .to_str()
                .unwrap();
            assert_eq!(name_str, device_name);
            let uuid_str = unsafe { std::ffi::CStr::from_ptr(io_hdr.uuid.as_ptr()) }
                .to_str()
                .unwrap();
            assert_eq!(uuid_str.len(), 0);
        }
        #[test]
        fn test_gen_io_hdr_with_uuid_only() {
            let dm_data = DMData::empty_data();
            let device_uuid = "test-uuid-12345";
            let io_hdr = gen_io_hdr(None, Some(device_uuid), DM_READONLY_FLAG, &dm_data).unwrap();
            assert_eq!(io_hdr.flags, DM_READONLY_FLAG);
            assert_eq!(io_hdr.target_count, 0);
            // SAFETY: gen_io_hdr() guarantees null-termination of name and uuid fields.
            let name_str = unsafe { std::ffi::CStr::from_ptr(io_hdr.name.as_ptr()) }
                .to_str()
                .unwrap();
            assert_eq!(name_str.len(), 0);
            let uuid_str = unsafe { std::ffi::CStr::from_ptr(io_hdr.uuid.as_ptr()) }
                .to_str()
                .unwrap();
            assert_eq!(uuid_str, device_uuid);
        }
        #[test]
        fn test_gen_io_hdr_with_name_and_uuid() {
            let dm_data = DMData::empty_data();
            let device_name = "test-device";
            let device_uuid = "test-uuid";
            let io_hdr = gen_io_hdr(
                Some(device_name),
                Some(device_uuid),
                DM_READONLY_FLAG,
                &dm_data,
            )
            .unwrap();
            assert_eq!(io_hdr.flags, DM_READONLY_FLAG);
            // SAFETY: gen_io_hdr() guarantees null-termination of name and uuid fields.
            let name_str = unsafe { std::ffi::CStr::from_ptr(io_hdr.name.as_ptr()) }
                .to_str()
                .unwrap();
            assert_eq!(name_str, device_name);
            let uuid_str = unsafe { std::ffi::CStr::from_ptr(io_hdr.uuid.as_ptr()) }
                .to_str()
                .unwrap();
            assert_eq!(uuid_str, device_uuid);
        }
        #[test]
        fn test_gen_io_hdr_with_targets() {
            let target1 =
                DMTargetInfo::new(0, 1024, "linear".to_string(), "/dev/sda1 0".to_string());
            let target2 =
                DMTargetInfo::new(1024, 1024, "linear".to_string(), "/dev/sda2 0".to_string());
            let targets = vec![target1, target2];
            let dm_data = DMData::from_target_info(&targets).unwrap();
            let io_hdr = gen_io_hdr(Some("test-device"), None, DM_READONLY_FLAG, &dm_data).unwrap();
            assert_eq!(io_hdr.target_count, 2);
            let expected_size = std::cmp::max(
                DEF_BUF_SIZE,
                size_of::<dm_ioctl>() + dm_data.data_size().unwrap() as usize,
            ) as u32;
            assert_eq!(io_hdr.data_size, expected_size);
        }
        #[test]
        fn test_gen_io_hdr_no_name_no_uuid() {
            let dm_data = DMData::empty_data();
            let io_hdr = gen_io_hdr(None, None, DM_READONLY_FLAG, &dm_data).unwrap();
            assert_eq!(io_hdr.flags, DM_READONLY_FLAG);
            assert_eq!(io_hdr.target_count, 0);
            // SAFETY: gen_io_hdr() guarantees null-termination of name and uuid fields.
            let name_str = unsafe { std::ffi::CStr::from_ptr(io_hdr.name.as_ptr()) }
                .to_str()
                .unwrap();
            assert_eq!(name_str.len(), 0);
            let uuid_str = unsafe { std::ffi::CStr::from_ptr(io_hdr.uuid.as_ptr()) }
                .to_str()
                .unwrap();
            assert_eq!(uuid_str.len(), 0);
        }
    }

    mod dm_ioctl_payload_tests {
        use super::*;
        #[test]
        fn test_dm_ioctl_payload_new() {
            let dm_data = DMData::empty_data();
            let io_hdr = DMIoctl::new()
                .set_name("test-device")
                .set_flags(DM_READONLY_FLAG);
            let payload = DmIoctlPayload::new(&io_hdr, &dm_data);
            let returned_ioctl = payload.as_dm_ioctl();
            assert_eq!(returned_ioctl.version, DM_VERSION);
            assert_eq!(returned_ioctl.flags, DM_READONLY_FLAG);
            // SAFETY: set_name() guarantees null-termination of the name field.
            let name_str = unsafe { std::ffi::CStr::from_ptr(returned_ioctl.name.as_ptr()) }
                .to_str()
                .unwrap();
            assert_eq!(name_str, "test-device");
        }
        #[test]
        fn test_dm_ioctl_payload_as_data_slice() {
            let target =
                DMTargetInfo::new(0, 1024, "linear".to_string(), "/dev/sda1 0".to_string());
            let targets = vec![target];
            let dm_data = DMData::from_target_info(&targets).unwrap();
            let io_hdr = DMIoctl::new()
                .set_target_count(1)
                .set_data_size(dm_data.data_size().unwrap());
            let payload = DmIoctlPayload::new(&io_hdr, &dm_data);
            let data_slice: &[c_char] = payload.as_data_slice();
            assert!(!data_slice.is_empty());
            let data_slice_u8: &[u8] = payload.as_data_slice();
            assert!(!data_slice_u8.is_empty());
        }
        #[test]
        fn test_dm_ioctl_payload_as_mut_ptr() {
            let dm_data = DMData::empty_data();
            let io_hdr = DMIoctl::new();
            let mut payload = DmIoctlPayload::new(&io_hdr, &dm_data);
            let ptr = payload.as_mut_ptr();
            assert!(!ptr.is_null());
            let ioctl_from_ptr = unsafe { &*(ptr as *const dm_ioctl) };
            assert_eq!(ioctl_from_ptr.version, DM_VERSION);
        }
    }

    mod ioctl_payload_tests {
        use super::*;
        use rustix::ioctl::{IoctlOutput, opcode};
        const TEST_OPCODE: Opcode =
            opcode::read_write::<dm_ioctl>(DM_IOCTL as u8, DM_DEV_CREATE_CMD as u8);
        #[test]
        fn test_ioctl_payload_new() {
            let dm_data = DMData::empty_data();
            let io_hdr = DMIoctl::new().set_name("test-device");
            let dm_payload = DmIoctlPayload::new(&io_hdr, &dm_data);
            let ioctl_payload = IoctlPayload::<TEST_OPCODE>::new(dm_payload);
            assert_eq!(ioctl_payload.opcode(), TEST_OPCODE);
        }
        #[test]
        fn test_ioctl_payload_opcode() {
            let dm_data = DMData::empty_data();
            let io_hdr = DMIoctl::new();
            let dm_payload = DmIoctlPayload::new(&io_hdr, &dm_data);
            let ioctl_payload = IoctlPayload::<TEST_OPCODE>::new(dm_payload);
            assert_eq!(ioctl_payload.opcode(), TEST_OPCODE);
        }
        #[test]
        fn test_ioctl_payload_as_ptr() {
            let dm_data = DMData::empty_data();
            let io_hdr = DMIoctl::new().set_name("test-device");
            let dm_payload = DmIoctlPayload::new(&io_hdr, &dm_data);
            let mut ioctl_payload = IoctlPayload::<TEST_OPCODE>::new(dm_payload);
            let ptr = ioctl_payload.as_ptr();
            assert!(!ptr.is_null());
            let ioctl_from_ptr = unsafe { &*(ptr as *const dm_ioctl) };
            assert_eq!(ioctl_from_ptr.version, DM_VERSION);
        }
        #[test]
        fn test_ioctl_payload_output_from_ptr() {
            let dm_data = DMData::empty_data();
            let io_hdr = DMIoctl::new()
                .set_name("test-device")
                .set_data_size(dm_data.data_size().unwrap());
            let _dm_payload = DmIoctlPayload::new(&io_hdr, &dm_data);
            let test_ioctl = dm_ioctl {
                version: DM_VERSION,
                data_size: 512,
                data_start: size_of::<dm_ioctl>() as u32,
                ..dm_ioctl::default()
            };
            let mut test_buffer = Vec::new();
            test_buffer.extend_from_slice(unsafe {
                std::slice::from_raw_parts(
                    (&test_ioctl as *const dm_ioctl).cast::<u8>(),
                    size_of::<dm_ioctl>(),
                )
            });
            test_buffer.resize(512, 0);
            let output = unsafe {
                IoctlPayload::<TEST_OPCODE>::output_from_ptr(
                    IoctlOutput::default(),
                    test_buffer.as_ptr() as *mut rustix::ffi::c_void,
                )
            };
            assert!(output.is_ok());
            let result_payload = output.unwrap();
            let result_ioctl = result_payload.as_dm_ioctl();
            assert_eq!(result_ioctl.version, DM_VERSION);
            assert_eq!(result_ioctl.data_size, 512);
        }
    }
}
