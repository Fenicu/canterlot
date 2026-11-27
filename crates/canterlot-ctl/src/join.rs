//! `canterlot join`: привязка машины к хабу по одноразовому коду.
//!
//! Код уходит хабу только после двух проверок. Сначала УЦ, полученный без доверия
//! (`GetCa` через проверку, которая лишь запоминает цепочку), сверяется с `--pin`. Затем
//! открывается новое соединение, проверяющее цепочку этим УЦ и имя `--hub` в SAN; код
//! читается и отправляется только по нему. Соединение без доверия больше ни для чего не служит.

use std::{
    fmt, io,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use canterlot_proto::enroll::v1::{
    GetCaRequest, JoinRequest, worker_enroll_client::WorkerEnrollClient,
};
use hyper_util::rt::TokioIo;
use rustls::{
    CertificateError, ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme,
    client::{
        WebPkiServerVerifier,
        danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    },
    crypto::CryptoProvider,
    pki_types::{CertificateDer, ServerName, UnixTime},
};
use tonic::transport::{Channel, Endpoint, Uri};
use uuid::Uuid;

use crate::{
    cli::{JoinArgs, Version},
    error::CtlError,
    layout::Layout,
    ops::{self, OpKind, OpRecord, OpResult},
    release::{self, ReleaseSource},
    render,
    runner::Runner,
    stack::{cmd, run_ok},
    state::{self, WorkerClass},
    tls::pin_of,
    worker::{self, MachineIds, WorkerHost},
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub struct JoinReport {
    pub machine_id: Uuid,
    pub version: Version,
    pub hub_url: String,
    pub notes: Vec<String>,
}

impl fmt::Display for JoinReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "canterlot worker {} is connected to {} as machine {}.",
            self.version, self.hub_url, self.machine_id
        )?;
        for note in &self.notes {
            writeln!(f, "Note: {note}")?;
        }
        Ok(())
    }
}

/// Адрес хаба `host:port`; IPv6 — в квадратных скобках.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HubAddr {
    pub host: String,
    pub port: u16,
}

impl HubAddr {
    pub fn parse(text: &str) -> Result<Self, CtlError> {
        let bad = || CtlError::BadHubAddress(text.into());
        let text = text.trim();
        let text = text
            .strip_prefix("https://")
            .unwrap_or(text)
            .trim_end_matches('/');
        let (host, port) = if let Some(rest) = text.strip_prefix('[') {
            let (host, rest) = rest.split_once(']').ok_or_else(bad)?;
            (host, rest.strip_prefix(':').ok_or_else(bad)?)
        } else {
            text.rsplit_once(':').ok_or_else(bad)?
        };
        let port: u16 = port.parse().map_err(|_| bad())?;
        if host.is_empty() || host.contains(['/', ' ', '@']) || port == 0 {
            return Err(bad());
        }
        Ok(Self {
            host: host.to_owned(),
            port,
        })
    }

    pub fn authority(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    fn server_name(&self) -> Result<ServerName<'static>, CtlError> {
        ServerName::try_from(self.host.clone())
            .map_err(|_| CtlError::BadHubAddress(self.authority()))
    }
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Принимает любую цепочку и запоминает её. Годится только для `GetCa`: ответ затем
/// сверяется с `--pin`, а код по этому соединению не идёт.
#[derive(Debug)]
struct CaptureOnly {
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for CaptureOnly {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

type Chain = Vec<CertificateDer<'static>>;

/// Полная проверка webpki; принятая цепочка запоминается, чтобы потом сверить с ней
/// адрес, который вернёт хаб.
#[derive(Debug)]
struct Recording {
    inner: Arc<WebPkiServerVerifier>,
    chain: Mutex<Option<Chain>>,
}

impl ServerCertVerifier for Recording {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let verified =
            self.inner
                .verify_server_cert(end_entity, intermediates, server_name, ocsp, now)?;
        let mut chain = vec![end_entity.clone().into_owned()];
        chain.extend(intermediates.iter().map(|c| c.clone().into_owned()));
        *self.chain.lock().expect("chain lock") = Some(chain);
        Ok(verified)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

fn client_config(verifier: Arc<dyn ServerCertVerifier>) -> Result<ClientConfig, CtlError> {
    let mut config = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|err| CtlError::Other(format!("tls_error: {err}")))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec()];
    Ok(config)
}

type TlsSlot = Arc<Mutex<Option<rustls::Error>>>;

/// Канал tonic поверх своего TLS: так проверку выбирает утилита, а не `ClientTlsConfig`.
/// Ошибка рукопожатия rustls кладётся в `slot`, чтобы отличить чужое имя от чужого УЦ.
async fn channel(addr: &HubAddr, config: ClientConfig, slot: TlsSlot) -> Result<Channel, CtlError> {
    let config = Arc::new(config);
    let name = addr.server_name()?;
    let target = (addr.host.clone(), addr.port);
    let connector = tower::service_fn(move |_: Uri| {
        let config = config.clone();
        let name = name.clone();
        let target = target.clone();
        let slot = slot.clone();
        async move {
            let tcp = tokio::net::TcpStream::connect((target.0.as_str(), target.1)).await?;
            let tls = tokio_rustls::TlsConnector::from(config)
                .connect(name, tcp)
                .await
                .inspect_err(|err| {
                    if let Some(tls) = err
                        .get_ref()
                        .and_then(|inner| inner.downcast_ref::<rustls::Error>())
                    {
                        *slot.lock().expect("tls slot") = Some(tls.clone());
                    }
                })?;
            Ok::<_, io::Error>(TokioIo::new(tls))
        }
    });
    Endpoint::from_shared(format!("http://{}", addr.authority()))
        .map_err(|_| CtlError::BadHubAddress(addr.authority()))?
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(CALL_TIMEOUT)
        .connect_with_connector(connector)
        .await
        .map_err(|err| CtlError::HubUnreachable(format!("{}: {err}", addr.authority())))
}

/// Шаг 1: УЦ без доверия и сверка с `--pin`.
async fn fetch_pinned_ca(addr: &HubAddr, pin: &str) -> Result<Vec<u8>, CtlError> {
    let capture = Arc::new(CaptureOnly {
        provider: provider(),
    });
    let channel = channel(addr, client_config(capture)?, TlsSlot::default()).await?;
    let ca_der = WorkerEnrollClient::new(channel)
        .get_ca(GetCaRequest {})
        .await
        .map_err(|status| CtlError::JoinRefused(status.message().to_owned()))?
        .into_inner()
        .ca_der;
    let got = pin_of(&ca_der);
    if !got.eq_ignore_ascii_case(pin.trim()) {
        return Err(CtlError::PinMismatch {
            want: pin.trim().to_owned(),
            got,
        });
    }
    Ok(ca_der)
}

struct Verified {
    channel: Channel,
    verifier: Arc<WebPkiServerVerifier>,
    chain: Chain,
}

/// Шаг 2: новое соединение с проверкой цепочки по закреплённому УЦ и имени `--hub`.
async fn verified_channel(addr: &HubAddr, ca_der: &[u8]) -> Result<Verified, CtlError> {
    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from(ca_der.to_vec()))
        .map_err(|err| CtlError::TlsUntrusted(format!("unusable CA certificate: {err}")))?;
    let inner = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider())
        .build()
        .map_err(|err| CtlError::TlsUntrusted(err.to_string()))?;
    let recording = Arc::new(Recording {
        inner: inner.clone(),
        chain: Mutex::new(None),
    });
    let slot = TlsSlot::default();
    let result = channel(addr, client_config(recording.clone())?, slot.clone()).await;
    let failure = slot.lock().expect("tls slot").take();
    match (result, failure) {
        (Ok(channel), _) => {
            let chain = recording
                .chain
                .lock()
                .expect("chain lock")
                .take()
                .ok_or_else(|| CtlError::TlsUntrusted("no verified chain".into()))?;
            Ok(Verified {
                channel,
                verifier: inner,
                chain,
            })
        }
        (
            Err(_),
            Some(rustls::Error::InvalidCertificate(
                CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. },
            )),
        ) => Err(CtlError::SanMismatch {
            host: addr.host.clone(),
        }),
        (Err(_), Some(err)) => Err(CtlError::TlsUntrusted(err.to_string())),
        (Err(err), None) => Err(err),
    }
}

/// Покрывает ли проверенная цепочка другой адрес (тот, что хаб вернул в ответе).
fn chain_names(verified: &Verified, addr: &HubAddr) -> bool {
    let Ok(name) = addr.server_name() else {
        return false;
    };
    let Some((end_entity, intermediates)) = verified.chain.split_first() else {
        return false;
    };
    verified
        .verifier
        .verify_server_cert(end_entity, intermediates, &name, &[], UnixTime::now())
        .is_ok()
}

fn read_code(args: &JoinArgs, host: &WorkerHost) -> Result<String, CtlError> {
    let code = match args.code.as_deref() {
        Some("-") => {
            let mut line = String::new();
            io::stdin().read_line(&mut line)?;
            line
        }
        Some(code) => code.to_owned(),
        None => host.ask("Join code", "--code")?,
    };
    let code = code.trim().to_owned();
    if code.is_empty() {
        return Err(CtlError::BadAnswer("empty join code".into()));
    }
    Ok(code)
}

struct Joined {
    ids: MachineIds,
    token: String,
    ca_pem: String,
    hub: HubAddr,
    notes: Vec<String>,
}

fn parse_uuid(text: &str, field: &str) -> Result<Uuid, CtlError> {
    text.parse()
        .map_err(|_| CtlError::BadOutput(format!("join response {field}")))
}

/// Шаги 1–3: проверки, код, обмен. Ни одна ошибка не несёт кода или токена.
async fn exchange(
    args: &JoinArgs,
    addr: &HubAddr,
    host: &WorkerHost,
    version: &Version,
) -> Result<Joined, CtlError> {
    let ca_der = fetch_pinned_ca(addr, &args.pin).await?;
    let verified = verified_channel(addr, &ca_der).await?;
    let code = read_code(args, host)?;
    let reply = WorkerEnrollClient::new(verified.channel.clone())
        .join(JoinRequest {
            code,
            hostname: host.hostname.clone(),
            worker_version: version.to_string(),
        })
        .await
        .map_err(|status| CtlError::JoinRefused(status.message().to_owned()))?
        .into_inner();
    if reply.ca_der != ca_der {
        return Err(CtlError::JoinRefused(
            "ca_changed: the hub answered with a different CA than the pinned one".into(),
        ));
    }
    let ids = MachineIds {
        machine_id: parse_uuid(&reply.machine_id, "machine_id")?,
        installation_id: parse_uuid(&reply.installation_id, "installation_id")?,
        restore_generation: parse_uuid(&reply.restore_generation, "restore_generation")?,
    };
    if reply.token.is_empty() {
        return Err(CtlError::BadOutput("join response token".into()));
    }
    // Адрес из ответа берётся, только если его называет уже проверенный сертификат;
    // иначе воркер ходит туда, где имя проверено.
    let mut notes = Vec::new();
    let mut hub = addr.clone();
    if !reply.grpc_endpoint.is_empty()
        && let Ok(advertised) = HubAddr::parse(&reply.grpc_endpoint)
        && advertised != *addr
    {
        if chain_names(&verified, &advertised) {
            hub = advertised;
        } else {
            notes.push(format!(
                "the hub advertises {}, which its certificate does not name; this worker uses {}",
                advertised.authority(),
                addr.authority()
            ));
        }
    }
    Ok(Joined {
        ids,
        token: reply.token,
        ca_pem: pem::encode(&pem::Pem::new("CERTIFICATE", ca_der)),
        hub,
        notes,
    })
}

fn read_machine_id(layout: &Layout) -> Result<Uuid, CtlError> {
    let text = std::fs::read_to_string(layout.worker_config_dir().join("worker.toml"))?;
    let table: toml::Table = toml::from_str(&text)?;
    table
        .get("machine_id")
        .and_then(|v| v.as_str())
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| CtlError::BadOutput("worker.toml machine_id".into()))
}

fn read_hub_url(layout: &Layout) -> Result<String, CtlError> {
    let text = std::fs::read_to_string(layout.worker_config_dir().join("worker.toml"))?;
    let table: toml::Table = toml::from_str(&text)?;
    table
        .get("hub_url")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .ok_or_else(|| CtlError::BadOutput("worker.toml hub_url".into()))
}

fn identity_present(layout: &Layout) -> bool {
    let dir = layout.worker_config_dir();
    dir.join("worker.toml").is_file() && dir.join("token").is_file()
}

/// `show-user` своего пользователя доступен без прав: `sudo -n` здесь только мешает.
async fn linger_is_on(runner: &dyn Runner, user: &str) -> bool {
    let shown = runner
        .run(cmd("loginctl", &["show-user", user, "-p", "Linger"]))
        .await;
    matches!(&shown, Ok(out) if out.status == 0
        && String::from_utf8_lossy(&out.stdout)
            .lines()
            .any(|line| line.trim() == "Linger=yes"))
}

/// Включение своего пользователя обычно разрешено polkit без sudo; sudo — запасной путь.
/// Код `enable-linger` сам по себе ничего не говорит о состоянии, поэтому его читаем заново.
async fn enable_linger(runner: &dyn Runner, host: &WorkerHost, notes: &mut Vec<String>) {
    if linger_is_on(runner, &host.user).await {
        return;
    }
    let _ = runner
        .run(cmd("loginctl", &["enable-linger", &host.user]))
        .await;
    if !linger_is_on(runner, &host.user).await {
        let _ = runner
            .run(cmd(
                "sudo",
                &["-n", "loginctl", "enable-linger", &host.user],
            ))
            .await;
    }
    if !linger_is_on(runner, &host.user).await {
        notes.push(format!(
            "the worker stops when you log out until lingering is on: run `sudo loginctl enable-linger {}`",
            host.user
        ));
    }
}

pub async fn join(
    args: &JoinArgs,
    layout: &Layout,
    runner: &dyn Runner,
    src: &ReleaseSource,
    host: &WorkerHost,
) -> Result<JoinReport, CtlError> {
    let addr = HubAddr::parse(&args.hub)?;
    let ops_dir = layout.worker_ops_dir();
    let _lock = ops::lock(&ops_dir)?;
    // Под блокировкой незавершённая запись — след упавшего процесса: тождество, если оно
    // успело записаться, берётся из файлов, иначе код ещё не потрачен.
    if let Some(mut stale) = ops::unfinished(&ops_dir)? {
        ops::finish(
            &ops_dir,
            &mut stale,
            OpResult::FailedBeforeChange {
                reason: "interrupted; superseded by a new join".into(),
            },
        )?;
    }
    let class = state::classify_worker(layout)?;
    if class == WorkerClass::Installed && !args.replace {
        return Err(CtlError::AlreadyJoined);
    }
    // `--from-dir` задаёт версию каталогом; `--version` с ним несовместим в CLI.
    let version = match &args.from_dir {
        Some(dir) => {
            let found = release::dir_version(dir)?;
            if let Some(want) = &args.version
                && want != &found
            {
                return Err(CtlError::Other(format!(
                    "version_mismatch: --version {want} but {} holds {found}",
                    dir.display()
                )));
            }
            found
        }
        None => match &args.version {
            Some(v) => v.clone(),
            None => Version::parse(env!("CARGO_PKG_VERSION"))?,
        },
    };
    let mut rec = ops::begin(
        &ops_dir,
        OpRecord::new(OpKind::Install, Some(version.clone())),
    )?;
    match run(
        args, &addr, layout, runner, src, host, &version, class, &ops_dir, &mut rec,
    )
    .await
    {
        Ok(report) => {
            ops::finish(&ops_dir, &mut rec, OpResult::Done)?;
            Ok(report)
        }
        Err(err) => {
            let _ = ops::finish(
                &ops_dir,
                &mut rec,
                OpResult::FailedBeforeChange {
                    reason: err.to_string(),
                },
            );
            Err(err)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run(
    args: &JoinArgs,
    addr: &HubAddr,
    layout: &Layout,
    runner: &dyn Runner,
    src: &ReleaseSource,
    host: &WorkerHost,
    version: &Version,
    class: WorkerClass,
    ops_dir: &Path,
    rec: &mut OpRecord,
) -> Result<JoinReport, CtlError> {
    let home = worker::home_of(layout)?;
    // Тождество без юнита — прошлый join упал после обмена: код второй раз не нужен.
    let resume = class != WorkerClass::Installed && identity_present(layout);
    let mut notes = Vec::new();
    let (machine_id, hub_url) = if resume {
        (read_machine_id(layout)?, read_hub_url(layout)?)
    } else {
        ops::advance(ops_dir, rec, "exchange")?;
        let joined = exchange(args, addr, host, version).await?;
        let hub_url = format!("https://{}", joined.hub.authority());
        let agents = worker::detect_agents(runner, None).await;
        worker::write_identity(
            layout,
            &joined.ids,
            &joined.ca_pem,
            &joined.token,
            &hub_url,
            &agents,
        )?;
        notes.extend(joined.notes);
        (joined.ids.machine_id, hub_url)
    };

    ops::advance(ops_dir, rec, "release")?;
    let assets = release::asset_names(version);
    let names = [assets.worker.as_str(), "SHA256SUMS"];
    let (staged, fetched) = match &args.from_dir {
        Some(dir) => (release::from_dir(dir, &names)?, false),
        None => {
            let staging = layout.worker_staging();
            crate::fsx::ensure_dir(home, &staging, 0o700)?;
            (release::fetch(src, version, &names, &staging).await?, true)
        }
    };
    if staged.version != *version {
        return Err(CtlError::Other(format!(
            "version_mismatch: {} holds {}, the operation targets {version}",
            staged.dir.display(),
            staged.version
        )));
    }
    let archive = staged
        .files
        .get(&assets.worker)
        .ok_or_else(|| CtlError::AssetMissing(assets.worker.clone()))?;
    let installed = worker::install_release(layout, archive, version);
    if fetched {
        let _ = std::fs::remove_dir_all(&staged.dir);
    }
    installed?;
    crate::fsx::replace_symlink(
        &Path::new("releases").join(version.as_str()),
        &layout.worker_current(),
    )?;
    if let Some(exe) = &host.self_exe {
        worker::install_ctl(layout, exe, version)?;
    }

    ops::advance(ops_dir, rec, "unit")?;
    let unit = layout.worker_unit();
    if let Some(dir) = unit.parent() {
        crate::fsx::ensure_dir(home, dir, 0o755)?;
    }
    crate::fsx::write_atomic_mode(&unit, render::render_worker_unit().as_bytes(), 0o644)?;
    enable_linger(runner, host, &mut notes).await;
    run_ok(
        runner,
        "systemctl --user daemon-reload",
        worker::systemctl_user(&["daemon-reload"]),
    )
    .await?;
    run_ok(
        runner,
        "systemctl --user enable",
        worker::systemctl_user(&["enable", "--now", "canterlot-worker"]),
    )
    .await?;
    if class == WorkerClass::Installed {
        run_ok(
            runner,
            "systemctl --user restart",
            worker::systemctl_user(&["restart", "canterlot-worker"]),
        )
        .await?;
    }

    ops::advance(ops_dir, rec, "connect")?;
    worker::poll(host, || async {
        let status = worker::worker_status(runner, layout)
            .await
            .map_err(|err| err.to_string())?;
        if status.connected && status.version == version.as_str() {
            Ok(())
        } else {
            Err(format!(
                "worker {} is not connected yet (version {})",
                machine_id, status.version
            ))
        }
    })
    .await
    .map_err(CtlError::NotReady)?;
    Ok(JoinReport {
        machine_id,
        version: version.clone(),
        hub_url,
        notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_address_forms() {
        assert_eq!(
            HubAddr::parse("hub.example.internal:7443").unwrap(),
            HubAddr {
                host: "hub.example.internal".into(),
                port: 7443
            }
        );
        let v6 = HubAddr::parse("[2001:db8::1]:7443").unwrap();
        assert_eq!(v6.host, "2001:db8::1");
        assert_eq!(v6.authority(), "[2001:db8::1]:7443");
        assert_eq!(
            HubAddr::parse("https://192.0.2.10:7443/").unwrap().host,
            "192.0.2.10"
        );
        for bad in ["hub.example.internal", "hub:0", ":7443", "a/b:1", "[::1]"] {
            assert!(HubAddr::parse(bad).is_err(), "{bad}");
        }
    }
}
