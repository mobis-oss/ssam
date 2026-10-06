// Copyright 2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use std::{
    mem::size_of,
    os::fd::{AsRawFd, RawFd},
    pin::Pin,
    task::{Context as TaskContext, Poll},
};

use anyhow::Context;
use linux_raw_sys::general::NGROUPS_MAX;
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::UnixStream,
};
use tonic::{
    Request, Status,
    transport::server::{Connected, UdsConnectInfo},
};

const INITIAL_PEER_GROUPS_BUFFER_SIZE: usize = 16;

#[derive(Clone)]
pub(crate) struct PeerConnectInfo {
    uds: UdsConnectInfo,
    peer_groups: Vec<u32>,
}

pub(crate) struct PeerUnixStream {
    stream: UnixStream,
    peer_groups: Vec<u32>,
}

impl PeerUnixStream {
    pub(crate) fn new(stream: UnixStream) -> Self {
        let peer_groups = read_peer_groups(stream.as_raw_fd(), INITIAL_PEER_GROUPS_BUFFER_SIZE)
            .inspect_err(|error| {
                log::warn!(
                    "Failed to read peer supplementary groups; supplementary GID authorization is unavailable: {error:#}"
                );
            })
            .unwrap_or_default();
        Self {
            stream,
            peer_groups,
        }
    }
}

impl Connected for PeerUnixStream {
    type ConnectInfo = PeerConnectInfo;

    fn connect_info(&self) -> Self::ConnectInfo {
        PeerConnectInfo {
            uds: <UnixStream as Connected>::connect_info(&self.stream),
            peer_groups: self.peer_groups.clone(),
        }
    }
}

impl AsyncRead for PeerUnixStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

impl AsyncWrite for PeerUnixStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

fn parse_peer_groups(groups: &[libc::gid_t], returned_len: usize) -> anyhow::Result<Vec<u32>> {
    let gid_size = size_of::<libc::gid_t>();
    anyhow::ensure!(
        returned_len.is_multiple_of(gid_size),
        "peer groups returned length is not aligned to gid_t"
    );
    let group_count = returned_len / gid_size;
    anyhow::ensure!(
        group_count <= groups.len(),
        "peer groups returned length exceeds buffer"
    );

    Ok(groups[..group_count].to_vec())
}

fn read_peer_groups(fd: RawFd, initial_group_count: usize) -> anyhow::Result<Vec<u32>> {
    let max_groups: usize = NGROUPS_MAX
        .try_into()
        .context("NGROUPS_MAX does not fit usize")?;
    let gid_size = size_of::<libc::gid_t>();
    let mut group_count = initial_group_count.min(max_groups);

    loop {
        let supplied_len = group_count
            .checked_mul(gid_size)
            .context("peer groups buffer size overflow")?;
        let mut groups = vec![0; group_count];
        let mut socket_len: libc::socklen_t = supplied_len
            .try_into()
            .context("peer groups buffer size does not fit socklen_t")?;

        // SAFETY: `groups` owns a writable buffer of `socket_len` bytes, and `socket_len`
        // points to valid storage for the kernel to update.
        let result = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PEERGROUPS,
                groups.as_mut_ptr().cast(),
                &raw mut socket_len,
            )
        };
        let returned_len: usize = socket_len
            .try_into()
            .context("peer groups returned length does not fit usize")?;

        if result == 0 {
            return parse_peer_groups(&groups, returned_len);
        }

        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ERANGE) {
            return Err(error).context("failed to read peer groups");
        }
        anyhow::ensure!(
            returned_len.is_multiple_of(gid_size),
            "peer groups required length is not aligned to gid_t"
        );

        let required_groups = returned_len / gid_size;
        let next_group_count = required_groups
            .max(group_count.saturating_mul(2))
            .min(max_groups);
        anyhow::ensure!(
            next_group_count > group_count,
            "peer groups buffer cannot be enlarged"
        );
        group_count = next_group_count;
    }
}

pub(crate) fn authorize_peer(
    request: Request<()>,
    allowed_gids: &[u32],
) -> Result<Request<()>, Status> {
    let info = request
        .extensions()
        .get::<PeerConnectInfo>()
        .ok_or_else(|| Status::permission_denied("peer credentials unavailable"))?;
    let credentials = info
        .uds
        .peer_cred
        .as_ref()
        .ok_or_else(|| Status::permission_denied("peer credentials unavailable"))?;

    let daemon_egid = rustix::process::getegid().as_raw();
    if peer_is_authorized(
        credentials.gid(),
        &info.peer_groups,
        daemon_egid,
        allowed_gids,
    ) {
        Ok(request)
    } else {
        Err(Status::permission_denied("peer credentials rejected"))
    }
}

fn peer_is_authorized(
    peer_egid: u32,
    peer_groups: &[u32],
    daemon_egid: u32,
    allowed_gids: &[u32],
) -> bool {
    let gid_is_allowed = |gid: &u32| gid == &daemon_egid || allowed_gids.contains(gid);
    gid_is_allowed(&peer_egid) || peer_groups.iter().any(gid_is_allowed)
}

#[cfg(test)]
mod tests {
    use super::{
        PeerConnectInfo, PeerUnixStream, UdsConnectInfo, authorize_peer, parse_peer_groups,
        peer_is_authorized, read_peer_groups,
    };
    use std::{mem::size_of, os::fd::AsRawFd};
    use tonic::{Code, Request};

    #[test]
    fn peer_authorization_rejects_missing_credentials() {
        let status =
            authorize_peer(Request::new(()), &[]).expect_err("credentials must be required");
        assert_eq!(status.code(), Code::PermissionDenied);

        let mut request = Request::new(());
        request.extensions_mut().insert(PeerConnectInfo {
            uds: UdsConnectInfo {
                peer_addr: None,
                peer_cred: None,
            },
            peer_groups: vec![1000],
        });
        let status = authorize_peer(request, &[1000]).expect_err("credentials must be required");
        assert_eq!(status.code(), Code::PermissionDenied);
    }

    #[test]
    fn peer_authorization_matches_effective_or_supplementary_groups() {
        assert!(peer_is_authorized(1000, &[], 1000, &[]));
        assert!(peer_is_authorized(2000, &[], 1000, &[2000]));
        assert!(peer_is_authorized(2000, &[1000], 1000, &[]));
        assert!(peer_is_authorized(2000, &[1000], 3000, &[1000]));
        assert!(peer_is_authorized(1000, &[1000], 3000, &[1000]));
        assert!(!peer_is_authorized(2000, &[3000], 1000, &[1000]));
        assert!(!peer_is_authorized(2000, &[], 1000, &[1000]));
        assert!(peer_is_authorized(2000, &[], 1000, &[2000]));
    }

    #[test]
    fn peer_group_buffer_checks_lengths() {
        let gid_size = size_of::<libc::gid_t>();
        assert_eq!(
            parse_peer_groups(&[1000, u32::MAX], gid_size * 2).unwrap(),
            vec![1000, u32::MAX]
        );
        assert_eq!(parse_peer_groups(&[1000], 0).unwrap(), [] as [u32; 0]);
        assert!(parse_peer_groups(&[1000], gid_size - 1).is_err());
        assert!(parse_peer_groups(&[1000], gid_size * 2).is_err());
    }

    #[test]
    fn peer_group_lookup_failure_is_unavailable() {
        assert!(read_peer_groups(-1, 1).is_err());
    }

    #[tokio::test]
    async fn accepted_socket_exposes_peer_credentials_and_groups() {
        let path = format!("\0ssamd-test-peer-groups-{}", std::process::id());
        let listener = tokio::net::UnixListener::bind(&path).expect("listener should bind");
        let _client = tokio::net::UnixStream::connect(&path)
            .await
            .expect("client should connect");
        let (stream, _) = listener
            .accept()
            .await
            .expect("connection should be accepted");
        let wrapper = PeerUnixStream::new(stream);
        let info = <PeerUnixStream as tonic::transport::server::Connected>::connect_info(&wrapper);
        assert!(info.uds.peer_cred.is_some());
        let mut expected: Vec<u32> = rustix::process::getgroups()
            .expect("current supplementary groups should be readable")
            .into_iter()
            .map(rustix::process::Gid::as_raw)
            .collect();
        let mut actual = info.peer_groups;
        expected.sort_unstable();
        actual.sort_unstable();
        assert_eq!(actual, expected);

        let mut grown = read_peer_groups(wrapper.stream.as_raw_fd(), 1)
            .expect("peer groups should grow from a small buffer");
        grown.sort_unstable();
        assert_eq!(grown, expected);
    }
}
