use sqlx::{PgPool, types::Json};
use uuid::Uuid;

use crate::model::HoldReason;

pub struct TestHub {
    #[cfg(feature = "fault-injection")]
    pub faults: crate::fault_injection::Faults,
    pub addr: std::net::SocketAddr,
    pub pki: tls::TestPki,
    pub installation: crate::installation::Installation,
    pub registry: crate::session::Registry,
    pub dispatcher: crate::dispatcher::Dispatcher,
    pub shutdown: tokio::sync::oneshot::Sender<()>,
    pub join: tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
}

pub async fn spawn_test_hub(pool: PgPool, timers: crate::config::HubTimers) -> TestHub {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test listener");
    spawn_on(
        pool,
        timers,
        listener,
        tls::test_pki(),
        #[cfg(feature = "fault-injection")]
        Default::default(),
    )
    .await
}

#[cfg(feature = "fault-injection")]
pub async fn spawn_test_hub_with_faults(
    pool: PgPool,
    timers: crate::config::HubTimers,
    addr: Option<std::net::SocketAddr>,
    pki: tls::TestPki,
    faults: crate::fault_injection::Faults,
) -> TestHub {
    let listener =
        tokio::net::TcpListener::bind(addr.unwrap_or_else(|| "127.0.0.1:0".parse().unwrap()))
            .await
            .expect("test listener");
    spawn_on(pool, timers, listener, pki, faults).await
}

async fn spawn_on(
    pool: PgPool,
    timers: crate::config::HubTimers,
    listener: tokio::net::TcpListener,
    pki: tls::TestPki,
    #[cfg(feature = "fault-injection")] faults: crate::fault_injection::Faults,
) -> TestHub {
    use crate::{dispatcher, grpc, ingest, installation, outbox, session::Registry};
    let directory = tempfile::tempdir().expect("test installation directory");
    let manifest = directory.path().join("manifest");
    let existing: Option<installation::Installation> =
        sqlx::query_as("select installation_id, restore_generation, owner_id from installation")
            .fetch_optional(&pool)
            .await
            .expect("test installation lookup");
    if let Some(existing) = existing {
        std::fs::write(&manifest, format!(
            "installation_id = \"{}\"\nrestore_generation = \"{}\"\nrestore_in_progress = false\n",
            existing.installation_id, existing.restore_generation,
        )).expect("test manifest");
    } else {
        installation::init(&pool, &manifest)
            .await
            .expect("test installation init");
    }
    let installation = installation::check(&pool, &manifest)
        .await
        .expect("test installation check");
    let registry = Registry::new(timers.clone());
    let (dispatcher, rx) = dispatcher::Dispatcher::channel();
    let mut state =
        grpc::HubGrpc::new(pool.clone(), registry.clone(), installation, timers.clone());
    let ingest = ingest::Ingest::new(pool.clone()).with_dispatcher(dispatcher.clone());
    #[cfg(feature = "fault-injection")]
    let ingest = {
        state.faults = faults.clone();
        let mut ingest = ingest;
        ingest.faults = faults.clone();
        ingest
    };
    state.ingest = std::sync::Arc::new(ingest);
    let cert = pki.server_cert_pem.clone();
    let key = pki.server_key_pem.clone();
    let addr = listener.local_addr().expect("test address");
    let (shutdown, stop) = tokio::sync::oneshot::channel();
    let (stopping, stopped) = tokio::sync::watch::channel(false);
    let outbox = tokio::spawn(outbox::run(
        pool.clone(),
        registry.clone(),
        timers.clone(),
        stopped.clone(),
    ));
    let dispatch = tokio::spawn(dispatcher::run(pool, registry.clone(), rx, timers, stopped));
    let join = tokio::spawn(async move {
        let signal = stopping.clone();
        let result = grpc::serve_with_listener(state, listener, &cert, &key, async move {
            let _ = stop.await;
            let _ = signal.send(true);
        })
        .await;
        let _ = stopping.send(true);
        outbox.await.expect("test outbox task");
        dispatch.await.expect("test dispatcher task");
        result
    });
    TestHub {
        #[cfg(feature = "fault-injection")]
        faults,
        addr,
        pki,
        installation,
        registry,
        dispatcher,
        shutdown,
        join,
    }
}

pub mod tls {
    use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair};

    #[derive(Clone)]
    pub struct TestPki {
        pub ca_pem: String,
        pub server_cert_pem: String,
        pub server_key_pem: String,
    }

    pub fn test_pki() -> TestPki {
        let mut params = CertificateParams::new(Vec::new()).expect("CA parameters");
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca = CertifiedIssuer::self_signed(params, KeyPair::generate().expect("CA key"))
            .expect("CA certificate");
        let key = KeyPair::generate().expect("server key");
        let cert = CertificateParams::new(vec!["localhost".into()])
            .expect("server parameters")
            .signed_by(&key, &ca)
            .expect("server certificate");
        TestPki {
            ca_pem: ca.pem(),
            server_cert_pem: cert.pem(),
            server_key_pem: key.serialize_pem(),
        }
    }
}

pub struct Seed {
    pub owner_id: Uuid,
    pub machine_id: Uuid,
    pub conversation_id: Uuid,
}

impl Seed {
    pub fn credential_digest(&self) -> Vec<u8> {
        use sha2::{Digest, Sha256};
        Sha256::digest(self.machine_id.as_bytes()).to_vec()
    }
}

pub async fn seed(pool: &PgPool) -> Seed {
    let owner_id = Uuid::now_v7();
    let machine_id = Uuid::now_v7();
    sqlx::query("insert into owners (id) values ($1)")
        .bind(owner_id)
        .execute(pool)
        .await
        .expect("seed owner");
    sqlx::query("insert into machines (id, owner_id, name) values ($1, $2, 'synthetic')")
        .bind(machine_id)
        .bind(owner_id)
        .execute(pool)
        .await
        .expect("seed machine");
    use sha2::{Digest, Sha256};
    sqlx::query("insert into machine_credentials (machine_id, token_sha256) values ($1, $2)")
        .bind(machine_id)
        .bind(Sha256::digest(machine_id.as_bytes()).as_slice())
        .execute(pool)
        .await
        .expect("seed credential");
    let conversation_id = seed_conversation(pool, owner_id, machine_id).await;
    Seed {
        owner_id,
        machine_id,
        conversation_id,
    }
}

pub async fn seed_conversation(pool: &PgPool, owner_id: Uuid, machine_id: Uuid) -> Uuid {
    let conversation_id = Uuid::now_v7();
    let mut transaction = pool.begin().await.expect("begin seed conversation");
    sqlx::query(
        "insert into conversations (id, owner_id, machine_id, harness, workdir)
         values ($1, $2, $3, 'synthetic', '/tmp/w')",
    )
    .bind(conversation_id)
    .bind(owner_id)
    .bind(machine_id)
    .execute(&mut *transaction)
    .await
    .expect("seed conversation");
    sqlx::query("insert into queue_state (conversation_id, hold_reasons) values ($1, $2)")
        .bind(conversation_id)
        .bind(Json([HoldReason::NoLiveExecution]))
        .execute(&mut *transaction)
        .await
        .expect("seed queue state");
    transaction
        .commit()
        .await
        .expect("commit seed conversation");
    conversation_id
}
