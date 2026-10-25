use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use prost::Message;
use serde::{Deserialize, Serialize};
use tokio::{
    sync::mpsc,
    time::{Instant, MissedTickBehavior, timeout},
};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

use crate::{Error, HANDSHAKE_TIMEOUT, MESSAGE_LIMIT, ProbeHost, client, wire::Up};

pub const QUEUE_CAPACITY: usize = 8;
pub const FLOOD_MESSAGE_BYTES: usize = 524_288;
pub const FLOOD_DURATION: Duration = Duration::from_secs(10);

#[derive(Debug, Deserialize, Serialize)]
pub struct Measurement {
    pub topology: String,
    pub duration_ms: u128,
    pub heartbeat_count: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub heartbeat_rtt_ms_sorted: Vec<f64>,
    pub rss_before_kib: u64,
    pub rss_after_kib: u64,
    pub rss_peak_sampled_kib: u64,
    pub queue_capacity: usize,
    pub queue_max: usize,
    pub data_messages_received: usize,
    pub data_message_bytes: usize,
    pub distinct_connections: bool,
}

pub fn sized_up(serialized_bytes: usize) -> Up {
    let mut message = Up {
        payload: vec![0x5a; serialized_bytes],
        ..Up::default()
    };
    let overhead = message.encoded_len() - serialized_bytes;
    message.payload.truncate(serialized_bytes - overhead);
    assert_eq!(message.encoded_len(), serialized_bytes);
    message
}

fn rss_kib() -> Result<u64, Error> {
    let status = std::fs::read_to_string("/proc/self/status")?;
    let rss = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .ok_or("VmRSS is unavailable")?;
    Ok(rss
        .split_whitespace()
        .next()
        .ok_or("invalid VmRSS")?
        .parse()?)
}

fn percentile(sorted: &[f64], percentile: usize) -> f64 {
    sorted[(sorted.len() * percentile).div_ceil(100) - 1]
}

pub async fn run(shared: bool) -> Result<Measurement, Error> {
    let host = ProbeHost::start(HANDSHAKE_TIMEOUT, MESSAGE_LIMIT).await?;
    let control_channel = host.channel().await?;
    let data_channel = if shared {
        control_channel.clone()
    } else {
        host.channel().await?
    };
    let mut control_client = client(control_channel, MESSAGE_LIMIT);
    let mut data_client = client(data_channel, MESSAGE_LIMIT);
    let (control_tx, control_rx) = mpsc::channel(8);
    let (data_tx, data_rx) = mpsc::channel(QUEUE_CAPACITY);
    control_tx.send(Up::default()).await?;
    data_tx.send(Up::default()).await?;
    let control_response = control_client
        .control(host.request(ReceiverStream::new(control_rx)))
        .await?;
    let data_response = data_client
        .data(host.request(ReceiverStream::new(data_rx)))
        .await?;
    let distinct_connections = control_response.metadata().get("x-probe-peer")
        != data_response.metadata().get("x-probe-peer");
    assert_eq!(distinct_connections, !shared);
    let mut control = control_response.into_inner();
    let mut data = data_response.into_inner();
    timeout(Duration::from_secs(3), control.message())
        .await??
        .ok_or("missing control welcome")?;
    timeout(Duration::from_secs(3), data.message())
        .await??
        .ok_or("missing data welcome")?;

    let rss_before_kib = rss_kib()?;
    let mut rss_peak_sampled_kib = rss_before_kib;
    let cancel = CancellationToken::new();
    let producer_cancel = cancel.clone();
    let queue_max = Arc::new(AtomicUsize::new(0));
    let producer_queue_max = queue_max.clone();
    let producer = tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                _ = producer_cancel.cancelled() => break,
                permit = data_tx.reserve() => {
                    let permit = permit?;
                    let occupied = QUEUE_CAPACITY - data_tx.capacity();
                    producer_queue_max.fetch_max(occupied, Ordering::SeqCst);
                    permit.send(sized_up(FLOOD_MESSAGE_BYTES));
                }
            }
        }
        Ok::<_, Error>(())
    });
    let consumer = tokio::spawn(async move {
        let mut received = 0;
        while let Some(message) = data.message().await? {
            if message.encoded_len() != FLOOD_MESSAGE_BYTES {
                return Err("unexpected data message size".into());
            }
            received += 1;
        }
        Ok::<_, Error>(received)
    });

    let start = Instant::now();
    let end = start + FLOOD_DURATION;
    let mut tick = tokio::time::interval_at(start, Duration::from_millis(100));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut latencies = Vec::new();
    loop {
        tokio::select! {
            biased;
            _ = tokio::time::sleep_until(end) => break,
            _ = tick.tick() => {}
        }
        let sequence = (latencies.len() + 1) as u64;
        let heartbeat_start = Instant::now();
        timeout(Duration::from_secs(2), async {
            control_tx
                .send(Up {
                    sequence,
                    ..Up::default()
                })
                .await?;
            let reply = control.message().await?.ok_or("control stream ended")?;
            if reply.sequence != sequence {
                return Err("unexpected heartbeat sequence".into());
            }
            Ok::<_, Error>(())
        })
        .await??;
        latencies.push(heartbeat_start.elapsed().as_secs_f64() * 1000.0);
        rss_peak_sampled_kib = rss_peak_sampled_kib.max(rss_kib()?);
    }
    let duration_ms = start.elapsed().as_millis();
    let rss_after_kib = rss_kib()?;
    rss_peak_sampled_kib = rss_peak_sampled_kib.max(rss_after_kib);
    cancel.cancel();
    timeout(Duration::from_secs(3), producer).await???;
    let data_messages_received = timeout(Duration::from_secs(3), consumer).await???;
    latencies.sort_by(f64::total_cmp);
    if latencies.is_empty() || data_messages_received < 10 {
        return Err("insufficient measurement samples".into());
    }
    Ok(Measurement {
        topology: if shared { "shared" } else { "separate" }.into(),
        duration_ms,
        heartbeat_count: latencies.len(),
        p50_ms: percentile(&latencies, 50),
        p95_ms: percentile(&latencies, 95),
        p99_ms: percentile(&latencies, 99),
        heartbeat_rtt_ms_sorted: latencies,
        rss_before_kib,
        rss_after_kib,
        rss_peak_sampled_kib,
        queue_capacity: QUEUE_CAPACITY,
        queue_max: queue_max.load(Ordering::SeqCst),
        data_messages_received,
        data_message_bytes: FLOOD_MESSAGE_BYTES,
        distinct_connections,
    })
}
