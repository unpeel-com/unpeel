//! File-descriptor passing over Unix sockets (`sendmsg`/`recvmsg` with
//! `SCM_RIGHTS`, raw `libc`). Used by the PTY core handoff: every PTY
//! master, listener, and live client socket moves from the old core to the
//! new one without closing.
//!
//! Framing: one message = 4-byte big-endian payload length, then the
//! payload. The fds ride on the `sendmsg` that carries the length header,
//! so the receiver picks them up on the first `recvmsg` and then reads the
//! payload with ordinary reads. Child module of `session_host`.

use std::io::{self, Read, Write};
use std::os::unix::io::{FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;

/// Upper bound on fds per message (a Session carries pty + listener +
/// clients; a core header carries lock + listener).
pub(crate) const MAX_FDS_PER_MESSAGE: usize = 64;
const MAX_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;

/// Send `payload` as one framed message with `fds` attached.
pub(crate) fn send_message(
    stream: &mut UnixStream,
    payload: &[u8],
    fds: &[RawFd],
) -> io::Result<()> {
    if fds.len() > MAX_FDS_PER_MESSAGE {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "too many fds"));
    }
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "payload too large",
        ));
    }
    let header = (payload.len() as u32).to_be_bytes();
    send_with_fds(stream, &header, fds)?;
    stream.write_all(payload)?;
    stream.flush()
}

/// Receive one framed message and the fds attached to its header.
pub(crate) fn recv_message(stream: &mut UnixStream) -> io::Result<(Vec<u8>, Vec<OwnedFd>)> {
    let mut header = [0u8; 4];
    let (n, fds) = recv_with_fds(stream, &mut header, MAX_FDS_PER_MESSAGE)?;
    if n == 0 {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "peer closed"));
    }
    if n < 4 {
        stream.read_exact(&mut header[n..])?;
    }
    let len = u32::from_be_bytes(header) as usize;
    if len > MAX_PAYLOAD_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "payload too large",
        ));
    }
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload)?;
    Ok((payload, fds))
}

fn send_with_fds(stream: &UnixStream, data: &[u8], fds: &[RawFd]) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let mut iov = libc::iovec {
        iov_base: data.as_ptr() as *mut libc::c_void,
        iov_len: data.len(),
    };
    let space = unsafe { libc::CMSG_SPACE(std::mem::size_of_val(fds) as u32) } as usize;
    let mut control = vec![0u8; space.max(1)];
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    if !fds.is_empty() {
        msg.msg_control = control.as_mut_ptr() as *mut libc::c_void;
        msg.msg_controllen = space as _;
        let cmsg = unsafe { libc::CMSG_FIRSTHDR(&msg) };
        unsafe {
            (*cmsg).cmsg_level = libc::SOL_SOCKET;
            (*cmsg).cmsg_type = libc::SCM_RIGHTS;
            (*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of_val(fds) as u32) as _;
            let dest = libc::CMSG_DATA(cmsg) as *mut RawFd;
            for (index, fd) in fds.iter().enumerate() {
                std::ptr::write_unaligned(dest.add(index), *fd);
            }
        }
    }
    loop {
        let sent = unsafe { libc::sendmsg(stream.as_raw_fd(), &msg, 0) };
        if sent < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if sent as usize != data.len() {
            return Err(io::Error::new(io::ErrorKind::WriteZero, "short sendmsg"));
        }
        return Ok(());
    }
}

fn recv_with_fds(
    stream: &UnixStream,
    buf: &mut [u8],
    max_fds: usize,
) -> io::Result<(usize, Vec<OwnedFd>)> {
    use std::os::unix::io::AsRawFd;
    let mut iov = libc::iovec {
        iov_base: buf.as_mut_ptr() as *mut libc::c_void,
        iov_len: buf.len(),
    };
    let space =
        unsafe { libc::CMSG_SPACE((max_fds * std::mem::size_of::<RawFd>()) as u32) } as usize;
    let mut control = vec![0u8; space];
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr() as *mut libc::c_void;
    msg.msg_controllen = space as _;
    #[cfg(target_os = "linux")]
    let flags = libc::MSG_CMSG_CLOEXEC;
    #[cfg(not(target_os = "linux"))]
    let flags = 0;
    let received = loop {
        let n = unsafe { libc::recvmsg(stream.as_raw_fd(), &mut msg, flags) };
        if n < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        break n as usize;
    };
    let mut fds = Vec::new();
    let mut cmsg = unsafe { libc::CMSG_FIRSTHDR(&msg) };
    while !cmsg.is_null() {
        let header = unsafe { &*cmsg };
        if header.cmsg_level == libc::SOL_SOCKET && header.cmsg_type == libc::SCM_RIGHTS {
            let data_len = header.cmsg_len as usize - unsafe { libc::CMSG_LEN(0) } as usize;
            let count = data_len / std::mem::size_of::<RawFd>();
            let data = unsafe { libc::CMSG_DATA(cmsg) } as *const RawFd;
            for index in 0..count {
                let fd = unsafe { std::ptr::read_unaligned(data.add(index)) };
                #[cfg(not(target_os = "linux"))]
                unsafe {
                    libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
                }
                fds.push(unsafe { OwnedFd::from_raw_fd(fd) });
            }
        }
        cmsg = unsafe { libc::CMSG_NXTHDR(&msg, cmsg) };
    }
    if msg.msg_flags & libc::MSG_CTRUNC != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "fd list truncated",
        ));
    }
    Ok((received, fds))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::io::AsRawFd;

    #[test]
    fn round_trips_payload_and_fds() {
        let (a, mut b) = UnixStream::pair().unwrap();
        let (mut p1, p2) = UnixStream::pair().unwrap();
        let (q1, mut q2) = UnixStream::pair().unwrap();
        let payload = vec![7u8; 100_000];
        // The payload exceeds the socket buffer: send from another thread so
        // the receive side drains it, exactly as the two cores do.
        let p2_fd = p2.as_raw_fd();
        let q1_fd = q1.as_raw_fd();
        let sender_payload = payload.clone();
        let sender = std::thread::spawn(move || {
            let mut a = a;
            send_message(&mut a, &sender_payload, &[p2_fd, q1_fd]).unwrap();
            send_message(&mut a, b"second", &[]).unwrap();
            a
        });
        let (got, fds) = recv_message(&mut b).unwrap();
        assert_eq!(got, payload);
        assert_eq!(fds.len(), 2);
        // The received fds are live copies of the originals.
        let mut moved_p2: UnixStream = fds[0].try_clone().unwrap().into();
        moved_p2.write_all(b"hi").unwrap();
        let mut buf = [0u8; 2];
        p1.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"hi");
        let mut moved_q1: UnixStream = fds[1].try_clone().unwrap().into();
        moved_q1.write_all(b"yo").unwrap();
        q2.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"yo");
        let (got, fds) = recv_message(&mut b).unwrap();
        assert_eq!(got, b"second");
        assert!(fds.is_empty());
        let _a = sender.join().unwrap();
    }

    #[test]
    fn many_fds_in_one_message() {
        let (mut a, mut b) = UnixStream::pair().unwrap();
        let pairs: Vec<(UnixStream, UnixStream)> =
            (0..16).map(|_| UnixStream::pair().unwrap()).collect();
        let fds: Vec<RawFd> = pairs.iter().map(|(x, _)| x.as_raw_fd()).collect();
        send_message(&mut a, b"x", &fds).unwrap();
        let (_, got) = recv_message(&mut b).unwrap();
        assert_eq!(got.len(), 16);
    }

    #[test]
    fn rejects_too_many_fds() {
        let (mut a, _b) = UnixStream::pair().unwrap();
        let fds = vec![0; MAX_FDS_PER_MESSAGE + 1];
        assert!(send_message(&mut a, b"x", &fds).is_err());
    }

    #[test]
    fn peer_close_is_eof() {
        let (a, mut b) = UnixStream::pair().unwrap();
        drop(a);
        assert_eq!(
            recv_message(&mut b).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }
}
