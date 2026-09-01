//! Datagram socket wrapper with ECN support.
//!
//! This is a near-literal port of `udpsocket.h/.cpp` from the reference C++
//! project. The key behavior is:
//!
//! - Uses **numeric** IPv4/IPv6 addresses only (no DNS lookups), matching
//!   `inet_pton` behavior in the original.
//! - Receives ECN via ancillary data (`recvmsg`) on Unix platforms.
//! - Sets ECN on send via ancillary data (`sendmsg`) on Unix platforms.
//! - Uses `select()` for timeouts in microseconds.
//!
//! The Rust implementation avoids self-referential structs by constructing
//! `msghdr/iovec` on the stack per call. This keeps the code safe and still
//! extremely fast (no heap allocations in the send/recv hot path).

use crate::congestion::{ecn_tp, size_tp, time_tp};
use crate::core::UdpSocketError;

#[cfg(unix)]
use core::{fmt, mem};

/// Socket backend support status for the current compilation target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SocketPlatformSupport {
    /// Unix-like targets are the primary implementation and support full ECN handling.
    Full,
    /// Windows builds work, but some ECN setup paths are best-effort and depend on OS support.
    Partial,
    /// Other targets compile, but the UDP socket backend is not implemented.
    Unsupported,
}

impl SocketPlatformSupport {
    /// Returns whether the UDP socket backend is available on this target.
    pub const fn is_available(self) -> bool {
        !matches!(self, SocketPlatformSupport::Unsupported)
    }
}

/// Holds a resolved socket address (IPv4 or IPv6) and its length.
#[cfg(unix)]
#[derive(Clone, Copy)]
pub struct Endpoint {
    pub sa: libc::sockaddr_storage,
    pub len: libc::socklen_t,
}

#[cfg(unix)]
impl fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Endpoint")
            .field("family", &self.family())
            .field("len", &self.len)
            .finish()
    }
}

#[cfg(unix)]
impl Endpoint {
    /// Returns `true` if this is an IPv4 address.
    #[inline]
    pub fn is_v4(&self) -> bool {
        self.sa.ss_family as i32 == libc::AF_INET
    }
    /// Returns `true` if this is an IPv6 address.
    #[inline]
    pub fn is_v6(&self) -> bool {
        self.sa.ss_family as i32 == libc::AF_INET6
    }
    /// Address family.
    #[inline]
    pub fn family(&self) -> i32 {
        self.sa.ss_family as i32
    }

    #[inline]
    fn empty() -> Self {
        Self {
            sa: zeroed_sockaddr_storage_unix(),
            len: 0,
        }
    }
}

/// Platform-abstracted socket handle.
#[cfg(unix)]
pub type SocketHandle = libc::c_int;

#[cfg(windows)]
#[derive(Clone, Copy, Debug)]
pub struct Endpoint {
    pub sa: win::SOCKADDR_STORAGE,
    pub len: win::socklen_t,
}

#[cfg(windows)]
impl Endpoint {
    /// Returns `true` if this is an IPv4 address.
    #[inline]
    pub fn is_v4(&self) -> bool {
        self.sa.ss_family as i32 == win::AF_INET
    }

    /// Returns `true` if this is an IPv6 address.
    #[inline]
    pub fn is_v6(&self) -> bool {
        self.sa.ss_family as i32 == win::AF_INET6
    }

    /// Address family.
    #[inline]
    pub fn family(&self) -> i32 {
        self.sa.ss_family as i32
    }

    #[inline]
    fn empty() -> Self {
        Self {
            sa: zeroed_sockaddr_storage_windows(),
            len: 0,
        }
    }
}

/// Platform-abstracted socket handle.
#[cfg(windows)]
pub type SocketHandle = win::SOCKET;

#[cfg(not(any(unix, windows)))]
#[derive(Clone, Copy, Debug)]
pub struct Endpoint {
    _priv: (),
}

#[cfg(not(any(unix, windows)))]
pub type SocketHandle = usize;

#[cfg(unix)]
#[derive(Debug)]
#[repr(align(8))]
struct UnixControlBuffer([u8; CTRL_BUF_SIZE_UNIX]);

#[cfg(unix)]
impl UnixControlBuffer {
    #[inline]
    fn new() -> Self {
        Self([0u8; CTRL_BUF_SIZE_UNIX])
    }

    #[inline]
    fn as_mut_c_void(&mut self) -> *mut libc::c_void {
        self.0.as_mut_ptr() as *mut libc::c_void
    }

    #[inline]
    fn len(&self) -> usize {
        self.0.len()
    }
}

#[cfg(unix)]
struct UnixIoState {
    send_iov: libc::iovec,
    recv_iov: libc::iovec,
    send_msg: libc::msghdr,
    recv_msg: libc::msghdr,
    send_ctrl: UnixControlBuffer,
    recv_ctrl: UnixControlBuffer,
}

#[cfg(unix)]
impl core::fmt::Debug for UnixIoState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("UnixIoState").finish_non_exhaustive()
    }
}

#[cfg(unix)]
impl UnixIoState {
    fn new() -> Box<Self> {
        let mut state = Box::new(Self {
            send_iov: zeroed_iovec_unix(),
            recv_iov: zeroed_iovec_unix(),
            send_msg: zeroed_msghdr_unix(),
            recv_msg: zeroed_msghdr_unix(),
            send_ctrl: UnixControlBuffer::new(),
            recv_ctrl: UnixControlBuffer::new(),
        });
        state.init();
        state
    }

    fn init(&mut self) {
        self.send_msg.msg_iov = &mut self.send_iov as *mut libc::iovec;
        self.send_msg.msg_iovlen = 1;
        self.send_msg.msg_control = self.send_ctrl.as_mut_c_void();
        self.send_msg.msg_controllen = self.send_ctrl.len();

        self.recv_msg.msg_iov = &mut self.recv_iov as *mut libc::iovec;
        self.recv_msg.msg_iovlen = 1;
        self.recv_msg.msg_control = self.recv_ctrl.as_mut_c_void();
        self.recv_msg.msg_controllen = self.recv_ctrl.len();
    }
}

/// UDP socket wrapper.
#[derive(Debug)]
pub struct UDPSocket {
    // --- Unix backend (Linux/macOS/BSD) ---
    #[cfg(unix)]
    socket: SocketHandle,
    #[cfg(unix)]
    peer: Endpoint,
    #[cfg(unix)]
    connected: bool,
    #[cfg(unix)]
    unix_io: Box<UnixIoState>,

    // --- Windows backend ---
    #[cfg(windows)]
    socket: SocketHandle,
    #[cfg(windows)]
    peer: Endpoint,
    #[cfg(windows)]
    connected: bool,
    #[cfg(windows)]
    wsa_acquired: bool,
    #[cfg(windows)]
    wsa_recv_msg: Option<win::LPFN_WSARECVMSG>,
    #[cfg(windows)]
    wsa_send_msg: Option<win::LPFN_WSASENDMSG>,
    /// Control buffer storage reused across sends (no per-call allocation).
    #[cfg(windows)]
    send_ctrl: [u8; win::CTRL_BUF_SIZE_WIN],
    /// Control buffer storage reused across receives (no per-call allocation).
    #[cfg(windows)]
    recv_ctrl: [u8; win::CTRL_BUF_SIZE_WIN],

    // --- Unsupported / other targets ---
    #[cfg(not(any(unix, windows)))]
    _priv: (),
}

impl Default for UDPSocket {
    fn default() -> Self {
        Self::new()
    }
}

impl UDPSocket {
    /// Return the socket backend support status for the current target.
    pub const fn platform_support() -> SocketPlatformSupport {
        if cfg!(unix) {
            SocketPlatformSupport::Full
        } else if cfg!(windows) {
            SocketPlatformSupport::Partial
        } else {
            SocketPlatformSupport::Unsupported
        }
    }

    /// Construct a new socket wrapper.
    pub fn new() -> Self {
        #[cfg(unix)]
        {
            set_max_priority_unix();
            Self {
                socket: invalid_socket_unix(),
                peer: Endpoint::empty(),
                connected: false,
                unix_io: UnixIoState::new(),
            }
        }
        #[cfg(windows)]
        {
            Self {
                socket: win::INVALID_SOCKET,
                peer: Endpoint::empty(),
                connected: false,
                wsa_acquired: false,
                wsa_recv_msg: None,
                wsa_send_msg: None,
                send_ctrl: [0u8; win::CTRL_BUF_SIZE_WIN],
                recv_ctrl: [0u8; win::CTRL_BUF_SIZE_WIN],
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            Self { _priv: () }
        }
    }

    /// Bind the socket to a local address.
    pub fn Bind(&mut self, addr: &str, port: u16) -> Result<(), UdpSocketError> {
        #[cfg(unix)]
        {
            self.close_if_open();
            let ep = resolve_endpoint_unix(addr, port)?;
            let s = make_socket_unix(ep.family())?;
            enable_recv_ecn_unix(s, ep.family())?;
            // SAFETY: `s` is a valid datagram socket, and `ep` owns a fully initialized
            // sockaddr storage whose address remains stable for the duration of the call.
            let rc = unsafe { libc::bind(s, &ep.sa as *const _ as *const libc::sockaddr, ep.len) };
            if rc != 0 {
                let code = last_errno_unix();
                // SAFETY: `s` is still owned by this function on the bind failure path.
                unsafe { libc::close(s) };
                return Err(UdpSocketError::Syscall { call: "bind", code });
            }

            self.socket = s;
            self.connected = false;
            Ok(())
        }
        #[cfg(windows)]
        {
            self.close_if_open_windows();

            if !self.wsa_acquired {
                win::winsock_acquire()?;
                self.wsa_acquired = true;
            }

            let ep = win::resolve_endpoint_windows(addr, port)?;
            let sck = win::make_socket_windows(ep.family())?;

            // Best-effort: enable receive ECN if available on this Windows build.
            let _ = win::enable_recv_ecn_windows(sck);

            // Load WSARecvMsg/WSASendMsg extension function pointers (best-effort).
            match win::load_msg_fns_windows(sck) {
                Ok((r, w)) => {
                    self.wsa_recv_msg = Some(r);
                    self.wsa_send_msg = Some(w);
                }
                Err(_) => {
                    self.wsa_recv_msg = None;
                    self.wsa_send_msg = None;
                }
            }

            // SAFETY: `sck` is a valid socket and `ep` owns initialized storage that lives
            // across the FFI call.
            let rc = unsafe { win::bind(sck, &ep.sa as *const _ as *const win::SOCKADDR, ep.len) };
            if rc == win::SOCKET_ERROR {
                let code = win::last_error_code_windows();
                // SAFETY: the freshly created socket has not been transferred elsewhere.
                unsafe { win::closesocket(sck) };
                return Err(UdpSocketError::Syscall { call: "bind", code });
            }

            self.socket = sck;
            self.peer = Endpoint::empty();
            self.connected = false;
            Ok(())
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (addr, port);
            Err(UdpSocketError::NotSupported("Bind"))
        }
    }

    /// Connect the socket to a peer address.
    pub fn Connect(&mut self, addr: &str, port: u16) -> Result<(), UdpSocketError> {
        #[cfg(unix)]
        {
            self.close_if_open();
            let peer = resolve_endpoint_unix(addr, port)?;
            let s = make_socket_unix(peer.family())?;
            enable_recv_ecn_unix(s, peer.family())?;

            // SAFETY: `s` is a valid datagram socket and `peer` holds the fully initialized
            // address we want the kernel to copy from during connect.
            let rc = unsafe {
                libc::connect(s, &peer.sa as *const _ as *const libc::sockaddr, peer.len)
            };
            if rc != 0 {
                let code = last_errno_unix();
                // SAFETY: `s` is still owned locally on the error path.
                unsafe { libc::close(s) };
                return Err(UdpSocketError::Syscall {
                    call: "connect",
                    code,
                });
            }

            self.socket = s;
            self.peer = peer;
            self.connected = true;
            Ok(())
        }
        #[cfg(windows)]
        {
            self.close_if_open_windows();

            if !self.wsa_acquired {
                win::winsock_acquire()?;
                self.wsa_acquired = true;
            }

            let peer = win::resolve_endpoint_windows(addr, port)?;
            let sck = win::make_socket_windows(peer.family())?;

            let _ = win::enable_recv_ecn_windows(sck);

            match win::load_msg_fns_windows(sck) {
                Ok((r, w)) => {
                    self.wsa_recv_msg = Some(r);
                    self.wsa_send_msg = Some(w);
                }
                Err(_) => {
                    self.wsa_recv_msg = None;
                    self.wsa_send_msg = None;
                }
            }

            // SAFETY: `sck` is valid and `peer` outlives the FFI call.
            let rc = unsafe {
                win::connect(sck, &peer.sa as *const _ as *const win::SOCKADDR, peer.len)
            };
            if rc == win::SOCKET_ERROR {
                let code = win::last_error_code_windows();
                // SAFETY: the temporary socket has not been handed off elsewhere.
                unsafe { win::closesocket(sck) };
                return Err(UdpSocketError::Syscall {
                    call: "connect",
                    code,
                });
            }

            self.socket = sck;
            self.peer = peer;
            self.connected = true;
            Ok(())
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (addr, port);
            Err(UdpSocketError::NotSupported("Connect"))
        }
    }

    /// Receive a datagram.
    ///
    /// Returns the number of bytes received. If `timeout > 0` and no packet was
    /// available, returns `Ok(0)`.
    pub fn Receive(
        &mut self,
        buf: &mut [u8],
        ecn: &mut ecn_tp,
        timeout: time_tp,
    ) -> Result<size_tp, UdpSocketError> {
        #[cfg(unix)]
        {
            if buf.is_empty() {
                return Err(UdpSocketError::InvalidInput("len must be > 0"));
            }
            if !is_socket_valid_unix(self.socket) {
                return Err(UdpSocketError::InvalidInput("socket not initialized"));
            }
            if timeout < 0 {
                return Err(UdpSocketError::InvalidInput("timeout must be >= 0"));
            }

            // Match the reference behavior: timeout=0 blocks in `recvmsg`, while
            // a positive timeout performs a readiness wait and returns 0 on expiry.
            if timeout > 0 && !wait_for_readable_unix(self.socket, timeout)? {
                return Ok(0);
            }

            let connected = self.connected;
            let peer = &mut self.peer;
            let io = self.unix_io.as_mut();

            io.recv_iov.iov_base = buf.as_mut_ptr() as *mut libc::c_void;
            io.recv_iov.iov_len = buf.len();
            io.recv_msg.msg_controllen = io.recv_ctrl.len();
            io.recv_msg.msg_flags = 0;

            // On unconnected sockets, recvmsg writes sender address into msg_name.
            if !connected {
                io.recv_msg.msg_name = &mut peer.sa as *mut _ as *mut libc::c_void;
                io.recv_msg.msg_namelen =
                    mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
            } else {
                io.recv_msg.msg_name = core::ptr::null_mut();
                io.recv_msg.msg_namelen = 0;
            }

            // SAFETY: the msghdr/iovec/control buffers all point into `io` and `buf`, which
            // remain alive and exclusively borrowed for the duration of the syscall.
            let r = unsafe { libc::recvmsg(self.socket, &mut io.recv_msg as *mut libc::msghdr, 0) };
            if r < 0 {
                let code = last_errno_unix();
                // Match the C++ behavior: interrupted receive is treated like a timeout.
                if code == libc::EINTR {
                    return Ok(0);
                }
                return Err(UdpSocketError::Syscall {
                    call: "recvmsg",
                    code,
                });
            }

            if !connected {
                peer.len = io.recv_msg.msg_namelen;
            }

            // Iterate over all control messages.
            // The reference implementation treats any non-ECN cmsg as fatal.
            let mut any = false;
            for_each_cmsg_unix(&io.recv_msg, |cmsg| {
                any = true;
                match parse_ecn_cmsg_unix(cmsg, ecn) {
                    Ok(true) => Ok(()),
                    Ok(false) => {
                        let (level, ty, _) = cmsg_metadata_unix(cmsg);
                        Err(UdpSocketError::UnexpectedControlMessage { level, ty })
                    }
                    Err(e) => Err(e),
                }
            })?;

            if !any {
                // If the platform does not provide ECN cmsgs, keep ecn as-is.
            }

            Ok(r as size_tp)
        }
        #[cfg(windows)]
        {
            if buf.is_empty() {
                return Err(UdpSocketError::InvalidInput("len must be > 0"));
            }
            if !win::is_socket_valid_windows(self.socket) {
                return Err(UdpSocketError::InvalidInput("socket not initialized"));
            }
            if timeout < 0 {
                return Err(UdpSocketError::InvalidInput("timeout must be >= 0"));
            }

            // Match the Unix/reference behavior: timeout=0 blocks in recv, while
            // a positive timeout performs a readiness wait and returns 0 on expiry.
            if timeout > 0 && !win::wait_for_readable_windows(self.socket, timeout)? {
                return Ok(0);
            }

            // Preferred path: WSARecvMsg (ECN control messages).
            if let Some(recv_fn) = self.wsa_recv_msg {
                let mut data_buf = win::WSABUF {
                    len: buf.len().min(u32::MAX as usize) as u32,
                    buf: buf.as_mut_ptr() as *mut i8,
                };

                let ctrl_buf = win::WSABUF {
                    len: self.recv_ctrl.len() as u32,
                    buf: self.recv_ctrl.as_mut_ptr() as *mut i8,
                };

                let mut msg = win::WSAMSG {
                    name: core::ptr::null_mut(),
                    namelen: 0,
                    lpBuffers: &mut data_buf as *mut win::WSABUF,
                    dwBufferCount: 1,
                    Control: ctrl_buf,
                    dwFlags: 0,
                };

                if !self.connected {
                    msg.name = &mut self.peer.sa as *mut _ as *mut win::SOCKADDR;
                    msg.namelen = core::mem::size_of::<win::SOCKADDR_STORAGE>() as win::socklen_t;
                }

                let mut num_bytes: u32 = 0;
                // SAFETY: `msg` references live buffers owned by `self` and `buf`, and the
                // completion/overlapped pointers are explicitly null because this call is sync.
                let rc = unsafe {
                    recv_fn(
                        self.socket,
                        &mut msg as *mut win::WSAMSG,
                        &mut num_bytes as *mut u32,
                        core::ptr::null_mut(),
                        core::ptr::null_mut(),
                    )
                };
                if rc == win::SOCKET_ERROR {
                    return Err(UdpSocketError::Syscall {
                        call: "WSARecvMsg",
                        code: win::last_error_code_windows(),
                    });
                }

                if !self.connected {
                    self.peer.len = msg.namelen;
                }

                let mut cmsg = win::wsa_cmsg_firsthdr(&msg);
                while !cmsg.is_null() {
                    if win::parse_ecn_cmsg_windows(cmsg, ecn) {
                        break;
                    }
                    cmsg = win::wsa_cmsg_nxthdr(&msg, cmsg);
                }

                return Ok(num_bytes as size_tp);
            }

            // Fallback: recv()/recvfrom() without cmsgs (ECN unknown).
            let mut from_len: win::socklen_t =
                core::mem::size_of::<win::SOCKADDR_STORAGE>() as win::socklen_t;
            // SAFETY: the buffer pointers and optional source-address storage stay valid for the
            // duration of the blocking Winsock call.
            let rc = unsafe {
                if self.connected {
                    win::recv(
                        self.socket,
                        buf.as_mut_ptr() as *mut i8,
                        buf.len().min(i32::MAX as usize) as i32,
                        0,
                    )
                } else {
                    win::recvfrom(
                        self.socket,
                        buf.as_mut_ptr() as *mut i8,
                        buf.len().min(i32::MAX as usize) as i32,
                        0,
                        &mut self.peer.sa as *mut _ as *mut win::SOCKADDR,
                        &mut from_len as *mut win::socklen_t,
                    )
                }
            };
            if rc == win::SOCKET_ERROR {
                return Err(UdpSocketError::Syscall {
                    call: "recvfrom",
                    code: win::last_error_code_windows(),
                });
            }
            if !self.connected {
                self.peer.len = from_len;
            }
            Ok(rc as size_tp)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (buf, ecn, timeout);
            Err(UdpSocketError::NotSupported("Receive"))
        }
    }

    /// Send a datagram.
    pub fn Send(
        &mut self,
        buf: &[u8],
        len: size_tp,
        ecn: ecn_tp,
    ) -> Result<size_tp, UdpSocketError> {
        #[cfg(unix)]
        {
            let max_len = buf.len() as u64;
            if len == 0 || len > max_len {
                return Err(UdpSocketError::InvalidInput("len out of range"));
            }
            if !is_socket_valid_unix(self.socket) {
                return Err(UdpSocketError::InvalidInput("socket not initialized"));
            }

            // Match C++ assert on ECN values.
            match ecn {
                ecn_tp::ecn_not_ect | ecn_tp::ecn_ect0 | ecn_tp::ecn_l4s_id | ecn_tp::ecn_ce => {}
            }

            let connected = self.connected;
            let peer = &mut self.peer;
            let io = self.unix_io.as_mut();

            io.send_iov.iov_base = buf.as_ptr() as *mut libc::c_void;
            io.send_iov.iov_len = len as usize;
            io.send_msg.msg_controllen = io.send_ctrl.len();

            if connected {
                io.send_msg.msg_name = core::ptr::null_mut();
                io.send_msg.msg_namelen = 0;
            } else {
                io.send_msg.msg_name = &mut peer.sa as *mut _ as *mut libc::c_void;
                io.send_msg.msg_namelen = peer.len;
            }

            // Fill one ECN cmsg.
            let cmsg = first_cmsg_unix(&mut io.send_msg)
                .ok_or(UdpSocketError::InvalidInput("control buffer too small"))?;
            fill_ecn_cmsg_unix(cmsg, peer.family(), ecn)?;

            // SAFETY: the msghdr/iovec/control buffers all point into `io` and `buf`, which
            // remain alive for the duration of the syscall.
            let rc = unsafe { libc::sendmsg(self.socket, &io.send_msg as *const libc::msghdr, 0) };
            if rc < 0 {
                return Err(UdpSocketError::Syscall {
                    call: "sendmsg",
                    code: last_errno_unix(),
                });
            }
            Ok(rc as size_tp)
        }
        #[cfg(windows)]
        {
            let max_len = buf.len() as u64;
            if len == 0 || len > max_len {
                return Err(UdpSocketError::InvalidInput("len out of range"));
            }
            if !win::is_socket_valid_windows(self.socket) {
                return Err(UdpSocketError::InvalidInput("socket not initialized"));
            }

            // Match C++ assert on ECN values.
            match ecn {
                ecn_tp::ecn_not_ect | ecn_tp::ecn_ect0 | ecn_tp::ecn_l4s_id | ecn_tp::ecn_ce => {}
            }

            // Preferred path: WSASendMsg (ECN control messages).
            if let Some(send_fn) = self.wsa_send_msg {
                let mut data_buf = win::WSABUF {
                    len: (len as usize).min(u32::MAX as usize) as u32,
                    buf: buf.as_ptr() as *mut i8,
                };

                let ctrl_buf = win::WSABUF {
                    len: self.send_ctrl.len() as u32,
                    buf: self.send_ctrl.as_mut_ptr() as *mut i8,
                };

                let mut msg = win::WSAMSG {
                    name: core::ptr::null_mut(),
                    namelen: 0,
                    lpBuffers: &mut data_buf as *mut win::WSABUF,
                    dwBufferCount: 1,
                    Control: ctrl_buf,
                    dwFlags: 0,
                };

                if !self.connected {
                    if self.peer.len == 0 {
                        return Err(UdpSocketError::InvalidInput(
                            "peer not known (call Receive first or use Connect)",
                        ));
                    }
                    msg.name = &mut self.peer.sa as *mut _ as *mut win::SOCKADDR;
                    msg.namelen = self.peer.len;
                }

                let cmsg = win::wsa_cmsg_firsthdr(&msg);
                if cmsg.is_null() {
                    return Err(UdpSocketError::InvalidInput("control buffer too small"));
                }
                win::fill_ecn_cmsg_windows(cmsg, self.peer.family(), ecn);

                let mut num_bytes: u32 = 0;
                // SAFETY: `msg` points at live buffers and this synchronous call does not use
                // overlapped I/O, so the null completion pointers are correct.
                let rc = unsafe {
                    send_fn(
                        self.socket,
                        &mut msg as *mut win::WSAMSG,
                        0,
                        &mut num_bytes as *mut u32,
                        core::ptr::null_mut(),
                        core::ptr::null_mut(),
                    )
                };
                if rc == win::SOCKET_ERROR {
                    return Err(UdpSocketError::Syscall {
                        call: "WSASendMsg",
                        code: win::last_error_code_windows(),
                    });
                }
                return Ok(num_bytes as size_tp);
            }

            // Fallback: send()/sendto() without cmsgs (ECN not set).
            // SAFETY: the buffer pointers and optional destination-address storage remain valid
            // for the duration of the Winsock call.
            let rc = unsafe {
                if self.connected {
                    win::send(
                        self.socket,
                        buf.as_ptr() as *const i8,
                        (len as usize).min(i32::MAX as usize) as i32,
                        0,
                    )
                } else {
                    if self.peer.len == 0 {
                        return Err(UdpSocketError::InvalidInput(
                            "peer not known (call Receive first or use Connect)",
                        ));
                    }
                    win::sendto(
                        self.socket,
                        buf.as_ptr() as *const i8,
                        (len as usize).min(i32::MAX as usize) as i32,
                        0,
                        &self.peer.sa as *const _ as *const win::SOCKADDR,
                        self.peer.len,
                    )
                }
            };
            if rc == win::SOCKET_ERROR {
                return Err(UdpSocketError::Syscall {
                    call: "sendto",
                    code: win::last_error_code_windows(),
                });
            }
            Ok(rc as size_tp)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (buf, len, ecn);
            Err(UdpSocketError::NotSupported("Send"))
        }
    }

    #[cfg(unix)]
    fn close_if_open(&mut self) {
        if is_socket_valid_unix(self.socket) {
            // SAFETY: this type owns the file descriptor and invalidates it immediately after.
            unsafe { libc::close(self.socket) };
            self.socket = invalid_socket_unix();
            self.connected = false;
        }
    }

    #[cfg(windows)]
    fn close_if_open_windows(&mut self) {
        if win::is_socket_valid_windows(self.socket) {
            // SAFETY: this type owns the socket handle and invalidates it immediately after.
            unsafe { win::closesocket(self.socket) };
            self.socket = win::INVALID_SOCKET;
            self.connected = false;
        }
    }
}

impl Drop for UDPSocket {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            self.close_if_open();
        }
        #[cfg(windows)]
        {
            self.close_if_open_windows();
            if self.wsa_acquired {
                win::winsock_release();
                self.wsa_acquired = false;
            }
        }
    }
}

// ===== Unix implementation helpers =====

#[cfg(unix)]
#[inline]
fn invalid_socket_unix() -> SocketHandle {
    -1
}

#[cfg(unix)]
#[inline]
fn zeroed_sockaddr_storage_unix() -> libc::sockaddr_storage {
    // SAFETY: `sockaddr_storage` is a plain C storage container and zero is a valid baseline
    // before we write the active family-specific fields.
    unsafe { mem::zeroed() }
}

#[cfg(unix)]
#[inline]
fn zeroed_iovec_unix() -> libc::iovec {
    // SAFETY: a zeroed `iovec` is a valid placeholder until we assign `iov_base`/`iov_len`.
    unsafe { mem::zeroed() }
}

#[cfg(unix)]
#[inline]
fn zeroed_msghdr_unix() -> libc::msghdr {
    // SAFETY: a zeroed `msghdr` is a valid starting point before we wire in the owned iovec and
    // control buffers.
    unsafe { mem::zeroed() }
}

#[cfg(unix)]
#[inline]
fn zeroed_fd_set_unix() -> libc::fd_set {
    // SAFETY: `fd_set` is expected to be zero-initialized before `FD_ZERO`/`FD_SET` mutate it.
    unsafe { mem::zeroed() }
}

#[cfg(unix)]
#[inline]
fn zeroed_sched_param_unix() -> libc::sched_param {
    // SAFETY: `sched_param` is a POD C struct and zero is the documented default for fields we
    // do not explicitly populate.
    unsafe { mem::zeroed() }
}

#[cfg(windows)]
#[inline]
fn zeroed_sockaddr_storage_windows() -> win::SOCKADDR_STORAGE {
    // SAFETY: `SOCKADDR_STORAGE` is a plain Winsock storage container and zero is a valid empty
    // baseline before the family-specific fields are written.
    unsafe { core::mem::zeroed() }
}

#[cfg(unix)]
#[inline]
fn is_socket_valid_unix(s: SocketHandle) -> bool {
    s >= 0
}

#[cfg(any(target_os = "linux", target_os = "android"))]
#[inline]
fn last_errno_unix() -> i32 {
    // Safety: libc guarantees errno is thread-local.
    unsafe { *libc::__errno_location() }
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
#[inline]
fn last_errno_unix() -> i32 {
    // Portable fallback for non-Linux Unix. libc exposes errno via `__error` on macOS/BSD.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    // SAFETY: libc exposes errno through a thread-local pointer on these targets.
    unsafe {
        *libc::__error()
    }
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    // SAFETY: libc guarantees errno is thread-local on supported Unix targets.
    unsafe {
        *libc::__errno_location()
    }
}

#[cfg(unix)]
fn wait_for_readable_unix(s: SocketHandle, timeout: time_tp) -> Result<bool, UdpSocketError> {
    if !is_socket_valid_unix(s) {
        return Err(UdpSocketError::InvalidInput("invalid socket"));
    }
    if timeout < 0 {
        return Err(UdpSocketError::InvalidInput("timeout must be >= 0"));
    }

    // Build fd_set.
    let mut rfds: libc::fd_set = zeroed_fd_set_unix();
    // SAFETY: `rfds` is a live stack allocation and `s` has already been validated.
    unsafe {
        libc::FD_ZERO(&mut rfds);
        libc::FD_SET(s, &mut rfds);
    }

    let mut tv = libc::timeval {
        tv_sec: (timeout as i64 / 1_000_000) as libc::time_t,
        tv_usec: (timeout as i64 % 1_000_000) as libc::suseconds_t,
    };

    // SAFETY: all pointers either reference live stack storage or are null as allowed by `select`.
    let r = unsafe {
        libc::select(
            s + 1,
            &mut rfds,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            &mut tv,
        )
    };
    if r < 0 {
        let code = last_errno_unix();
        // Mirror the reference behavior: interrupted wait behaves like "not readable".
        if code == libc::EINTR {
            return Ok(false);
        }
        return Err(UdpSocketError::Syscall {
            call: "select",
            code,
        });
    }
    Ok(r > 0)
}

#[cfg(unix)]
fn make_socket_unix(family: i32) -> Result<SocketHandle, UdpSocketError> {
    if family != libc::AF_INET && family != libc::AF_INET6 {
        return Err(UdpSocketError::Syscall {
            call: "socket",
            code: libc::EAFNOSUPPORT,
        });
    }
    // SAFETY: arguments are plain values and `family` has been validated above.
    let s = unsafe { libc::socket(family, libc::SOCK_DGRAM, 0) };
    if !is_socket_valid_unix(s) {
        return Err(UdpSocketError::Syscall {
            call: "socket",
            code: last_errno_unix(),
        });
    }
    Ok(s)
}

#[cfg(unix)]
fn enable_recv_ecn_unix(s: SocketHandle, family: i32) -> Result<(), UdpSocketError> {
    if !is_socket_valid_unix(s) {
        return Err(UdpSocketError::InvalidInput("invalid socket"));
    }
    let set: libc::c_int = 1;

    let rc = match family {
        x if x == libc::AF_INET => unsafe {
            libc::setsockopt(
                s,
                libc::IPPROTO_IP,
                libc::IP_RECVTOS,
                &set as *const _ as *const libc::c_void,
                mem::size_of_val(&set) as libc::socklen_t,
            )
        },
        x if x == libc::AF_INET6 => unsafe {
            libc::setsockopt(
                s,
                libc::IPPROTO_IPV6,
                libc::IPV6_RECVTCLASS,
                &set as *const _ as *const libc::c_void,
                mem::size_of_val(&set) as libc::socklen_t,
            )
        },
        _ => {
            return Err(UdpSocketError::Syscall {
                call: "setsockopt",
                code: libc::EAFNOSUPPORT,
            })
        }
    };

    if rc != 0 {
        return Err(UdpSocketError::Syscall {
            call: "setsockopt",
            code: last_errno_unix(),
        });
    }

    Ok(())
}

#[cfg(unix)]
fn resolve_endpoint_unix(addr: &str, port: u16) -> Result<Endpoint, UdpSocketError> {
    use core::mem;
    use std::net::IpAddr;

    if addr.is_empty() {
        return Err(UdpSocketError::InvalidInput("addr must not be empty"));
    }

    // The reference implementation uses `inet_pton`, which accepts numeric IPv4/IPv6.
    // We mirror that behavior using Rust's `IpAddr` parser.
    let ip: IpAddr = addr
        .parse()
        .map_err(|_| UdpSocketError::UnsupportedAddress)?;

    match ip {
        IpAddr::V4(ip4) => {
            let mut sa: libc::sockaddr_storage = zeroed_sockaddr_storage_unix();
            let sin = &mut sa as *mut _ as *mut libc::sockaddr_in;
            // SAFETY: `sin` points into the local `sockaddr_storage`, which is large enough for
            // `sockaddr_in`, and we write every field required by the kernel.
            unsafe {
                (*sin).sin_family = libc::AF_INET as libc::sa_family_t;
                (*sin).sin_port = port.to_be();
                // `sin_addr.s_addr` is expected to occupy memory in network byte order.
                // Build the integer from native-endian bytes so the stored memory bytes
                // match the dotted-quad octets on both little- and big-endian hosts.
                (*sin).sin_addr = libc::in_addr {
                    s_addr: u32::from_ne_bytes(ip4.octets()),
                };
            }
            Ok(Endpoint {
                sa,
                len: mem::size_of::<libc::sockaddr_in>() as u32,
            })
        }
        IpAddr::V6(ip6) => {
            let mut sa: libc::sockaddr_storage = zeroed_sockaddr_storage_unix();
            let sin6 = &mut sa as *mut _ as *mut libc::sockaddr_in6;
            // SAFETY: `sin6` points into the local `sockaddr_storage`, which is large enough for
            // `sockaddr_in6`, and we write every field required by the kernel.
            unsafe {
                (*sin6).sin6_family = libc::AF_INET6 as libc::sa_family_t;
                (*sin6).sin6_port = port.to_be();
                (*sin6).sin6_flowinfo = 0;
                (*sin6).sin6_scope_id = 0;
                (*sin6).sin6_addr = libc::in6_addr {
                    s6_addr: ip6.octets(),
                };
            }
            Ok(Endpoint {
                sa,
                len: mem::size_of::<libc::sockaddr_in6>() as u32,
            })
        }
    }
}

// ----- cmsg helpers -----

#[cfg(unix)]
const fn cmsg_align_unix(len: usize) -> usize {
    let a = mem::size_of::<libc::c_long>();
    (len + a - 1) & !(a - 1)
}

#[cfg(unix)]
const fn cmsg_space_unix(data_len: usize) -> usize {
    cmsg_align_unix(mem::size_of::<libc::cmsghdr>()) + cmsg_align_unix(data_len)
}

#[cfg(unix)]
const fn cmsg_len_unix(data_len: usize) -> usize {
    cmsg_align_unix(mem::size_of::<libc::cmsghdr>()) + data_len
}

#[cfg(unix)]
const CTRL_BUF_SIZE_UNIX: usize = cmsg_space_unix(mem::size_of::<libc::c_int>());

#[cfg(unix)]
fn first_cmsg_unix(msg: &mut libc::msghdr) -> Option<*mut libc::cmsghdr> {
    if msg.msg_controllen < mem::size_of::<libc::cmsghdr>() {
        return None;
    }
    Some(msg.msg_control as *mut libc::cmsghdr)
}

#[cfg(unix)]
fn for_each_cmsg_unix<F>(msg: &libc::msghdr, mut f: F) -> Result<(), UdpSocketError>
where
    F: FnMut(*mut libc::cmsghdr) -> Result<(), UdpSocketError>,
{
    // Equivalent to CMSG_FIRSTHDR/CMSG_NXTHDR.
    let base = msg.msg_control as *mut u8;
    // SAFETY: `msg_control` points at the control buffer owned by the caller and
    // `msg_controllen` is the initialized byte length for that buffer.
    let end = unsafe { base.add(msg.msg_controllen) };

    let mut cur = base as *mut libc::cmsghdr;
    while (cur as *mut u8) < end {
        let (_, _, c_len) = cmsg_metadata_unix(cur);
        if c_len < mem::size_of::<libc::cmsghdr>() {
            break;
        }
        f(cur)?;

        // Advance to next header using CMSG_ALIGN.
        // SAFETY: `c_len` was read from the current header and alignment mirrors the C macros.
        let next = unsafe { (cur as *mut u8).add(cmsg_align_unix(c_len)) };
        if next >= end {
            break;
        }
        cur = next as *mut libc::cmsghdr;
    }
    Ok(())
}

#[cfg(unix)]
#[inline]
fn decode_ecn_unix(tos_or_tc: i32) -> ecn_tp {
    match (tos_or_tc & 0x03) as u8 {
        0 => ecn_tp::ecn_not_ect,
        1 => ecn_tp::ecn_l4s_id,
        2 => ecn_tp::ecn_ect0,
        _ => ecn_tp::ecn_ce,
    }
}

#[cfg(unix)]
#[inline]
fn encode_ecn_unix(e: ecn_tp) -> i32 {
    (e as i32) & 0x03
}

#[cfg(unix)]
fn ip_recv_cmsg_type_unix(family: i32) -> Result<i32, UdpSocketError> {
    // Normalizes Linux vs macOS differences like the reference macro.
    if family == libc::AF_INET {
        #[cfg(target_os = "linux")]
        {
            Ok(libc::IP_TOS)
        }
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            Ok(libc::IP_RECVTOS)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios")))]
        {
            // Stub for other Unix.
            Ok(libc::IP_TOS)
        }
    } else if family == libc::AF_INET6 {
        #[cfg(target_os = "linux")]
        {
            Ok(libc::IPV6_TCLASS)
        }
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            Ok(libc::IPV6_RECVTCLASS)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios")))]
        {
            Ok(libc::IPV6_TCLASS)
        }
    } else {
        Err(UdpSocketError::Syscall {
            call: "ip_recv_cmsg_type",
            code: libc::EAFNOSUPPORT,
        })
    }
}

#[cfg(unix)]
fn parse_ecn_cmsg_unix(c: *mut libc::cmsghdr, ecn: &mut ecn_tp) -> Result<bool, UdpSocketError> {
    if c.is_null() {
        return Ok(false);
    }
    let (level, ty, cmsg_len) = cmsg_metadata_unix(c);
    let header_len = cmsg_align_unix(mem::size_of::<libc::cmsghdr>());
    let payload_len = cmsg_len.saturating_sub(header_len);

    if level == libc::IPPROTO_IP && ty == ip_recv_cmsg_type_unix(libc::AF_INET)? {
        if payload_len < mem::size_of::<u8>() {
            return Err(UdpSocketError::InvalidInput("short ECN cmsg"));
        }
        // SAFETY: the payload length check above guarantees that one byte is readable at the
        // aligned payload offset.
        let tos = unsafe { *((c as *mut u8).add(header_len) as *const u8) };
        *ecn = decode_ecn_unix(tos as i32);
        return Ok(true);
    }

    if level == libc::IPPROTO_IPV6 && ty == ip_recv_cmsg_type_unix(libc::AF_INET6)? {
        if payload_len < mem::size_of::<u8>() {
            return Err(UdpSocketError::InvalidInput("short ECN cmsg"));
        }
        // SAFETY: the payload length check above guarantees that one byte is readable at the
        // aligned payload offset.
        let tc = unsafe { *((c as *mut u8).add(header_len) as *const u8) };
        *ecn = decode_ecn_unix(tc as i32);
        return Ok(true);
    }

    Ok(false)
}

#[cfg(unix)]
fn fill_ecn_cmsg_unix(
    c: *mut libc::cmsghdr,
    family: i32,
    ecn: ecn_tp,
) -> Result<(), UdpSocketError> {
    if c.is_null() {
        return Err(UdpSocketError::InvalidInput("null cmsghdr"));
    }

    // SAFETY: `c` points into our own control buffer, which is at least one aligned cmsghdr plus
    // `c_int` payload large by construction.
    unsafe {
        (*c).cmsg_len = cmsg_len_unix(mem::size_of::<libc::c_int>()) as _;
        if family == libc::AF_INET {
            (*c).cmsg_level = libc::IPPROTO_IP;
            (*c).cmsg_type = libc::IP_TOS;
        } else if family == libc::AF_INET6 {
            (*c).cmsg_level = libc::IPPROTO_IPV6;
            (*c).cmsg_type = libc::IPV6_TCLASS;
        } else {
            return Err(UdpSocketError::Syscall {
                call: "fill_ecn_cmsg",
                code: libc::EAFNOSUPPORT,
            });
        }

        let v: i32 = encode_ecn_unix(ecn);
        let data = (c as *mut u8).add(cmsg_align_unix(mem::size_of::<libc::cmsghdr>()));
        core::ptr::copy_nonoverlapping(&v as *const _ as *const u8, data, mem::size_of::<i32>());
    }
    Ok(())
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "netbsd",
    target_os = "openbsd"
))]
fn set_max_priority_unix() {
    // Matches the reference: attempts to elevate scheduling if running as root.
    // SAFETY: the libc calls only operate on the current process and a stack-local `sched_param`.
    unsafe {
        if libc::geteuid() == 0 {
            let mut sp: libc::sched_param = zeroed_sched_param_unix();
            let pri = libc::sched_get_priority_max(libc::SCHED_RR);
            if pri > 0 {
                sp.sched_priority = pri;
                let _ = libc::sched_setscheduler(0, libc::SCHED_RR, &sp as *const _);
            }
        }
    }
}

#[cfg(all(
    unix,
    not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "dragonfly",
        target_os = "netbsd",
        target_os = "openbsd"
    ))
))]
fn set_max_priority_unix() {}

#[cfg(unix)]
#[inline]
fn cmsg_metadata_unix(c: *mut libc::cmsghdr) -> (i32, i32, usize) {
    // SAFETY: callers only pass pointers returned by our control-message iteration helpers or the
    // first-header helper, both of which stay within the control buffer bounds.
    unsafe { ((*c).cmsg_level, (*c).cmsg_type, (*c).cmsg_len) }
}

// ===== Tests =====

#[cfg(test)]
mod portability_tests {
    use super::*;

    #[test]
    fn platform_support_matches_cfg() {
        let expected = if cfg!(unix) {
            SocketPlatformSupport::Full
        } else if cfg!(windows) {
            SocketPlatformSupport::Partial
        } else {
            SocketPlatformSupport::Unsupported
        };

        assert_eq!(UDPSocket::platform_support(), expected);
        assert_eq!(
            UDPSocket::platform_support().is_available(),
            cfg!(any(unix, windows))
        );
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[repr(align(8))]
    struct AlignedControl([u8; CTRL_BUF_SIZE_UNIX]);

    #[test]
    fn cmsg_size_is_reasonable() {
        let control = AlignedControl([0u8; CTRL_BUF_SIZE_UNIX]);

        // For one int cmsg, the buffer should be small (<256 bytes).
        assert!(!control.0.is_empty());
        assert!(control.0.len() < 256);
    }

    #[test]
    fn encode_decode_roundtrip() {
        for &v in &[
            ecn_tp::ecn_not_ect,
            ecn_tp::ecn_l4s_id,
            ecn_tp::ecn_ect0,
            ecn_tp::ecn_ce,
        ] {
            let enc = encode_ecn_unix(v);
            let dec = decode_ecn_unix(enc);
            assert_eq!(dec as u8, v as u8);
        }
    }

    #[test]
    fn fill_ecn_cmsg_unix_writes_expected_ipv4_header_and_value() {
        let mut control = AlignedControl([0u8; CTRL_BUF_SIZE_UNIX]);
        let cmsg = control.0.as_mut_ptr() as *mut libc::cmsghdr;

        fill_ecn_cmsg_unix(cmsg, libc::AF_INET, ecn_tp::ecn_ce).expect("fill ecn cmsg");

        let header_len = cmsg_align_unix(mem::size_of::<libc::cmsghdr>());
        assert_eq!(
            unsafe { (*cmsg).cmsg_len },
            cmsg_len_unix(mem::size_of::<libc::c_int>())
        );
        assert_eq!(unsafe { (*cmsg).cmsg_level }, libc::IPPROTO_IP);
        assert_eq!(unsafe { (*cmsg).cmsg_type }, libc::IP_TOS);

        let raw_value = unsafe { *((control.0.as_ptr().add(header_len)) as *const i32) };
        assert_eq!(raw_value, encode_ecn_unix(ecn_tp::ecn_ce));
    }

    #[test]
    fn parse_ecn_cmsg_unix_accepts_one_byte_ipv4_payload() {
        let mut control = AlignedControl([0u8; CTRL_BUF_SIZE_UNIX]);
        let cmsg = control.0.as_mut_ptr() as *mut libc::cmsghdr;
        let header_len = cmsg_align_unix(mem::size_of::<libc::cmsghdr>());

        unsafe {
            (*cmsg).cmsg_len = (header_len + 1) as _;
            (*cmsg).cmsg_level = libc::IPPROTO_IP;
            (*cmsg).cmsg_type = ip_recv_cmsg_type_unix(libc::AF_INET).unwrap();
            core::ptr::write(control.0.as_mut_ptr().add(header_len), 0x03);
        }

        let mut ecn = ecn_tp::ecn_not_ect;
        assert!(parse_ecn_cmsg_unix(cmsg, &mut ecn).expect("parse one-byte payload"));
        assert_eq!(ecn, ecn_tp::ecn_ce);
    }

    #[test]
    fn parse_ecn_cmsg_unix_rejects_short_payload() {
        let mut control = AlignedControl([0u8; CTRL_BUF_SIZE_UNIX]);
        let cmsg = control.0.as_mut_ptr() as *mut libc::cmsghdr;
        let header_len = cmsg_align_unix(mem::size_of::<libc::cmsghdr>());

        unsafe {
            (*cmsg).cmsg_len = header_len as _;
            (*cmsg).cmsg_level = libc::IPPROTO_IP;
            (*cmsg).cmsg_type = ip_recv_cmsg_type_unix(libc::AF_INET).unwrap();
        }

        let mut ecn = ecn_tp::ecn_not_ect;
        let err = parse_ecn_cmsg_unix(cmsg, &mut ecn).expect_err("short payload should fail");
        match err {
            UdpSocketError::InvalidInput(message) => assert_eq!(message, "short ECN cmsg"),
            other => panic!("unexpected error: {other}"),
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_recvmsg_reports_one_byte_ip_tos_and_parser_handles_it() {
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

        let recv_socket =
            unsafe { OwnedFd::from_raw_fd(make_socket_unix(libc::AF_INET).expect("recv socket")) };
        enable_recv_ecn_unix(recv_socket.as_raw_fd(), libc::AF_INET).expect("enable receive ECN");

        let bind_ep = resolve_endpoint_unix("127.0.0.1", 0).expect("bind endpoint");
        let bind_rc = unsafe {
            libc::bind(
                recv_socket.as_raw_fd(),
                &bind_ep.sa as *const _ as *const libc::sockaddr,
                bind_ep.len,
            )
        };
        assert_eq!(bind_rc, 0, "bind failed: {}", last_errno_unix());

        let mut local_addr = zeroed_sockaddr_storage_unix();
        let mut local_len = core::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
        let getsockname_rc = unsafe {
            libc::getsockname(
                recv_socket.as_raw_fd(),
                &mut local_addr as *mut _ as *mut libc::sockaddr,
                &mut local_len,
            )
        };
        assert_eq!(
            getsockname_rc,
            0,
            "getsockname failed: {}",
            last_errno_unix()
        );

        let local = unsafe {
            &*((&local_addr as *const libc::sockaddr_storage).cast::<libc::sockaddr_in>())
        };
        let port = u16::from_be(local.sin_port);

        let send_socket =
            unsafe { OwnedFd::from_raw_fd(make_socket_unix(libc::AF_INET).expect("send socket")) };
        let peer = resolve_endpoint_unix("127.0.0.1", port).expect("peer endpoint");
        let payload = [0xABu8];

        let mut send_iov = zeroed_iovec_unix();
        send_iov.iov_base = payload.as_ptr() as *mut libc::c_void;
        send_iov.iov_len = payload.len();

        let mut send_ctrl = AlignedControl([0u8; CTRL_BUF_SIZE_UNIX]);
        let mut send_msg = zeroed_msghdr_unix();
        send_msg.msg_name = &peer.sa as *const _ as *mut libc::c_void;
        send_msg.msg_namelen = peer.len;
        send_msg.msg_iov = &mut send_iov as *mut libc::iovec;
        send_msg.msg_iovlen = 1;
        send_msg.msg_control = send_ctrl.0.as_mut_ptr() as *mut libc::c_void;
        send_msg.msg_controllen = send_ctrl.0.len();

        let send_cmsg = first_cmsg_unix(&mut send_msg).expect("send cmsg");
        fill_ecn_cmsg_unix(send_cmsg, libc::AF_INET, ecn_tp::ecn_l4s_id)
            .expect("fill send ECN cmsg");

        let sent =
            unsafe { libc::sendmsg(send_socket.as_raw_fd(), &send_msg as *const libc::msghdr, 0) };
        assert_eq!(
            sent,
            payload.len() as isize,
            "sendmsg failed: {}",
            last_errno_unix()
        );

        let mut recv_buffer = [0u8; 1];
        let mut recv_iov = zeroed_iovec_unix();
        recv_iov.iov_base = recv_buffer.as_mut_ptr() as *mut libc::c_void;
        recv_iov.iov_len = recv_buffer.len();

        let mut recv_ctrl = AlignedControl([0u8; CTRL_BUF_SIZE_UNIX]);
        let mut recv_peer = zeroed_sockaddr_storage_unix();
        let mut recv_msg = zeroed_msghdr_unix();
        recv_msg.msg_name = &mut recv_peer as *mut _ as *mut libc::c_void;
        recv_msg.msg_namelen = core::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
        recv_msg.msg_iov = &mut recv_iov as *mut libc::iovec;
        recv_msg.msg_iovlen = 1;
        recv_msg.msg_control = recv_ctrl.0.as_mut_ptr() as *mut libc::c_void;
        recv_msg.msg_controllen = recv_ctrl.0.len();

        let received = unsafe {
            libc::recvmsg(
                recv_socket.as_raw_fd(),
                &mut recv_msg as *mut libc::msghdr,
                0,
            )
        };
        assert_eq!(
            received,
            payload.len() as isize,
            "recvmsg failed: {}",
            last_errno_unix()
        );
        assert_eq!(recv_buffer, payload);

        let recv_cmsg = first_cmsg_unix(&mut recv_msg).expect("recv cmsg");
        let (level, ty, cmsg_len) = cmsg_metadata_unix(recv_cmsg);
        let header_len = cmsg_align_unix(core::mem::size_of::<libc::cmsghdr>());
        let payload_len = cmsg_len.saturating_sub(header_len);

        assert_eq!(level, libc::IPPROTO_IP);
        assert_eq!(
            ty,
            ip_recv_cmsg_type_unix(libc::AF_INET).expect("ip cmsg type")
        );
        assert_eq!(payload_len, core::mem::size_of::<u8>());

        let mut ecn = ecn_tp::ecn_not_ect;
        assert!(parse_ecn_cmsg_unix(recv_cmsg, &mut ecn).expect("parse kernel cmsg"));
        assert_eq!(ecn, ecn_tp::ecn_l4s_id);
    }
}

// ===== Windows implementation helpers =====

/// Windows Winsock backend.
///
/// The public API of `UDPSocket` mirrors the reference C++ project. On Windows,
/// the reference uses Winsock `WSARecvMsg` / `WSASendMsg` to attach/read ECN
/// control messages (IP_ECN / IPV6_ECN). This module implements the same
/// behavior.
///
/// Some APIs (e.g., `WSASetRecvIPEcn`) require newer Windows builds. If the OS
/// does not support them, `Bind`/`Connect` still succeed, but ECN information
/// may be absent.
#[cfg(windows)]
mod win {
    use super::*;

    use core::ffi::c_void;
    use core::mem::{self, MaybeUninit};
    use core::ptr;
    use core::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
    use std::sync::{Once, OnceLock};

    pub type SOCKET = usize;
    pub type socklen_t = i32;

    pub const INVALID_SOCKET: SOCKET = !0usize;
    pub const SOCKET_ERROR: i32 = -1;

    // Address families (winsock2.h)
    pub const AF_INET: i32 = 2;
    pub const AF_INET6: i32 = 23;
    pub const SOCK_DGRAM: i32 = 2;

    // Protocol levels.
    pub const IPPROTO_IP: i32 = 0;
    pub const IPPROTO_IPV6: i32 = 41;

    // ECN ancillary types (mstcpip.h / ws2ipdef.h). Both are 50 decimal.
    pub const IP_ECN: i32 = 50;
    pub const IPV6_ECN: i32 = 50;

    // ioctl to retrieve extension function pointers (winsock2.h).
    pub const SIO_GET_EXTENSION_FUNCTION_POINTER: u32 = 0xC8000006;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct GUID {
        pub Data1: u32,
        pub Data2: u16,
        pub Data3: u16,
        pub Data4: [u8; 8],
    }

    // WSAID_WSARECVMSG and WSAID_WSASENDMSG GUIDs from mswsock.h:
    // - WSAID_WSARECVMSG: {F689D7C8-6F1F-436B-8A53-E54FE351C322}
    // - WSAID_WSASENDMSG: {A441E712-754F-43CA-84A7-0DEE44CF606D}
    pub const WSAID_WSARECVMSG: GUID = GUID {
        Data1: 0xF689D7C8,
        Data2: 0x6F1F,
        Data3: 0x436B,
        Data4: [0x8A, 0x53, 0xE5, 0x4F, 0xE3, 0x51, 0xC3, 0x22],
    };

    pub const WSAID_WSASENDMSG: GUID = GUID {
        Data1: 0xA441E712,
        Data2: 0x754F,
        Data3: 0x43CA,
        Data4: [0x84, 0xA7, 0x0D, 0xEE, 0x44, 0xCF, 0x60, 0x6D],
    };

    #[repr(C)]
    pub struct WSADATA {
        pub wVersion: u16,
        pub wHighVersion: u16,
        pub szDescription: [u8; 257],
        pub szSystemStatus: [u8; 129],
        pub iMaxSockets: u16,
        pub iMaxUdpDg: u16,
        pub lpVendorInfo: *mut u8,
    }

    #[repr(C)]
    pub struct IN_ADDR {
        pub S_addr: u32,
    }

    #[repr(C)]
    pub struct IN6_ADDR {
        pub Byte: [u8; 16],
    }

    #[repr(C)]
    pub struct SOCKADDR {
        pub sa_family: u16,
        pub sa_data: [u8; 14],
    }

    #[repr(C)]
    pub struct SOCKADDR_IN {
        pub sin_family: u16,
        pub sin_port: u16,
        pub sin_addr: IN_ADDR,
        pub sin_zero: [u8; 8],
    }

    #[repr(C)]
    pub struct SOCKADDR_IN6 {
        pub sin6_family: u16,
        pub sin6_port: u16,
        pub sin6_flowinfo: u32,
        pub sin6_addr: IN6_ADDR,
        pub sin6_scope_id: u32,
    }

    // Standard Windows layout: 128 bytes.
    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    pub struct SOCKADDR_STORAGE {
        pub ss_family: u16,
        pub ss_pad1: [u8; 6],
        pub ss_align: i64,
        pub ss_pad2: [u8; 112],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct TIMEVAL {
        pub tv_sec: i32,
        pub tv_usec: i32,
    }

    pub const FD_SETSIZE: usize = 64;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct FD_SET {
        pub fd_count: u32,
        pub fd_array: [SOCKET; FD_SETSIZE],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct WSABUF {
        pub len: u32,
        pub buf: *mut i8,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct WSAMSG {
        pub name: *mut SOCKADDR,
        pub namelen: socklen_t,
        pub lpBuffers: *mut WSABUF,
        pub dwBufferCount: u32,
        pub Control: WSABUF,
        pub dwFlags: u32,
    }

    #[repr(C)]
    pub struct CMSGHDR {
        pub cmsg_len: usize,
        pub cmsg_level: i32,
        pub cmsg_type: i32,
    }

    pub type LPFN_WSARECVMSG = unsafe extern "system" fn(
        s: SOCKET,
        lpMsg: *mut WSAMSG,
        lpdwNumberOfBytesRecvd: *mut u32,
        lpOverlapped: *mut c_void,
        lpCompletionRoutine: *mut c_void,
    ) -> i32;

    pub type LPFN_WSASENDMSG = unsafe extern "system" fn(
        s: SOCKET,
        lpMsg: *mut WSAMSG,
        dwFlags: u32,
        lpNumberOfBytesSent: *mut u32,
        lpOverlapped: *mut c_void,
        lpCompletionRoutine: *mut c_void,
    ) -> i32;

    #[link(name = "ws2_32")]
    extern "system" {
        pub fn WSAStartup(wVersionRequested: u16, lpWSAData: *mut WSADATA) -> i32;
        pub fn WSACleanup() -> i32;
        pub fn WSAGetLastError() -> i32;

        pub fn socket(af: i32, typ: i32, protocol: i32) -> SOCKET;
        pub fn closesocket(s: SOCKET) -> i32;
        pub fn bind(s: SOCKET, name: *const SOCKADDR, namelen: socklen_t) -> i32;
        pub fn connect(s: SOCKET, name: *const SOCKADDR, namelen: socklen_t) -> i32;

        pub fn recv(s: SOCKET, buf: *mut i8, len: i32, flags: i32) -> i32;
        pub fn send(s: SOCKET, buf: *const i8, len: i32, flags: i32) -> i32;
        pub fn recvfrom(
            s: SOCKET,
            buf: *mut i8,
            len: i32,
            flags: i32,
            from: *mut SOCKADDR,
            fromlen: *mut socklen_t,
        ) -> i32;
        pub fn sendto(
            s: SOCKET,
            buf: *const i8,
            len: i32,
            flags: i32,
            to: *const SOCKADDR,
            tolen: socklen_t,
        ) -> i32;

        pub fn select(
            nfds: i32,
            readfds: *mut FD_SET,
            writefds: *mut FD_SET,
            exceptfds: *mut FD_SET,
            timeout: *mut TIMEVAL,
        ) -> i32;

        pub fn WSAIoctl(
            s: SOCKET,
            dwIoControlCode: u32,
            lpvInBuffer: *mut c_void,
            cbInBuffer: u32,
            lpvOutBuffer: *mut c_void,
            cbOutBuffer: u32,
            lpcbBytesReturned: *mut u32,
            lpOverlapped: *mut c_void,
            lpCompletionRoutine: *mut c_void,
        ) -> i32;
    }

    // `WSASetRecvIPEcn` is not universally available in import libraries depending on the SDK,
    // so we resolve it dynamically at runtime.
    pub type FnWSASetRecvIPEcn = unsafe extern "system" fn(Socket: SOCKET, Enabled: u32) -> i32;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetModuleHandleA(lpModuleName: *const i8) -> *mut c_void;
        fn LoadLibraryA(lpLibFileName: *const i8) -> *mut c_void;
        fn GetProcAddress(hModule: *mut c_void, lpProcName: *const i8) -> *mut c_void;
    }

    static WSA_SET_RECV_IP_ECN: OnceLock<Option<FnWSASetRecvIPEcn>> = OnceLock::new();

    fn load_wsa_set_recv_ip_ecn() -> Option<FnWSASetRecvIPEcn> {
        unsafe {
            let dll = b"ws2_32.dll\0";
            let mut h = GetModuleHandleA(dll.as_ptr() as *const i8);
            if h.is_null() {
                h = LoadLibraryA(dll.as_ptr() as *const i8);
            }
            if h.is_null() {
                return None;
            }
            let sym = b"WSASetRecvIPEcn\0";
            let p = GetProcAddress(h, sym.as_ptr() as *const i8);
            if p.is_null() {
                return None;
            }
            Some(core::mem::transmute::<*mut c_void, FnWSASetRecvIPEcn>(p))
        }
    }

    static WSA_INIT_ONCE: Once = Once::new();
    static WSA_USERS: AtomicUsize = AtomicUsize::new(0);
    static WSA_STARTUP_RC: AtomicI32 = AtomicI32::new(i32::MIN);

    /// Ensure Winsock is initialized once per process, with a lightweight refcount.
    pub fn winsock_acquire() -> Result<(), UdpSocketError> {
        WSA_INIT_ONCE.call_once(|| {
            let mut data = MaybeUninit::<WSADATA>::uninit();
            let rc = unsafe { WSAStartup(0x0202u16, data.as_mut_ptr()) };
            WSA_STARTUP_RC.store(rc, Ordering::Release);
        });

        let rc = WSA_STARTUP_RC.load(Ordering::Acquire);
        if rc != 0 {
            return Err(UdpSocketError::Syscall {
                call: "WSAStartup",
                code: rc,
            });
        }

        WSA_USERS.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Decrement Winsock users and call `WSACleanup` when the last socket drops.
    pub fn winsock_release() {
        if WSA_USERS.fetch_sub(1, Ordering::AcqRel) == 1 {
            unsafe { WSACleanup() };
        }
    }

    #[inline]
    pub fn last_error_code_windows() -> i32 {
        unsafe { WSAGetLastError() }
    }

    #[inline]
    pub fn is_socket_valid_windows(s: SOCKET) -> bool {
        s != INVALID_SOCKET
    }

    pub fn wait_for_readable_windows(
        s: SOCKET,
        timeout_us: time_tp,
    ) -> Result<bool, UdpSocketError> {
        if !is_socket_valid_windows(s) {
            return Err(UdpSocketError::InvalidInput("invalid socket"));
        }
        if timeout_us < 0 {
            return Err(UdpSocketError::InvalidInput("timeout must be >= 0"));
        }

        let mut rfds = FD_SET {
            fd_count: 1,
            fd_array: [0; FD_SETSIZE],
        };
        rfds.fd_array[0] = s;

        let mut tv = TIMEVAL {
            tv_sec: (timeout_us as i64 / 1_000_000) as i32,
            tv_usec: (timeout_us as i64 % 1_000_000) as i32,
        };

        let r = unsafe {
            select(
                (s as i32) + 1,
                &mut rfds as *mut FD_SET,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut tv as *mut TIMEVAL,
            )
        };
        if r == SOCKET_ERROR {
            return Err(UdpSocketError::Syscall {
                call: "select",
                code: last_error_code_windows(),
            });
        }
        Ok(r > 0)
    }

    pub fn make_socket_windows(family: i32) -> Result<SOCKET, UdpSocketError> {
        if family != AF_INET && family != AF_INET6 {
            return Err(UdpSocketError::InvalidInput("unsupported address family"));
        }
        let s = unsafe { socket(family, SOCK_DGRAM, 0) };
        if !is_socket_valid_windows(s) {
            return Err(UdpSocketError::Syscall {
                call: "socket",
                code: last_error_code_windows(),
            });
        }
        Ok(s)
    }

    pub fn enable_recv_ecn_windows(s: SOCKET) -> Result<(), UdpSocketError> {
        if !is_socket_valid_windows(s) {
            return Err(UdpSocketError::InvalidInput("invalid socket"));
        }

        let f = *WSA_SET_RECV_IP_ECN.get_or_init(load_wsa_set_recv_ip_ecn);
        let func = match f {
            Some(func) => func,
            None => return Err(UdpSocketError::NotSupported("WSASetRecvIPEcn")),
        };

        let rc = unsafe { func(s, 1) };
        if rc == SOCKET_ERROR {
            return Err(UdpSocketError::Syscall {
                call: "WSASetRecvIPEcn",
                code: last_error_code_windows(),
            });
        }
        Ok(())
    }

    pub fn load_msg_fns_windows(
        s: SOCKET,
    ) -> Result<(LPFN_WSARECVMSG, LPFN_WSASENDMSG), UdpSocketError> {
        let mut recv_fn: Option<LPFN_WSARECVMSG> = None;
        let mut send_fn: Option<LPFN_WSASENDMSG> = None;

        let mut bytes: u32 = 0;

        let mut guid_recv = WSAID_WSARECVMSG;
        let rc_recv = unsafe {
            WSAIoctl(
                s,
                SIO_GET_EXTENSION_FUNCTION_POINTER,
                &mut guid_recv as *mut GUID as *mut c_void,
                mem::size_of::<GUID>() as u32,
                &mut recv_fn as *mut _ as *mut c_void,
                mem::size_of::<Option<LPFN_WSARECVMSG>>() as u32,
                &mut bytes as *mut u32,
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        if rc_recv == SOCKET_ERROR {
            return Err(UdpSocketError::Syscall {
                call: "WSAIoctl(WSARecvMsg)",
                code: last_error_code_windows(),
            });
        }

        let mut guid_send = WSAID_WSASENDMSG;
        let rc_send = unsafe {
            WSAIoctl(
                s,
                SIO_GET_EXTENSION_FUNCTION_POINTER,
                &mut guid_send as *mut GUID as *mut c_void,
                mem::size_of::<GUID>() as u32,
                &mut send_fn as *mut _ as *mut c_void,
                mem::size_of::<Option<LPFN_WSASENDMSG>>() as u32,
                &mut bytes as *mut u32,
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        if rc_send == SOCKET_ERROR {
            return Err(UdpSocketError::Syscall {
                call: "WSAIoctl(WSASendMsg)",
                code: last_error_code_windows(),
            });
        }

        let recv_fn = recv_fn.ok_or(UdpSocketError::Syscall {
            call: "WSARecvMsg",
            code: last_error_code_windows(),
        })?;
        let send_fn = send_fn.ok_or(UdpSocketError::Syscall {
            call: "WSASendMsg",
            code: last_error_code_windows(),
        })?;
        Ok((recv_fn, send_fn))
    }

    pub fn resolve_endpoint_windows(addr: &str, port: u16) -> Result<Endpoint, UdpSocketError> {
        // Try IPv4 first, then IPv6 (matches reference).
        if let Ok(ip4) = addr.parse::<std::net::Ipv4Addr>() {
            let mut v4: SOCKADDR_IN = unsafe { mem::zeroed() };
            v4.sin_family = AF_INET as u16;
            v4.sin_port = port.to_be();
            v4.sin_addr = IN_ADDR {
                S_addr: u32::from_ne_bytes(ip4.octets()),
            };
            v4.sin_zero = [0u8; 8];

            let mut storage: SOCKADDR_STORAGE = unsafe { mem::zeroed() };
            unsafe {
                ptr::copy_nonoverlapping(
                    &v4 as *const SOCKADDR_IN as *const u8,
                    &mut storage as *mut SOCKADDR_STORAGE as *mut u8,
                    mem::size_of::<SOCKADDR_IN>(),
                );
            }
            return Ok(Endpoint {
                sa: storage,
                len: mem::size_of::<SOCKADDR_IN>() as socklen_t,
            });
        }

        if let Ok(ip6) = addr.parse::<std::net::Ipv6Addr>() {
            let mut v6: SOCKADDR_IN6 = unsafe { mem::zeroed() };
            v6.sin6_family = AF_INET6 as u16;
            v6.sin6_port = port.to_be();
            v6.sin6_flowinfo = 0;
            v6.sin6_addr = IN6_ADDR { Byte: ip6.octets() };
            v6.sin6_scope_id = 0;

            let mut storage: SOCKADDR_STORAGE = unsafe { mem::zeroed() };
            unsafe {
                ptr::copy_nonoverlapping(
                    &v6 as *const SOCKADDR_IN6 as *const u8,
                    &mut storage as *mut SOCKADDR_STORAGE as *mut u8,
                    mem::size_of::<SOCKADDR_IN6>(),
                );
            }
            return Ok(Endpoint {
                sa: storage,
                len: mem::size_of::<SOCKADDR_IN6>() as socklen_t,
            });
        }

        Err(UdpSocketError::UnsupportedAddress)
    }

    // ---- Control message helpers (mswsock.h macros) ----

    const fn wsa_cmsg_align(len: usize) -> usize {
        let a = mem::size_of::<usize>();
        (len + (a - 1)) & !(a - 1)
    }

    pub const fn wsa_cmsg_len(data_len: usize) -> usize {
        wsa_cmsg_align(mem::size_of::<CMSGHDR>()) + data_len
    }

    pub const fn wsa_cmsg_space(data_len: usize) -> usize {
        wsa_cmsg_align(mem::size_of::<CMSGHDR>()) + wsa_cmsg_align(data_len)
    }

    pub const CTRL_BUF_SIZE_WIN: usize = wsa_cmsg_space(mem::size_of::<i32>());

    #[inline]
    pub fn wsa_cmsg_firsthdr(msg: &WSAMSG) -> *mut CMSGHDR {
        if (msg.Control.len as usize) >= mem::size_of::<CMSGHDR>() {
            msg.Control.buf as *mut CMSGHDR
        } else {
            ptr::null_mut()
        }
    }

    #[inline]
    pub fn wsa_cmsg_nxthdr(msg: &WSAMSG, cmsg: *mut CMSGHDR) -> *mut CMSGHDR {
        if cmsg.is_null() {
            return ptr::null_mut();
        }
        unsafe {
            let base = msg.Control.buf as *mut u8;
            let end = base.add(msg.Control.len as usize);
            let cmsg_u8 = cmsg as *mut u8;
            let step = wsa_cmsg_align((*cmsg).cmsg_len as usize);
            let next = cmsg_u8.add(step);
            if next.add(mem::size_of::<CMSGHDR>()) > end {
                ptr::null_mut()
            } else {
                next as *mut CMSGHDR
            }
        }
    }

    #[inline]
    unsafe fn wsa_cmsg_data(cmsg: *mut CMSGHDR) -> *mut u8 {
        unsafe { (cmsg as *mut u8).add(wsa_cmsg_align(mem::size_of::<CMSGHDR>())) }
    }

    #[inline]
    pub fn parse_ecn_cmsg_windows(c: *mut CMSGHDR, ecn: &mut ecn_tp) -> bool {
        if c.is_null() {
            return false;
        }
        unsafe {
            if (*c).cmsg_level == IPPROTO_IP && (*c).cmsg_type == IP_ECN {
                let p = wsa_cmsg_data(c) as *const i32;
                let v = *p;
                *ecn = match (v & 0x3) as u8 {
                    0 => ecn_tp::ecn_not_ect,
                    1 => ecn_tp::ecn_l4s_id,
                    2 => ecn_tp::ecn_ect0,
                    _ => ecn_tp::ecn_ce,
                };
                return true;
            }
            if (*c).cmsg_level == IPPROTO_IPV6 && (*c).cmsg_type == IPV6_ECN {
                let p = wsa_cmsg_data(c) as *const i32;
                let v = *p;
                *ecn = match (v & 0x3) as u8 {
                    0 => ecn_tp::ecn_not_ect,
                    1 => ecn_tp::ecn_l4s_id,
                    2 => ecn_tp::ecn_ect0,
                    _ => ecn_tp::ecn_ce,
                };
                return true;
            }
        }
        false
    }

    #[inline]
    pub fn fill_ecn_cmsg_windows(c: *mut CMSGHDR, family: i32, ecn: ecn_tp) {
        unsafe {
            (*c).cmsg_len = wsa_cmsg_len(mem::size_of::<i32>());
            (*c).cmsg_level = if family == AF_INET {
                IPPROTO_IP
            } else {
                IPPROTO_IPV6
            };
            (*c).cmsg_type = if family == AF_INET { IP_ECN } else { IPV6_ECN };
            let p = wsa_cmsg_data(c) as *mut i32;
            *p = ecn as i32;
        }
    }
}
