use std::time::Duration;

use grpc_probe::{
    Error, HANDSHAKE_TIMEOUT, MESSAGE_LIMIT, ProbeHost, client,
    measurement::{Measurement, QUEUE_CAPACITY, sized_up},
    wire::{Down, Up},
};
use prost::Message;
use tokio::{
    sync::mpsc,
    time::{Instant, sleep, timeout},
};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Code, Request, Streaming, transport::Channel};

struct Session {
    tx: mpsc::Sender<Up>,
    inbound: Streaming<Down>,
    peer: String,
}

async fn open(
    host: &ProbeHost,
    channel: Channel,
    limit: usize,
    data: bool,
) -> Result<Session, Error> {
    let (tx, rx) = mpsc::channel(8);
    tx.send(Up::default()).await?;
    let mut client = client(channel, limit);
    let request = host.request(ReceiverStream::new(rx));
    let response = timeout(Duration::from_secs(3), async {
        if data {
            client.data(request).await
        } else {
            client.control(request).await
        }
    })
    .await??;
    let peer = response
        .metadata()
        .get("x-probe-peer")
        .ok_or("missing peer")?
        .to_str()?
        .to_owned();
    let mut inbound = response.into_inner();
    assert_eq!(
        timeout(Duration::from_secs(3), inbound.message()).await??,
        Some(Down::default())
    );
    Ok(Session { tx, inbound, peer })
}

async fn heartbeat(session: &mut Session, sequence: u64) -> Result<(), Error> {
    session
        .tx
        .send(Up {
            sequence,
            ..Up::default()
        })
        .await?;
    let reply = timeout(Duration::from_secs(3), session.inbound.message())
        .await??
        .ok_or("missing heartbeat")?;
    assert_eq!(reply.sequence, sequence);
    Ok(())
}

#[tokio::test]
async fn tls_verifies_ca_and_server_name() -> Result<(), Error> {
    let host = ProbeHost::start(HANDSHAKE_TIMEOUT, MESSAGE_LIMIT).await?;
    let other_ca = ProbeHost::start(HANDSHAKE_TIMEOUT, MESSAGE_LIMIT).await?;
    assert!(
        host.channel_with_ca(&other_ca.ca_pem, "localhost")
            .await
            .is_err()
    );
    assert!(
        host.channel_with_ca(&host.ca_pem, "wrong.example")
            .await
            .is_err()
    );
    let mut session = open(&host, host.channel().await?, MESSAGE_LIMIT, false).await?;
    heartbeat(&mut session, 1).await
}

#[tokio::test]
async fn machine_credential_is_required_in_metadata() -> Result<(), Error> {
    let host = ProbeHost::start(HANDSHAKE_TIMEOUT, MESSAGE_LIMIT).await?;
    let channel = host.channel().await?;
    let mut client = client(channel.clone(), MESSAGE_LIMIT);
    for invalid in [None, Some("Bearer invalid")] {
        let mut request = Request::new(tokio_stream::iter([Up::default()]));
        if let Some(value) = invalid {
            request
                .metadata_mut()
                .insert("authorization", value.parse()?);
        }
        let error = client
            .control(request)
            .await
            .expect_err("invalid credential accepted");
        assert_eq!(error.code(), Code::Unauthenticated);
    }
    assert_eq!(host.calls(), 0);
    let mut session = open(&host, channel, MESSAGE_LIMIT, false).await?;
    heartbeat(&mut session, 1).await
}

#[tokio::test]
async fn server_pushes_on_two_distinct_connections() -> Result<(), Error> {
    let host = ProbeHost::start(HANDSHAKE_TIMEOUT, MESSAGE_LIMIT).await?;
    let mut control = open(&host, host.channel().await?, MESSAGE_LIMIT, false).await?;
    let mut data = open(&host, host.channel().await?, MESSAGE_LIMIT, true).await?;
    assert_ne!(control.peer, data.peer);
    heartbeat(&mut control, 1).await?;
    heartbeat(&mut data, 2).await?;
    assert_eq!(host.calls(), 2);
    Ok(())
}

#[tokio::test]
async fn message_at_limit_is_accepted() -> Result<(), Error> {
    let host = ProbeHost::start(HANDSHAKE_TIMEOUT, MESSAGE_LIMIT).await?;
    let mut session = open(&host, host.channel().await?, MESSAGE_LIMIT, true).await?;
    session.tx.send(sized_up(MESSAGE_LIMIT)).await?;
    let reply = timeout(Duration::from_secs(3), session.inbound.message())
        .await??
        .ok_or("missing data")?;
    assert_eq!(reply.encoded_len(), MESSAGE_LIMIT);
    heartbeat(&mut session, 1).await
}

#[tokio::test]
async fn oversize_message_is_rejected_without_crash() -> Result<(), Error> {
    for boundary in [
        "client_encode",
        "server_decode",
        "server_encode",
        "client_decode",
    ] {
        let server_limit = if boundary == "client_decode" {
            MESSAGE_LIMIT + 1
        } else {
            MESSAGE_LIMIT
        };
        let client_limit = if boundary == "server_decode" {
            MESSAGE_LIMIT + 1
        } else {
            MESSAGE_LIMIT
        };
        let host = ProbeHost::start(HANDSHAKE_TIMEOUT, server_limit).await?;
        let channel = host.channel().await?;
        let mut session = open(&host, channel.clone(), client_limit, true).await?;
        let oversized = sized_up(MESSAGE_LIMIT + 1);
        if matches!(boundary, "client_decode" | "server_encode") {
            let down = Down {
                payload: oversized.payload,
                ..Down::default()
            };
            assert_eq!(down.encoded_len(), MESSAGE_LIMIT + 1);
            session
                .tx
                .send(Up {
                    response_bytes: down.payload.len() as u32,
                    ..Up::default()
                })
                .await?;
        } else {
            session.tx.send(oversized).await?;
        }
        let error = timeout(Duration::from_secs(3), session.inbound.message())
            .await?
            .expect_err("oversize message accepted");
        eprintln!(
            "size_boundary={boundary} status={:?} detail={}",
            error.code(),
            error.message()
        );
        let expected = if boundary == "client_encode" {
            Code::Internal
        } else {
            Code::OutOfRange
        };
        assert_eq!(error.code(), expected, "{boundary}: {error}");
        drop(session);
        let mut fresh = open(&host, channel, MESSAGE_LIMIT, false).await?;
        heartbeat(&mut fresh, 1).await?;
    }
    Ok(())
}

#[tokio::test]
async fn handshake_timeout_closes_silent_stream() -> Result<(), Error> {
    let host = ProbeHost::start(Duration::from_millis(300), MESSAGE_LIMIT).await?;
    let channel = host.channel().await?;
    let mut client = client(channel.clone(), MESSAGE_LIMIT);
    let (_tx, rx) = mpsc::channel::<Up>(8);
    let start = Instant::now();
    let mut inbound = client
        .control(host.request(ReceiverStream::new(rx)))
        .await?
        .into_inner();
    let error = timeout(Duration::from_secs(2), inbound.message())
        .await?
        .expect_err("silent stream accepted");
    assert_eq!(error.code(), Code::DeadlineExceeded);
    assert!(start.elapsed() >= Duration::from_millis(250));
    assert!(inbound.message().await?.is_none());
    let mut fresh = open(&host, channel, MESSAGE_LIMIT, false).await?;
    sleep(Duration::from_millis(400)).await;
    heartbeat(&mut fresh, 1).await
}

#[tokio::test]
async fn disconnect_ends_stream_without_library_retry() -> Result<(), Error> {
    let host = ProbeHost::start(HANDSHAKE_TIMEOUT, MESSAGE_LIMIT).await?;
    let channel = host.channel().await?;
    let mut session = open(&host, channel.clone(), MESSAGE_LIMIT, false).await?;
    host.disconnect();
    let error = timeout(Duration::from_secs(3), session.inbound.message())
        .await?
        .expect_err("stream survived disconnect");
    eprintln!(
        "disconnect_status={:?} detail={}",
        error.code(),
        error.message()
    );
    assert!(
        matches!(error.code(), Code::Unknown | Code::Unavailable),
        "{error}"
    );
    sleep(Duration::from_millis(300)).await;
    assert_eq!(host.calls(), 1);
    drop(session);
    drop(channel);
    let restarted = ProbeHost::start(HANDSHAKE_TIMEOUT, MESSAGE_LIMIT).await?;
    let mut fresh = open(&restarted, restarted.channel().await?, MESSAGE_LIMIT, false).await?;
    heartbeat(&mut fresh, 1).await
}

#[tokio::test]
async fn graceful_goaway_allows_active_stream_to_finish() -> Result<(), Error> {
    let mut host = ProbeHost::start(HANDSHAKE_TIMEOUT, MESSAGE_LIMIT).await?;
    let mut session = open(&host, host.channel().await?, MESSAGE_LIMIT, false).await?;
    host.begin_graceful_shutdown();
    sleep(Duration::from_millis(100)).await;
    heartbeat(&mut session, 1).await?;
    drop(session.tx);
    assert!(
        timeout(Duration::from_secs(3), session.inbound.message())
            .await??
            .is_none()
    );
    assert_eq!(host.calls(), 1);
    Ok(())
}

#[tokio::test]
async fn control_latency_under_data_flood() -> Result<(), Error> {
    for topology in ["shared", "separate"] {
        let output = timeout(
            Duration::from_secs(40),
            tokio::process::Command::new(env!("CARGO_BIN_EXE_grpc-probe"))
                .arg(topology)
                .kill_on_drop(true)
                .output(),
        )
        .await??;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: Measurement = serde_json::from_slice(&output.stdout)?;
        println!("{}", String::from_utf8(output.stdout)?);
        assert_eq!(result.topology, topology);
        assert_eq!(result.queue_capacity, QUEUE_CAPACITY);
        assert!(result.queue_max > 0 && result.queue_max <= QUEUE_CAPACITY);
        assert!(result.duration_ms >= 10_000);
        assert!(result.data_messages_received >= 10);
        assert_eq!(result.distinct_connections, topology == "separate");
        if topology == "separate" {
            assert!(result.p95_ms < 300.0, "{result:?}");
        }
    }
    Ok(())
}
