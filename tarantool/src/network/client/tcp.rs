//! Contains an implementation of a custom async coio based [`TcpStream`].
//!
//! ## Example
//! ```no_run
//! # async {
//! use futures::AsyncReadExt;
//! use tarantool::network::client::tcp::TcpStream;
//!
//! let mut stream = TcpStream::connect("localhost", 8080)
//!     .await
//!     .unwrap();
//! let mut buf = vec![];
//! let read_size = stream
//!     .read(&mut buf)
//!     .await
//!     .unwrap();
//! # };
//! ```

use std::cell::Cell;
use std::ffi::{CString, NulError};
use std::future::Future;
use std::mem::{self, MaybeUninit};
use std::net::{SocketAddr, SocketAddrV4, SocketAddrV6};
use std::os::unix::io::RawFd;
use std::os::unix::prelude::IntoRawFd;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll};
use std::{io, ptr};

#[cfg(feature = "async-std")]
use async_std::io::{Read as AsyncRead, Write as AsyncWrite};
#[cfg(not(feature = "async-std"))]
use futures::{AsyncRead, AsyncWrite};

use crate::ffi::tarantool as ffi;
use crate::fiber::r#async::context::ContextExt;
use crate::fiber::{self, r#async};

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("failed to resolve domain name '{0}'")]
    ResolveAddress(String),
    #[error("input parameters contain ffi incompatible strings: {0}")]
    ConstructCString(NulError),
    #[error("failed to connect to address '{address}': {error}")]
    Connect { error: io::Error, address: String },
    #[error("failed to set socket to nonblocking mode: {0}")]
    SetNonBlock(io::Error),
    #[error("unknown address family: {0}")]
    UnknownAddressFamily(u16),
    #[error("write half of the stream is closed")]
    WriteClosed,
}

/// Async TcpStream based on fibers and coio.
///
/// Use [timeout][t] on top of read or write operations on [`TcpStream`]
/// to set the max time to wait for an operation.
///
/// Atention should be payed that [`TcpStream`] is not [`futures::select`] friendly when awaiting multiple streams
/// As there is no coio support to await multiple file descriptors yet.
/// Though it can be used with [`futures::join`] without problems.
///
/// See module level [documentation](super::tcp) for examples.
///
/// [t]: crate::fiber::async::timeout::timeout
#[derive(Debug, Clone)]
pub struct TcpStream {
    /// A raw tcp socket file descriptor. Replaced with `None` when the stream
    /// is closed.
    ///
    /// Note that it's wrapped in a `Rc`, because the outer `TcpStream` needs to
    /// be mutably borrowable (thanks to AsyncWrite & AsyncRead traits) and it
    /// doesn't make sense to wrap it in a Mutex of any sort, because it's
    /// perfectly safe to read & write on a tcp socket even from concurrent threads,
    /// but we only use it from different fibers.
    fd: Rc<Cell<Option<RawFd>>>,
}

impl TcpStream {
    /// Creates a [`TcpStream`] to `url`.
    /// `resolve_timeout` - address resolution timeout.
    ///
    /// This functions makes the fiber **yield**.
    pub async fn connect(url: &str, port: u16) -> Result<TcpStream, Error> {
        let addrs = unsafe {
            let addr_info = get_address_info(url).await?;
            let addrs = get_addrs_from_info(addr_info, port);
            libc::freeaddrinfo(addr_info);
            // Try IPv4 addresses first when connecting
            ipv4_first(addrs?)
        };
        // FIXME: we're blocking the whole tx thread over here
        let stream = crate::unwrap_ok_or!(std::net::TcpStream::connect(addrs.as_slice()),
            Err(e) => {
                return Err(Error::Connect { error: e, address: format!("{url}:{port}") });
            }
        );
        stream.set_nonblocking(true).map_err(Error::SetNonBlock)?;
        let fd = stream.into_raw_fd();
        Ok(Self {
            fd: Rc::new(Cell::new(Some(fd))),
        })
    }

    #[inline(always)]
    #[track_caller]
    pub fn close(&mut self) -> io::Result<()> {
        let Some(fd) = self.fd.take() else {
            // Already closed.
            return Ok(());
        };

        // SAFETY: safe because we close the `fd` only once
        let rc = unsafe { ffi::coio_close(fd) };
        if rc != 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EBADF) {
                // FIXME: do this instead
                // crate::say_error!("close({fd}): Bad file descriptor");
                crate::log::say(
                    crate::log::SayLevel::Error,
                    file!(),
                    line!() as _,
                    None,
                    &format!("close({fd}): Bad file descriptor"),
                );
                if cfg!(debug_assertions) {
                    panic!("close({}): Bad file descriptor", fd);
                }
            }
            return Err(e);
        }
        Ok(())
    }
}

/// Sorts the supplied `addrs` so that IPv4 addresses are first.
fn ipv4_first(addrs: impl IntoIterator<Item = SocketAddr>) -> Vec<SocketAddr> {
    let (mut ipv4, mut ipv6): (Vec<_>, Vec<_>) = addrs.into_iter().partition(|addr| addr.is_ipv4());
    ipv4.append(&mut ipv6);
    ipv4
}

unsafe fn get_addrs_from_info(
    addrs: *const libc::addrinfo,
    port: u16,
) -> Result<Vec<SocketAddr>, Error> {
    let mut addr = addrs;
    let mut out_addrs = Vec::new();
    while !addr.is_null() {
        out_addrs.push(to_rs_sockaddr((*addr).ai_addr, port)?);
        addr = (*addr).ai_next;
    }
    Ok(out_addrs)
}

async unsafe fn get_address_info(url: &str) -> Result<*mut libc::addrinfo, Error> {
    struct GetAddrInfo(r#async::coio::GetAddrInfo);

    impl Future for GetAddrInfo {
        type Output = Result<*mut libc::addrinfo, ()>;

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            unsafe {
                if self.0.err.get() {
                    return Poll::Ready(Err(()));
                }
                if self.0.res.get().is_null() {
                    ContextExt::set_coio_getaddrinfo(cx, self.0.clone());
                    Poll::Pending
                } else {
                    Poll::Ready(Ok(self.0.res.get()))
                }
            }
        }
    }

    let host = CString::new(url).map_err(Error::ConstructCString)?;
    let mut hints = MaybeUninit::<libc::addrinfo>::zeroed().assume_init();
    hints.ai_family = libc::AF_UNSPEC;
    hints.ai_socktype = libc::SOCK_STREAM;
    GetAddrInfo(r#async::coio::GetAddrInfo {
        host,
        hints,
        res: Rc::new(Cell::new(ptr::null_mut())),
        err: Rc::new(Cell::new(false)),
    })
    .await
    .map_err(|()| Error::ResolveAddress(url.into()))
}

unsafe fn to_rs_sockaddr(addr: *const libc::sockaddr, port: u16) -> Result<SocketAddr, Error> {
    match (*addr).sa_family as libc::c_int {
        libc::AF_INET => {
            let addr: *mut libc::sockaddr_in = mem::transmute(addr);
            (*addr).sin_port = port;
            let octets: [u8; 4] = (*addr).sin_addr.s_addr.to_ne_bytes();
            Ok(SocketAddr::V4(SocketAddrV4::new(octets.into(), port)))
        }
        libc::AF_INET6 => {
            let addr: *mut libc::sockaddr_in6 = mem::transmute(addr);
            (*addr).sin6_port = port;
            let octets = (*addr).sin6_addr.s6_addr;
            let flow_info = (*addr).sin6_flowinfo;
            let scope_id = (*addr).sin6_scope_id;
            Ok(SocketAddr::V6(SocketAddrV6::new(
                octets.into(),
                port,
                flow_info,
                scope_id,
            )))
        }
        af => Err(Error::UnknownAddressFamily(af as u16)),
    }
}

impl AsyncWrite for TcpStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let Some(fd) = self.fd.get() else {
            let e = io::Error::new(io::ErrorKind::Other, "socket closed already");
            return Poll::Ready(Err(e));
        };

        let (result, err) = (
            // `self.fd` must be nonblocking for this to work correctly
            unsafe { libc::write(fd, buf.as_ptr() as *const libc::c_void, buf.len()) },
            io::Error::last_os_error(),
        );

        if result >= 0 {
            return Poll::Ready(Ok(result as usize));
        }
        match err.kind() {
            io::ErrorKind::WouldBlock => {
                // SAFETY: Safe as long as this future is executed by
                // `fiber::block_on` async executor.
                unsafe { ContextExt::set_coio_wait(cx, fd, ffi::CoIOFlags::WRITE) }
                Poll::Pending
            }
            io::ErrorKind::Interrupted => {
                // Return poll pending without setting coio wait
                // so that write can be retried immediately.
                //
                // SAFETY: Safe as long as this future is executed by
                // `fiber::block_on` async executor.
                unsafe { ContextExt::set_deadline(cx, fiber::clock()) }
                Poll::Pending
            }
            _ => Poll::Ready(Err(err)),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.fd.get().is_none() {
            let e = io::Error::new(io::ErrorKind::Other, "socket closed already");
            return Poll::Ready(Err(e));
        };

        // [`TcpStream`] similarily to std does not buffer anything,
        // so there is nothing to flush.
        //
        // If buffering is needed use [`futures::io::BufWriter`] on top of this stream.
        Poll::Ready(Ok(()))
    }

    fn poll_close(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.fd.get().is_none() {
            let e = io::Error::new(io::ErrorKind::Other, "socket closed already");
            return Poll::Ready(Err(e));
        };

        let res = self.close();
        Poll::Ready(res)
    }
}

impl AsyncRead for TcpStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        let Some(fd) = self.fd.get() else {
            let e = io::Error::new(io::ErrorKind::Other, "socket closed already");
            return Poll::Ready(Err(e));
        };

        let (result, err) = (
            // `self.fd` must be nonblocking for this to work correctly
            unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) },
            io::Error::last_os_error(),
        );

        if result >= 0 {
            return Poll::Ready(Ok(result as usize));
        }
        match err.kind() {
            io::ErrorKind::WouldBlock => {
                // SAFETY: Safe as long as this future is executed by
                // `fiber::block_on` async executor.
                unsafe { ContextExt::set_coio_wait(cx, fd, ffi::CoIOFlags::READ) }
                Poll::Pending
            }
            io::ErrorKind::Interrupted => {
                // Return poll pending without setting coio wait
                // so that read can be retried immediately.
                //
                // SAFETY: Safe as long as this future is executed by
                // `fiber::block_on` async executor.
                unsafe { ContextExt::set_deadline(cx, fiber::clock()) }
                Poll::Pending
            }
            _ => Poll::Ready(Err(err)),
        }
    }
}

impl Drop for TcpStream {
    fn drop(&mut self) {
        if let Err(e) = self.close() {
            // FIXME: do this instead
            // crate::say_error!("TcpStream::drop: closing tcp stream failed: {e}");
            crate::log::say(
                crate::log::SayLevel::Error,
                file!(),
                line!() as _,
                None,
                &format!("TcpStream::drop: closing tcp stream failed: {e}"),
            );
        }
    }
}

#[cfg(feature = "internal_test")]
mod tests {
    use super::*;

    use crate::fiber;
    use crate::fiber::r#async::timeout::{self, IntoTimeout};
    use crate::test::util::always_pending;
    use crate::test::util::listen_port;

    use std::collections::HashSet;
    use std::net::{TcpListener, ToSocketAddrs};
    use std::thread;
    use std::time::Duration;

    use futures::{AsyncReadExt, AsyncWriteExt, FutureExt};

    const _10_SEC: Duration = Duration::from_secs(10);
    const _0_SEC: Duration = Duration::from_secs(0);

    #[crate::test(tarantool = "crate")]
    fn resolve_address() {
        unsafe {
            let _ = fiber::block_on(get_address_info("localhost").timeout(_10_SEC)).unwrap();
        }
    }

    #[crate::test(tarantool = "crate")]
    async fn resolve_same_as_std() {
        let addrs_1: HashSet<_> = unsafe {
            get_addrs_from_info(
                get_address_info("example.org")
                    .timeout(_10_SEC)
                    .await
                    .unwrap(),
                80,
            )
            .unwrap()
            .into_iter()
            .collect()
        };
        let addrs_2: HashSet<_> = ToSocketAddrs::to_socket_addrs("example.org:80")
            .unwrap()
            .collect();
        assert_eq!(addrs_1, addrs_2);
    }

    #[crate::test(tarantool = "crate")]
    fn resolve_address_error() {
        unsafe {
            let err = fiber::block_on(get_address_info("invalid domain name").timeout(_10_SEC))
                .unwrap_err()
                .to_string();
            assert_eq!(err, "failed to resolve domain name 'invalid domain name'")
        }
    }

    #[crate::test(tarantool = "crate")]
    fn connect() {
        let _ = fiber::block_on(TcpStream::connect("localhost", listen_port()).timeout(_10_SEC))
            .unwrap();
    }

    #[crate::test(tarantool = "crate")]
    async fn read() {
        let mut stream = TcpStream::connect("localhost", listen_port())
            .timeout(_10_SEC)
            .await
            .unwrap();
        // Read greeting
        let mut buf = vec![0; 128];
        stream.read_exact(&mut buf).timeout(_10_SEC).await.unwrap();
    }

    #[crate::test(tarantool = "crate")]
    async fn read_timeout() {
        let mut stream = TcpStream::connect("localhost", listen_port())
            .timeout(_10_SEC)
            .await
            .unwrap();
        // Read greeting
        let mut buf = vec![0; 128];
        assert_eq!(
            stream
                .read_exact(&mut buf)
                .timeout(_0_SEC)
                .await
                .unwrap_err()
                .to_string(),
            "deadline expired"
        );
    }

    #[crate::test(tarantool = "crate")]
    fn write() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let listener = TcpListener::bind("127.0.0.1:3302").unwrap();
        // Spawn listener
        thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut buf = vec![];
                <std::net::TcpStream as std::io::Read>::read_to_end(&mut stream, &mut buf).unwrap();
                sender.send(buf).unwrap();
            }
        });
        // Send data
        {
            fiber::block_on(async {
                let mut stream = TcpStream::connect("localhost", 3302)
                    .timeout(_10_SEC)
                    .await
                    .unwrap();
                timeout::timeout(_10_SEC, stream.write_all(&[1, 2, 3]))
                    .await
                    .unwrap();
                timeout::timeout(_10_SEC, stream.write_all(&[4, 5]))
                    .await
                    .unwrap();
            });
        }
        let buf = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(buf, vec![1, 2, 3, 4, 5])
    }

    #[crate::test(tarantool = "crate")]
    fn split() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let listener = TcpListener::bind("127.0.0.1:3303").unwrap();
        // Spawn listener
        thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut buf = vec![0; 5];
                <std::net::TcpStream as std::io::Read>::read_exact(&mut stream, &mut buf).unwrap();
                <std::net::TcpStream as std::io::Write>::write_all(&mut stream, &buf.clone())
                    .unwrap();
                sender.send(buf).unwrap();
            }
        });
        // Send and read data
        {
            let stream =
                fiber::block_on(TcpStream::connect("localhost", 3303).timeout(_10_SEC)).unwrap();
            let (mut reader, mut writer) = stream.split();
            let reader_handle = fiber::start_async(async move {
                let mut buf = vec![0; 5];
                timeout::timeout(_10_SEC, reader.read_exact(&mut buf))
                    .await
                    .unwrap();
                assert_eq!(buf, vec![1, 2, 3, 4, 5])
            });
            let writer_handle = fiber::start_async(async move {
                timeout::timeout(_10_SEC, writer.write_all(&[1, 2, 3]))
                    .await
                    .unwrap();
                timeout::timeout(_10_SEC, writer.write_all(&[4, 5]))
                    .await
                    .unwrap();
            });
            writer_handle.join();
            reader_handle.join();
        }
        let buf = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(buf, vec![1, 2, 3, 4, 5])
    }

    #[crate::test(tarantool = "crate")]
    fn join_correct_timeout() {
        {
            fiber::block_on(async {
                let mut stream = TcpStream::connect("localhost", listen_port())
                    .timeout(_10_SEC)
                    .await
                    .unwrap();
                // Read greeting
                let mut buf = vec![0; 128];
                let (is_err, is_ok) = futures::join!(
                    timeout::timeout(_0_SEC, always_pending()),
                    timeout::timeout(_10_SEC, stream.read_exact(&mut buf))
                );
                assert_eq!(is_err.unwrap_err().to_string(), "deadline expired");
                is_ok.unwrap();
            });
        }
        // Testing with different order in join
        {
            fiber::block_on(async {
                let mut stream = TcpStream::connect("localhost", listen_port())
                    .timeout(_10_SEC)
                    .await
                    .unwrap();
                // Read greeting
                let mut buf = vec![0; 128];
                let (is_ok, is_err) = futures::join!(
                    timeout::timeout(_10_SEC, stream.read_exact(&mut buf)),
                    timeout::timeout(_0_SEC, always_pending())
                );
                assert_eq!(is_err.unwrap_err().to_string(), "deadline expired");
                is_ok.unwrap();
            });
        }
    }

    #[crate::test(tarantool = "crate")]
    fn select_correct_timeout() {
        {
            fiber::block_on(async {
                let mut stream = TcpStream::connect("localhost", listen_port())
                    .timeout(_10_SEC)
                    .await
                    .unwrap();
                // Read greeting
                let mut buf = vec![0; 128];
                let f1 = timeout::timeout(_0_SEC, always_pending()).fuse();
                let f2 = timeout::timeout(_10_SEC, stream.read_exact(&mut buf)).fuse();
                futures::pin_mut!(f1);
                futures::pin_mut!(f2);
                let is_err = futures::select!(
                    res = f1 => res.is_err(),
                    res = f2 => res.is_err()
                );
                assert!(is_err);
            });
        }
        // Testing with different future timeouting first
        {
            fiber::block_on(async {
                let mut stream = TcpStream::connect("localhost", listen_port())
                    .timeout(_10_SEC)
                    .await
                    .unwrap();
                // Read greeting
                let mut buf = vec![0; 128];
                let f1 = timeout::timeout(Duration::from_secs(15), always_pending()).fuse();
                let f2 = timeout::timeout(_10_SEC, stream.read_exact(&mut buf)).fuse();
                futures::pin_mut!(f1);
                futures::pin_mut!(f2);
                let is_ok = futures::select!(
                    res = f1 => res.is_ok(),
                    res = f2 => res.is_ok()
                );
                assert!(is_ok);
            });
        }
    }

    #[crate::test(tarantool = "crate")]
    async fn no_socket_double_close() {
        let mut stream = TcpStream::connect("localhost", listen_port())
            .timeout(_10_SEC)
            .await
            .unwrap();

        let fd = stream.fd.get().unwrap();

        // Socket is not closed yet
        assert_ne!(unsafe { dbg!(libc::fcntl(fd, libc::F_GETFD)) }, -1);

        // Close the socket
        stream.close().unwrap();

        // Socket is closed now
        assert_eq!(unsafe { dbg!(libc::fcntl(fd, libc::F_GETFD)) }, -1);

        // Reuse the socket's file descriptor
        assert_ne!(unsafe { libc::dup2(libc::STDOUT_FILENO, fd) }, -1);

        // The file descriptor is open
        assert_ne!(unsafe { dbg!(libc::fcntl(fd, libc::F_GETFD)) }, -1);

        drop(stream);

        // The now unrelated file descriptor mustn't be closed
        assert_ne!(unsafe { dbg!(libc::fcntl(fd, libc::F_GETFD)) }, -1);

        // Cleanup
        unsafe { libc::close(fd) };
    }
}
