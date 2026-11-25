//! Раннеры на потоке управления: возможность `runners_v1`, допуск несущей, токен раннера,
//! наблюдения и список раннеров. Настоящий gRPC-сервер хаба и клиент воркера.

use std::{net::SocketAddr, time::Duration};

use canterlot_hub::{
    config::HubTimers,
    grpc::{self, HubGrpc, PROTOCOL_VERSION},
    installation::{self, Installation},
    machines::{self, NewMachine, RunnerSpec},
    session::Registry,
    testkit::tls::{TestPki, test_pki},
};
use canterlot_proto::{limits::MAX_MESSAGE_BYTES, worker::v1::*};
use sqlx::PgPool;
use tokio::{net::TcpListener, sync::mpsc, task::JoinHandle, time::timeout};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{
    Request, Streaming,
    transport::{Certificate, Channel, ClientTlsConfig, Endpoint},
};
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(15);

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

struct Host {
    address: SocketAddr,
    pki: TestPki,
    installation: Installation,
    pool: PgPool,
    task: JoinHandle<Result<(), tonic::transport::Error>>,
    carrier: NewMachine,
    runner: NewMachine,
    plain: NewMachine,
}

impl Host {
    async fn start(pool: PgPool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let installation = installation::init(&pool, &dir.path().join("manifest"))
            .await
            .unwrap();
        let owner = installation.owner_id;
        let carrier = machines::add(&pool, owner, "carrier").await.unwrap();
        machines::set_carrier(&pool, carrier.machine_id, true)
            .await
            .unwrap();
        let runner = machines::add_runner(
            &pool,
            owner,
            "runner-a",
            &RunnerSpec {
                parent: carrier.machine_id,
                root_dir: "/srv/runner".into(),
                rank: 20,
                labels: vec!["build-host".into()],
            },
        )
        .await
        .unwrap();
        let plain = machines::add(&pool, owner, "plain").await.unwrap();
        let timers = HubTimers {
            handshake: Duration::from_secs(5),
            dispatch_sweep: Duration::from_millis(100),
            ..HubTimers::default()
        };
        let registry = Registry::new(timers.clone());
        let state = HubGrpc::new(pool.clone(), registry, installation, timers);
        let pki = test_pki();
        let (cert, key) = (pki.server_cert_pem.clone(), pki.server_key_pem.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            grpc::serve_with_listener(state, listener, &cert, &key, std::future::pending()).await
        });
        Self {
            address,
            pki,
            installation,
            pool,
            task,
            carrier,
            runner,
            plain,
        }
    }

    async fn channel(&self) -> Channel {
        Endpoint::from_shared(format!("https://{}", self.address))
            .unwrap()
            .tls_config(
                ClientTlsConfig::new()
                    .ca_certificate(Certificate::from_pem(&self.pki.ca_pem))
                    .domain_name("localhost"),
            )
            .unwrap()
            .connect_timeout(WAIT)
            .connect()
            .await
            .unwrap()
    }

    /// Подключение воркера машины с токеном: рукопожатие пройдено, поток готов.
    async fn connect(&self, machine: &NewMachine, optional: &[&str]) -> Worker {
        let (tx, rx) = mpsc::channel(8);
        let hello = Hello {
            installation_id: self.installation.installation_id.to_string(),
            machine_id: machine.machine_id.to_string(),
            restore_generation: self.installation.restore_generation.to_string(),
            protocol_version: PROTOCOL_VERSION,
            connection_id: Uuid::now_v7().to_string(),
            cj_journal_id: Uuid::now_v7().to_string(),
            boot_id: Uuid::now_v7().to_string(),
            stream_kind: StreamKind::Control as i32,
            optional_capabilities: optional.iter().map(|c| (*c).to_owned()).collect(),
            ..Hello::default()
        };
        tx.send(ControlUp {
            msg: Some(control_up::Msg::Hello(hello)),
        })
        .await
        .unwrap();
        let mut client = worker_control_client::WorkerControlClient::new(self.channel().await)
            .max_decoding_message_size(MAX_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_MESSAGE_BYTES);
        let mut request = Request::new(ReceiverStream::new(rx));
        let mut credential = format!("Bearer {}", machine.token)
            .parse::<tonic::metadata::MetadataValue<_>>()
            .unwrap();
        credential.set_sensitive(true);
        request.metadata_mut().insert("authorization", credential);
        let mut stream = client.control(request).await.unwrap().into_inner();
        let Some(control_down::Msg::HelloReply(HelloReply {
            result: Some(hello_reply::Result::Accepted(accepted)),
        })) = next(&mut stream).await.unwrap().msg
        else {
            panic!("expected accepted handshake")
        };
        Worker {
            tx,
            stream,
            capabilities: accepted.capabilities,
        }
    }

    /// Каждая таблица базы без значения токена: он живёт только в сообщении.
    async fn assert_nowhere_stored(&self, token: &str) {
        let hits: Vec<(String, i32)> = sqlx::query_as(
            "select table_name::text,
                    (xpath('/row/c/text()', query_to_xml(
                       format('select count(*) as c from %I t where row_to_json(t)::text like %L',
                              table_name, '%' || $1 || '%'), false, true, '')))[1]::text::int
             from information_schema.tables
             where table_schema = 'public' and table_type = 'BASE TABLE'",
        )
        .bind(token)
        .fetch_all(&self.pool)
        .await
        .unwrap();
        assert!(hits.len() > 20, "the scan must cover the schema");
        for (table, count) in hits {
            assert_eq!(count, 0, "the token was stored in {table}");
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Worker {
    tx: mpsc::Sender<ControlUp>,
    stream: Streaming<ControlDown>,
    capabilities: Vec<String>,
}

async fn next(stream: &mut Streaming<ControlDown>) -> Result<ControlDown, tonic::Status> {
    Ok(timeout(WAIT, stream.message())
        .await
        .expect("hub message in time")?
        .expect("stream open"))
}

impl Worker {
    async fn send(&self, msg: control_up::Msg) {
        self.tx.send(ControlUp { msg: Some(msg) }).await.unwrap();
    }

    /// Первое сообщение, которое выбирает `pick`; остальное (опросы лимитов и т. п.) пропускается.
    async fn expect<T>(&mut self, mut pick: impl FnMut(control_down::Msg) -> Option<T>) -> T {
        loop {
            let message = next(&mut self.stream).await.unwrap().msg.expect("message");
            if let Some(found) = pick(message) {
                return found;
            }
        }
    }

    async fn assigned(&mut self) -> Vec<(String, String)> {
        self.expect(|msg| match msg {
            control_down::Msg::RunnersAssigned(list) => Some(
                list.runners
                    .into_iter()
                    .map(|r| (r.name, r.root_dir))
                    .collect(),
            ),
            _ => None,
        })
        .await
    }

    async fn token_reply(&mut self) -> Result<RunnerToken, RunnerTokenRefused> {
        self.expect(|msg| match msg {
            control_down::Msg::RunnerToken(token) => Some(Ok(token)),
            control_down::Msg::RunnerTokenRefused(refused) => Some(Err(refused)),
            _ => None,
        })
        .await
    }

    async fn ask(&mut self, runner: Uuid, reset: bool) -> Result<RunnerToken, RunnerTokenRefused> {
        self.send(control_up::Msg::RunnerTokenRequest(RunnerTokenRequest {
            machine_id: runner.to_string(),
            reset_local_state: reset,
        }))
        .await;
        self.token_reply().await
    }
}

fn now_ms() -> u64 {
    u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap()
}

const RUNNERS: &[&str] = &["runners_v1"];

#[tokio::test]
async fn runners_v1_is_negotiated_only_when_the_worker_asks_for_it() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let host = Host::start(pool).await;
    let with = host.connect(&host.carrier, RUNNERS).await;
    assert!(with.capabilities.iter().any(|c| c == "runners_v1"));
    let without = host.connect(&host.plain, &[]).await;
    assert!(!without.capabilities.iter().any(|c| c == "runners_v1"));
    Ok(())
}

#[tokio::test]
async fn a_carrier_is_told_its_runners_on_connect_and_when_they_change() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let host = Host::start(pool.clone()).await;
    let mut carrier = host.connect(&host.carrier, RUNNERS).await;
    assert_eq!(
        carrier.assigned().await,
        vec![("runner-a".to_owned(), "/srv/runner".to_owned())]
    );
    // Новый раннер: список приходит сам.
    let second = machines::add_runner(
        &pool,
        host.installation.owner_id,
        "runner-b",
        &RunnerSpec {
            parent: host.carrier.machine_id,
            root_dir: "/srv/b".into(),
            rank: 0,
            labels: vec![],
        },
    )
    .await?;
    assert_eq!(
        carrier.assigned().await,
        vec![
            ("runner-a".to_owned(), "/srv/runner".to_owned()),
            ("runner-b".to_owned(), "/srv/b".to_owned())
        ]
    );
    // Отозванный раннер уходит из списка.
    machines::revoke(&pool, second.machine_id).await?;
    assert_eq!(
        carrier.assigned().await,
        vec![("runner-a".to_owned(), "/srv/runner".to_owned())]
    );
    // Снят флаг несущей: список опустошается.
    machines::set_carrier(&pool, host.carrier.machine_id, false).await?;
    assert!(carrier.assigned().await.is_empty());
    Ok(())
}

#[tokio::test]
async fn the_cloud_runner_is_listed_and_gets_its_token_only_while_a_window_serves() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let host = Host::start(pool.clone()).await;
    let mut carrier = host.connect(&host.carrier, RUNNERS).await;
    assert_eq!(
        carrier.assigned().await,
        vec![("runner-a".to_owned(), "/srv/runner".to_owned())]
    );
    // Окно создано, машина найдена (`identified`), раннера `cloud` ещё нет в базе.
    let config: canterlot_hub::config::CloudConfig = toml::from_str(&format!(
        "provider='yandex'\nkey_file='k.json'\nfolder_id='b1gfakefolder000000'\n\
         zone='ru-central1-a'\nsubnet_id='e9bfakesubnet000000'\n\
         runner_public_key='ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEXAMPLEEXAMPLE'\n\
         carrier_machine_id='{}'\n[[profile]]\ncores=2\nmemory_gb=2\ndisk_gb=10\n",
        host.carrier.machine_id
    ))?;
    let window = Uuid::now_v7();
    sqlx::query(
        "insert into cloud_windows (id, name, state, trigger, folder_id, preemptible, instance_id,
                                    host_key_public, price_hourly_kopecks, reservation_kopecks,
                                    month, hard_cap_minutes, created_at, create_called_at, profiles)
         values ($1, 'canterlot-runner-' || $1::text, 'identified', 'owner', 'b1gfakefolder000000',
                 true, 'fhmfakeinstance5001', 'ssh-ed25519 AAAA', 1806, 7224, '2026-11', 60,
                 now(), now(),
                 '[{\"cores\":32,\"memory_gb\":64,\"disk_gb\":80,\"compute_kopecks_per_hour\":1646,\"disk_kopecks_per_hour\":160}]')",
    )
    .bind(window)
    .execute(&pool)
    .await?;
    let machine = {
        let mut conn = pool.acquire().await?;
        canterlot_hub::cloud::windows::ensure_cloud_machine(&mut conn, &config, true).await?
    };
    // Токена ещё нет, а несущая уже получает раннера в списке: ей пора его подключать.
    assert_eq!(
        carrier.assigned().await,
        vec![
            ("cloud".to_owned(), "/srv/canterlot-runner".to_owned()),
            ("runner-a".to_owned(), "/srv/runner".to_owned())
        ]
    );
    // Запрос токена по сети: ротация с оживлением и сбросом состояния.
    sqlx::query("update machines set cj_journal_id = gen_random_uuid() where id = $1")
        .bind(machine)
        .execute(&pool)
        .await?;
    let reply = carrier.ask(machine, false).await.unwrap();
    assert_eq!(reply.machine_id, machine.to_string());
    assert_eq!(machines::verify(&pool, &reply.token).await?, Some(machine));
    let journal: Option<Uuid> =
        sqlx::query_scalar("select cj_journal_id from machines where id = $1")
            .bind(machine)
            .fetch_one(&pool)
            .await?;
    assert_eq!(journal, None);
    host.assert_nowhere_stored(&reply.token).await;

    // Окно закончилось: раннер ушёл из списка, токен не выдаётся.
    sqlx::query(
        "update cloud_windows set state = 'deleting', delete_requested_at = now() where id = $1",
    )
    .bind(window)
    .execute(&pool)
    .await?;
    machines::revoke(&pool, machine).await?;
    assert_eq!(
        carrier.assigned().await,
        vec![("runner-a".to_owned(), "/srv/runner".to_owned())]
    );
    let refused = carrier.ask(machine, true).await.unwrap_err();
    assert_eq!(refused.code, "revoked");
    Ok(())
}

#[tokio::test]
async fn the_token_goes_to_the_carrier_in_one_volatile_message() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let host = Host::start(pool.clone()).await;
    let mut carrier = host.connect(&host.carrier, RUNNERS).await;
    carrier.assigned().await;
    let reply = carrier.ask(host.runner.machine_id, true).await.unwrap();
    assert_eq!(reply.machine_id, host.runner.machine_id.to_string());
    assert_eq!(
        reply.installation_id,
        host.installation.installation_id.to_string()
    );
    assert_eq!(
        reply.restore_generation,
        host.installation.restore_generation.to_string()
    );
    assert_eq!(
        machines::verify(&pool, &reply.token).await?,
        Some(host.runner.machine_id)
    );
    // Прежний токен раннера отозван ротацией.
    assert_eq!(machines::verify(&pool, &host.runner.token).await?, None);
    host.assert_nowhere_stored(&reply.token).await;
    // Потерянное сообщение: несущая просит снова, хаб ротирует ещё раз.
    let again = carrier.ask(host.runner.machine_id, false).await.unwrap();
    assert_ne!(again.token, reply.token);
    assert_eq!(machines::verify(&pool, &reply.token).await?, None);
    assert_eq!(
        machines::verify(&pool, &again.token).await?,
        Some(host.runner.machine_id)
    );
    Ok(())
}

#[tokio::test]
async fn token_requests_are_refused_with_a_code_and_rotate_nothing() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let host = Host::start(pool.clone()).await;
    let other = machines::add(&pool, host.installation.owner_id, "other-carrier").await?;
    machines::set_carrier(&pool, other.machine_id, true).await?;
    let revoked = machines::add_runner(
        &pool,
        host.installation.owner_id,
        "runner-gone",
        &RunnerSpec {
            parent: host.carrier.machine_id,
            root_dir: "/srv/gone".into(),
            rank: 0,
            labels: vec![],
        },
    )
    .await?;
    machines::revoke(&pool, revoked.machine_id).await?;

    let mut carrier = host.connect(&host.carrier, RUNNERS).await;
    carrier.assigned().await;
    assert_eq!(
        carrier.ask(Uuid::now_v7(), false).await.unwrap_err().code,
        "unknown_machine"
    );
    assert_eq!(
        carrier
            .ask(host.plain.machine_id, false)
            .await
            .unwrap_err()
            .code,
        "unknown_machine"
    );
    assert_eq!(
        carrier
            .ask(revoked.machine_id, false)
            .await
            .unwrap_err()
            .code,
        "revoked"
    );
    let mut stranger = host.connect(&other, RUNNERS).await;
    stranger.assigned().await;
    assert_eq!(
        stranger
            .ask(host.runner.machine_id, false)
            .await
            .unwrap_err()
            .code,
        "not_parent"
    );
    // Раннер сам не несущая: даже со своим токеном и возможностью он не получит токен.
    let mut runner = host.connect(&host.runner, RUNNERS).await;
    assert_eq!(
        runner
            .ask(host.runner.machine_id, false)
            .await
            .unwrap_err()
            .code,
        "not_carrier"
    );
    let mut plain = host.connect(&host.plain, RUNNERS).await;
    assert_eq!(
        plain
            .ask(host.runner.machine_id, false)
            .await
            .unwrap_err()
            .code,
        "not_carrier"
    );
    // Ничего не ротировано: токен раннера действует.
    assert_eq!(
        machines::verify(&pool, &host.runner.token).await?,
        Some(host.runner.machine_id)
    );
    // Мусорный идентификатор — тоже отказ `unknown_machine`, а не обрыв потока.
    assert_eq!(
        carrier.ask_raw("not-a-uuid").await.unwrap_err().code,
        "unknown_machine"
    );
    Ok(())
}

impl Worker {
    async fn ask_raw(&mut self, machine_id: &str) -> Result<RunnerToken, RunnerTokenRefused> {
        self.send(control_up::Msg::RunnerTokenRequest(RunnerTokenRequest {
            machine_id: machine_id.to_owned(),
            reset_local_state: false,
        }))
        .await;
        self.token_reply().await
    }
}

#[tokio::test]
async fn runner_messages_without_the_capability_end_the_stream() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let host = Host::start(pool).await;
    // Несущая по флагу, но воркер возможность не объявил: запрос не принимается.
    let mut carrier = host.connect(&host.carrier, &[]).await;
    carrier
        .send(control_up::Msg::RunnerTokenRequest(RunnerTokenRequest {
            machine_id: host.runner.machine_id.to_string(),
            reset_local_state: false,
        }))
        .await;
    let status = loop {
        match timeout(WAIT, carrier.stream.message()).await.unwrap() {
            Err(status) => break status,
            Ok(Some(down)) => {
                assert!(
                    !matches!(
                        down.msg,
                        Some(
                            control_down::Msg::RunnerToken(_)
                                | control_down::Msg::RunnersAssigned(_)
                        )
                    ),
                    "nothing runner-related without the capability"
                );
            }
            Ok(None) => panic!("the stream must fail, not end quietly"),
        }
    };
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
    assert_eq!(status.message(), "capability_missing");
    assert_eq!(
        machines::verify(&host.pool, &host.runner.token).await?,
        Some(host.runner.machine_id)
    );
    Ok(())
}

#[tokio::test]
async fn observations_are_stored_only_from_the_carrier() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let host = Host::start(pool.clone()).await;
    let observation = |machine: Uuid, reachable: bool, failures: u32| {
        control_up::Msg::RunnerObservation(RunnerObservation {
            machine_id: machine.to_string(),
            reachable,
            consecutive_failures: failures,
            observed_at: now_ms(),
        })
    };
    // Не несущая шлёт наблюдение: оно отброшено, поток жив (следующий запрос отвечает).
    let mut plain = host.connect(&host.plain, RUNNERS).await;
    plain
        .send(observation(host.runner.machine_id, false, 9))
        .await;
    assert_eq!(
        plain
            .ask(host.runner.machine_id, false)
            .await
            .unwrap_err()
            .code,
        "not_carrier"
    );
    // Раннер о самом себе тоже не может.
    let mut runner = host.connect(&host.runner, RUNNERS).await;
    runner
        .send(observation(host.runner.machine_id, false, 9))
        .await;
    assert_eq!(
        runner
            .ask(host.runner.machine_id, false)
            .await
            .unwrap_err()
            .code,
        "not_carrier"
    );
    let stored: i64 = sqlx::query_scalar("select count(*) from runner_observations")
        .fetch_one(&pool)
        .await?;
    assert_eq!(stored, 0, "refused observations are not stored");

    let mut carrier = host.connect(&host.carrier, RUNNERS).await;
    carrier.assigned().await;
    carrier
        .send(observation(host.runner.machine_id, false, 3))
        .await;
    // Ответа нет: ждём, пока строка появится.
    timeout(WAIT, async {
        loop {
            let row: Option<(bool, i32, Uuid)> = sqlx::query_as(
                "select reachable, consecutive_failures, carrier_id from runner_observations
                 where machine_id = $1",
            )
            .bind(host.runner.machine_id)
            .fetch_optional(&pool)
            .await
            .unwrap();
            if let Some(row) = row {
                assert_eq!(row, (false, 3, host.carrier.machine_id));
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await?;
    Ok(())
}

fn owner_spec() -> String {
    serde_json::json!({
        "shell": "make gate",
        "source": {"repo": "/srv/src/app", "rev": "0123456789abcdef0123456789abcdef01234567"},
        "outputs": ["result"],
    })
    .to_string()
}

impl Worker {
    async fn submit(&mut self, request_id: &str, spec: String) -> Result<String, (String, String)> {
        self.send(control_up::Msg::OwnerJobSubmit(OwnerJobSubmit {
            request_id: request_id.to_owned(),
            spec_json: spec,
        }))
        .await;
        let request_id = request_id.to_owned();
        self.expect(|msg| match msg {
            control_down::Msg::OwnerJobAccepted(done) => {
                assert_eq!(done.request_id, request_id);
                Some(Ok(done.job_id))
            }
            control_down::Msg::OwnerJobRefused(refused) => {
                assert_eq!(refused.request_id, request_id);
                Some(Err((refused.code, refused.message)))
            }
            _ => None,
        })
        .await
    }
}

#[tokio::test]
async fn an_owner_job_goes_over_the_control_stream_and_is_answered_by_request() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let host = Host::start(pool.clone()).await;
    sqlx::query("update machines set capabilities=array['jobs_inplace_v1'] where id=$1")
        .bind(host.carrier.machine_id)
        .execute(&pool)
        .await?;
    let mut carrier = host.connect(&host.carrier, RUNNERS).await;
    carrier.assigned().await;
    let job_id = carrier.submit("req-1", owner_spec()).await.unwrap();
    let row: (bool, Option<Uuid>, Uuid, String) = sqlx::query_as(
        "select owner_job, conversation_id, carrier_id, shell from jobs where id=$1::uuid",
    )
    .bind(&job_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        row,
        (true, None, host.carrier.machine_id, "make gate".to_owned())
    );
    // Отказ приходит с кодом и тем же `request_id`, строки нет.
    let (code, message) = carrier
        .submit(
            "req-2",
            serde_json::json!({"shell": "x", "source": {"repo": "/srv/src/app", "rev": "abc"}})
                .to_string(),
        )
        .await
        .unwrap_err();
    assert_eq!(code, "invalid_arguments");
    assert!(!message.is_empty());
    let jobs: i64 = sqlx::query_scalar("select count(*) from jobs")
        .fetch_one(&pool)
        .await?;
    assert_eq!(jobs, 1);
    Ok(())
}

#[tokio::test]
async fn only_a_carrier_with_the_capability_may_submit_owner_jobs() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let host = Host::start(pool.clone()).await;
    // Обычная машина с возможностью: отказ кодом `not_carrier`, поток остаётся.
    let mut plain = host.connect(&host.plain, RUNNERS).await;
    let (code, _) = plain.submit("req-1", owner_spec()).await.unwrap_err();
    assert_eq!(code, "not_carrier");
    // Несущая без возможности: поток закрывается `capability_missing`.
    let mut carrier = host.connect(&host.carrier, &[]).await;
    carrier
        .send(control_up::Msg::OwnerJobSubmit(OwnerJobSubmit {
            request_id: "req-2".into(),
            spec_json: owner_spec(),
        }))
        .await;
    let status = loop {
        match timeout(WAIT, carrier.stream.message()).await.unwrap() {
            Err(status) => break status,
            Ok(Some(down)) => assert!(!matches!(
                down.msg,
                Some(control_down::Msg::OwnerJobAccepted(_))
            )),
            Ok(None) => panic!("the stream must fail, not end quietly"),
        }
    };
    assert_eq!(status.message(), "capability_missing");
    let jobs: i64 = sqlx::query_scalar("select count(*) from jobs")
        .fetch_one(&pool)
        .await?;
    assert_eq!(jobs, 0);
    Ok(())
}

/// Наблюдение, сменившее доступность раннера, сразу даёт клиентам событие со словарём раннера
/// и временем наблюдения, не дожидаясь обхода проектора.
#[tokio::test]
async fn an_observation_publishes_the_runners_availability_at_once() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let host = Host::start(pool.clone()).await;
    let mut carrier = host.connect(&host.carrier, RUNNERS).await;
    carrier.assigned().await;
    carrier
        .send(control_up::Msg::RunnerObservation(RunnerObservation {
            machine_id: host.runner.machine_id.to_string(),
            reachable: false,
            consecutive_failures: 3,
            observed_at: now_ms(),
        }))
        .await;
    let runner = host.runner.machine_id.to_string();
    let payload = timeout(WAIT, async {
        loop {
            let found: Option<serde_json::Value> = sqlx::query_scalar(
                "select payload from events
                 where kind='machine_availability_changed' and payload->>'machine_id'=$1
                 order by scope_seq desc limit 1",
            )
            .bind(&runner)
            .fetch_optional(&pool)
            .await
            .unwrap();
            if let Some(found) = found {
                break found;
            }
            tokio::task::yield_now().await;
        }
    })
    .await?;
    assert_eq!(payload["availability"], "offline", "{payload}");
    assert!(payload["observed_at"].is_string(), "{payload}");
    Ok(())
}
