use super::*;
use canterlot_hub::{fault_injection::Faults, testkit::TestHub};
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint};

#[derive(Clone)]
struct Proxy {
    control: Channel,
    data: Channel,
    faults: Faults,
    hellos: Arc<Mutex<Vec<Hello>>>,
    reports: Arc<Mutex<Vec<ReconcileReport>>>,
    release: tokio::sync::watch::Receiver<bool>,
    drop_sj_acks: Arc<std::sync::atomic::AtomicUsize>,
    batches: Arc<Mutex<Vec<JournalRecords>>>,
    reject_data: Arc<std::sync::atomic::AtomicBool>,
    data_release: tokio::sync::watch::Receiver<bool>,
    echo_exec: bool,
}

pub struct DataProxy {
    pub hellos: Arc<Mutex<Vec<Hello>>>,
    pub reports: Arc<Mutex<Vec<ReconcileReport>>>,
    pub release: tokio::sync::watch::Sender<bool>,
    pub url: String,
    pub drop_sj_acks: Arc<std::sync::atomic::AtomicUsize>,
    pub batches: Arc<Mutex<Vec<JournalRecords>>>,
    pub reject_data: Arc<std::sync::atomic::AtomicBool>,
    pub data_release: tokio::sync::watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl Drop for DataProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl DataProxy {
    pub async fn start(hub: &TestHub, hold_report: bool, echo_exec: bool) -> Self {
        let endpoint = Endpoint::from_shared(format!("https://localhost:{}", hub.addr.port()))
            .unwrap()
            .tls_config(
                ClientTlsConfig::new().ca_certificate(Certificate::from_pem(&hub.pki.ca_pem)),
            )
            .unwrap();
        let (release, released) = tokio::sync::watch::channel(!hold_report);
        let (data_release, data_released) = tokio::sync::watch::channel(true);
        let service = Proxy {
            control: endpoint.connect().await.unwrap(),
            data: endpoint.connect().await.unwrap(),
            faults: hub.faults.clone(),
            hellos: Arc::default(),
            reports: Arc::default(),
            release: released,
            drop_sj_acks: Arc::default(),
            batches: Arc::default(),
            reject_data: Arc::default(),
            data_release: data_released,
            echo_exec,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "https://localhost:{}",
            listener.local_addr().unwrap().port()
        );
        let hellos = service.hellos.clone();
        let reports = service.reports.clone();
        let drop_sj_acks = service.drop_sj_acks.clone();
        let batches = service.batches.clone();
        let reject_data = service.reject_data.clone();
        let cert = hub.pki.server_cert_pem.clone();
        let key = hub.pki.server_key_pem.clone();
        let task = tokio::spawn(async move {
            Server::builder()
                .tls_config(ServerTlsConfig::new().identity(Identity::from_pem(cert, key)))
                .unwrap()
                .add_service(
                    WorkerControlServer::new(service.clone())
                        .max_decoding_message_size(MAX_MESSAGE_BYTES)
                        .max_encoding_message_size(MAX_MESSAGE_BYTES),
                )
                .add_service(
                    WorkerDataServer::new(service)
                        .max_decoding_message_size(MAX_MESSAGE_BYTES)
                        .max_encoding_message_size(MAX_MESSAGE_BYTES),
                )
                .serve_with_incoming(TcpListenerStream::new(listener))
                .await
                .unwrap();
        });
        Self {
            hellos,
            reports,
            release,
            url,
            drop_sj_acks,
            batches,
            reject_data,
            data_release,
            task,
        }
    }
}

#[tonic::async_trait]
impl WorkerControl for Proxy {
    type ControlStream = ReceiverStream<Result<ControlDown, Status>>;
    async fn control(
        &self,
        request: Request<Streaming<ControlUp>>,
    ) -> Result<Response<Self::ControlStream>, Status> {
        let metadata = request.metadata().clone();
        let mut inbound = request.into_inner();
        let mut first = inbound.message().await?.unwrap();
        if let Some(control_up::Msg::Hello(hello)) = &first.msg {
            self.hellos.lock().unwrap().push(hello.clone());
        }
        if !self.echo_exec
            && let Some(control_up::Msg::Hello(hello)) = &mut first.msg
        {
            hello.optional_capabilities.retain(|cap| cap != "exec_v1");
        }
        let (up, rx) = mpsc::channel(8);
        up.send(first).await.unwrap();
        let mut request = Request::new(ReceiverStream::new(rx));
        *request.metadata_mut() = metadata;
        let mut backend = worker_control_client::WorkerControlClient::new(self.control.clone())
            .max_decoding_message_size(MAX_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_MESSAGE_BYTES)
            .control(request)
            .await?
            .into_inner();
        let (down, rx) = mpsc::channel(8);
        let reports = self.reports.clone();
        let mut release = self.release.clone();
        tokio::spawn(async move {
            let mut held = None;
            loop {
                tokio::select! {
                    _ = down.closed() => break,
                    _ = release.changed(), if held.is_some() => { if *release.borrow() && let Some(message) = held.take() { let _ = up.send(message).await; } },
                    message = inbound.message() => match message {
                        Ok(Some(message)) => {
                            if let Some(control_up::Msg::ReconcileReport(report)) = &message.msg {
                                reports.lock().unwrap().push(report.clone());
                                if !*release.borrow() { held = Some(message); continue; }
                            }
                            if up.send(message).await.is_err() { break; }
                        }
                        _ => break,
                    },
                    message = backend.message() => match message {
                        Ok(Some(message)) => { if down.send(Ok(message)).await.is_err() { break; } },
                        Err(error) => { let _ = down.send(Err(error)).await; break; },
                        Ok(None) => break,
                    }
                }
            }
        });
        Ok(Response::new(ReceiverStream::new(rx)))
    }
}

#[tonic::async_trait]
impl WorkerData for Proxy {
    type FetchAttachmentStream =
        ReceiverStream<Result<canterlot_proto::worker::v1::AttachmentChunk, Status>>;

    async fn fetch_attachment(
        &self,
        _request: Request<canterlot_proto::worker::v1::FetchAttachmentRequest>,
    ) -> Result<Response<Self::FetchAttachmentStream>, Status> {
        Err(Status::unimplemented("attachment fetch is not implemented"))
    }

    async fn put_media(
        &self,
        _request: Request<Streaming<canterlot_proto::worker::v1::MediaChunk>>,
    ) -> Result<Response<canterlot_proto::worker::v1::MediaStored>, Status> {
        Err(Status::unimplemented("media upload is not implemented"))
    }

    type DataStream = ReceiverStream<Result<DataDown, Status>>;
    async fn data(
        &self,
        request: Request<Streaming<DataUp>>,
    ) -> Result<Response<Self::DataStream>, Status> {
        let metadata = request.metadata().clone();
        let mut inbound = request.into_inner();
        let mut first = inbound.message().await?.unwrap();
        if let Some(data_up::Msg::Hello(hello)) = &first.msg {
            self.hellos.lock().unwrap().push(hello.clone());
        }
        if !self.echo_exec
            && let Some(data_up::Msg::Hello(hello)) = &mut first.msg
        {
            hello.optional_capabilities.retain(|cap| cap != "exec_v1");
        }
        let mut release = self.data_release.clone();
        release
            .wait_for(|released| *released)
            .await
            .map_err(|_| Status::cancelled("proxy stopped"))?;
        if self
            .reject_data
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            let (tx, rx) = mpsc::channel(1);
            tx.send(Ok(DataDown {
                msg: Some(data_down::Msg::HelloReply(HelloReply {
                    result: Some(hello_reply::Result::Rejected(Rejected {
                        reason: RejectReason::ConnectionBindingMismatch as i32,
                        retryable: true,
                        detail: String::new(),
                    })),
                })),
            }))
            .await
            .unwrap();
            return Ok(Response::new(ReceiverStream::new(rx)));
        }
        let (up, rx) = mpsc::channel(8);
        up.send(first).await.unwrap();
        let mut request = Request::new(ReceiverStream::new(rx));
        *request.metadata_mut() = metadata;
        let mut backend = worker_data_client::WorkerDataClient::new(self.data.clone())
            .max_decoding_message_size(MAX_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_MESSAGE_BYTES)
            .data(request)
            .await?
            .into_inner();
        let mut failures = self.faults.data_failures();
        let drop_sj_acks = self.drop_sj_acks.clone();
        let batches = self.batches.clone();
        let (down, rx) = mpsc::channel(8);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = down.closed() => break,
                    Ok(code) = failures.recv() => { let _ = down.send(Err(Status::new(code, "injected data failure"))).await; break; },
                    message = inbound.message() => match message {
                        Ok(Some(message)) => {
                            if let Some(data_up::Msg::SjRecords(batch)) = &message.msg { batches.lock().unwrap().push(batch.clone()); }
                            if up.send(message).await.is_err() { break; }
                        },
                        _ => break,
                    },
                    message = backend.message() => match message {
                        Ok(Some(message)) => {
                            if matches!(message.msg, Some(data_down::Msg::SjAck(_))) && drop_sj_acks.try_update(std::sync::atomic::Ordering::SeqCst, std::sync::atomic::Ordering::SeqCst, |n| n.checked_sub(1)).is_ok() { continue; }
                            if down.send(Ok(message)).await.is_err() { break; }
                        },
                        Err(error) => { let _ = down.send(Err(error)).await; break; },
                        Ok(None) => break,
                    }
                }
            }
        });
        Ok(Response::new(ReceiverStream::new(rx)))
    }
}
