//! Keep accepted transport I/O inside the owned application's stop/join boundary.
use std::{
    io::{self, IoSlice},
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
};

use axum::serve::Listener;
use futures_util::future::BoxFuture;
use haystack_app::{ApplicationHandle, WorkGuard};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
};

pub(crate) struct OwnedListener {
    listener: TcpListener,
    application: ApplicationHandle,
}
impl OwnedListener {
    pub(crate) fn new(listener: TcpListener, application: ApplicationHandle) -> Self {
        Self {
            listener,
            application,
        }
    }
}
impl Listener for OwnedListener {
    type Io = OwnedIo;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        let (stream, address) = Listener::accept(&mut self.listener).await;
        let Ok(guard) = self.application.admit() else {
            // Seal won the race with accept. Do not start a new connection;
            // Axum's graceful-shutdown signal will drop this pending accept.
            drop(stream);
            return std::future::pending().await;
        };
        let stop = guard.cancellation();
        (
            OwnedIo {
                stream: Some(stream),
                read_stop: Box::pin(stop.clone().cancelled_owned()),
                write_stop: Box::pin(stop.cancelled_owned()),
                _guard: guard,
            },
            address,
        )
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }
}

pub(crate) struct OwnedIo {
    stream: Option<TcpStream>,
    // Upgraded WebSockets can poll read and write from distinct tasks. Each
    // direction needs its own registered cancellation waker.
    read_stop: BoxFuture<'static, ()>,
    write_stop: BoxFuture<'static, ()>,
    // The request guard ends when middleware returns a response. This guard
    // instead follows transport ownership through Hyper or an upgraded socket.
    _guard: WorkGuard,
}
impl OwnedIo {
    fn stopped() -> io::Error {
        io::Error::new(io::ErrorKind::ConnectionAborted, "application stopping")
    }

    fn poll_write_operation<T>(
        &mut self,
        cx: &mut Context<'_>,
        poll: impl FnOnce(Pin<&mut TcpStream>, &mut Context<'_>) -> Poll<io::Result<T>>,
    ) -> Poll<io::Result<T>> {
        let stopped = self.write_stop.as_mut().poll(cx).is_ready();
        let Some(stream) = self.stream.as_mut() else {
            return Poll::Ready(Err(Self::stopped()));
        };
        // Allow an immediately writable WebSocket close frame (or final HTTP
        // bytes) to finish. After stop, no pending write/flush/shutdown may wait
        // on the peer: cancellation has registered a waker even if the socket
        // itself never becomes writable again.
        match poll(Pin::new(stream), cx) {
            Poll::Pending if stopped => {
                self.stream.take();
                Poll::Ready(Err(Self::stopped()))
            }
            result => result,
        }
    }
}
impl AsyncRead for OwnedIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.read_stop.as_mut().poll(cx).is_ready() {
            self.stream.take();
            return Poll::Ready(Err(Self::stopped()));
        }
        match self.stream.as_mut() {
            Some(stream) => Pin::new(stream).poll_read(cx, buf),
            None => Poll::Ready(Err(Self::stopped())),
        }
    }
}
impl AsyncWrite for OwnedIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.poll_write_operation(cx, |stream, cx| stream.poll_write(cx, buf))
    }
    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        self.poll_write_operation(cx, |stream, cx| stream.poll_write_vectored(cx, bufs))
    }
    fn is_write_vectored(&self) -> bool {
        self.stream
            .as_ref()
            .is_some_and(AsyncWrite::is_write_vectored)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.poll_write_operation(cx, AsyncWrite::poll_flush)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.poll_write_operation(cx, AsyncWrite::poll_shutdown)
    }
}
