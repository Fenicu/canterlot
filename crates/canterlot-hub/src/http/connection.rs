use std::{
    future::Future,
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use axum::{
    Extension, Router,
    body::Body,
    extract::{ConnectInfo, Request, State},
    http::StatusCode,
    middleware::{Next, from_fn_with_state},
    response::Response,
};
use http_body::{Body as HttpBody, Frame, SizeHint};
use hyper_util::{
    rt::{TokioExecutor, TokioIo, TokioTimer},
    server::conn::auto::Builder,
    service::TowerToHyperService,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
    sync::{OwnedSemaphorePermit, Semaphore, watch},
    task::JoinSet,
    time::{Instant, sleep_until},
};
use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};

use crate::config::HttpConfig;

use super::SHUTDOWN_GRACE;

#[derive(Clone)]
struct BodyDeadline {
    deadline: watch::Sender<Option<Instant>>,
    timeout: Duration,
}

struct ReadBody {
    inner: Pin<Box<Body>>,
    deadline: watch::Sender<Option<Instant>>,
}

impl HttpBody for ReadBody {
    type Data = axum::body::Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let result = self.inner.as_mut().poll_frame(cx);
        if matches!(result, Poll::Ready(None | Some(Err(_)))) || self.inner.is_end_stream() {
            self.deadline.send_replace(None);
        }
        result
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

async fn body_deadline(
    State(state): State<BodyDeadline>,
    request: Request,
    next: Next,
) -> Response {
    let (parts, body) = request.into_parts();
    state
        .deadline
        .send_replace((!body.is_end_stream()).then(|| Instant::now() + state.timeout));
    let body = Body::new(ReadBody {
        inner: Box::pin(body),
        deadline: state.deadline.clone(),
    });
    let response = next.run(Request::from_parts(parts, body)).await;
    if response.status() == StatusCode::SWITCHING_PROTOCOLS {
        state.deadline.send_replace(None);
    }
    response
}

async fn expired(mut deadline: watch::Receiver<Option<Instant>>) {
    loop {
        let current = *deadline.borrow_and_update();
        tokio::select! {
            biased;
            changed = deadline.changed() => if changed.is_err() { return; },
            () = async {
                match current {
                    Some(at) => sleep_until(at).await,
                    None => std::future::pending().await,
                }
            } => return,
        }
    }
}

struct ConnectionIo {
    socket: TcpStream,
    _permit: OwnedSemaphorePermit,
    cancelled: Pin<Box<WaitForCancellationFutureOwned>>,
}

impl AsyncRead for ConnectionIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.cancelled.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(io::ErrorKind::ConnectionAborted.into()));
        }
        Pin::new(&mut self.socket).poll_read(cx, buf)
    }
}

impl AsyncWrite for ConnectionIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.cancelled.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(io::ErrorKind::ConnectionAborted.into()));
        }
        Pin::new(&mut self.socket).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.cancelled.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(io::ErrorKind::ConnectionAborted.into()));
        }
        Pin::new(&mut self.socket).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.socket).poll_shutdown(cx)
    }
}

pub(super) async fn serve(
    router: Router,
    listener: TcpListener,
    config: HttpConfig,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> io::Result<()> {
    let permits = Arc::new(Semaphore::new(config.max_connections));
    let stop = CancellationToken::new();
    let force_close = CancellationToken::new();
    let mut connections = JoinSet::new();
    let mut shutdown = std::pin::pin!(shutdown);
    let result = loop {
        tokio::select! {
            biased;
            () = &mut shutdown => break Ok(()),
            Some(_) = connections.join_next() => {},
            accepted = listener.accept() => {
                let (socket, peer) = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) => break Err(error),
                };
                let Ok(permit) = permits.clone().try_acquire_owned() else {
                    drop(socket);
                    continue;
                };
                let header_deadline = Instant::now() + config.header_read_timeout;
                let close = force_close.child_token();
                let io = ConnectionIo { socket, _permit: permit, cancelled: Box::pin(close.clone().cancelled_owned()) };
                let stop = stop.clone();
                let config = config.clone();
                let router = router.clone();
                connections.spawn(async move {
                    let (deadline, receiver) = watch::channel(Some(header_deadline));
                    let router = router
                        .layer(Extension(ConnectInfo(peer)))
                        .layer(from_fn_with_state(BodyDeadline { deadline, timeout: config.body_read_timeout }, body_deadline));
                    let mut builder = Builder::new(TokioExecutor::new()).http1_only();
                    builder.http1().timer(TokioTimer::new()).header_read_timeout(config.header_read_timeout);
                    let connection = builder.serve_connection_with_upgrades(TokioIo::new(io), TowerToHyperService::new(router));
                    let mut connection = std::pin::pin!(connection);
                    let mut expired = std::pin::pin!(expired(receiver));
                    tokio::select! {
                        _ = &mut connection => {},
                        () = &mut expired => close.cancel(),
                        () = stop.cancelled() => {
                            connection.as_mut().graceful_shutdown();
                            tokio::select! {
                                _ = &mut connection => {},
                                () = &mut expired => close.cancel(),
                            }
                        },
                    }
                });
            },
        }
    };
    drop(listener);
    stop.cancel();
    if tokio::time::timeout(SHUTDOWN_GRACE, async {
        while connections.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        tracing::warn!(
            kind = "http_shutdown_timeout",
            "open requests were abandoned"
        );
    }
    force_close.cancel();
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    result
}
