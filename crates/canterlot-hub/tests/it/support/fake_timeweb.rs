//! Поддельный Timeweb Cloud для тестов клиента и жизненного цикла облачных окон.
//!
//! Формы ответов взяты из публичного OpenAPI (`servers`, `ssh-keys`, `configurator/servers`,
//! `projects/{id}/resources/servers`). Ручки сбоев лежат в `FakeState` и меняются прямо в
//! тесте через `fake.control()`: обрыв соединения после принятия создания, зависание, 5xx,
//! 429 с `Retry-After`, страницы списка с отказом на одной из них, удаление с хешем или 423,
//! карантин, серверы, видимые в списке только через задержку.
//!
//! Сервер поднимает свой цикл приёма соединений: так обрыв «принял и закрыл сокет» делается
//! по-настоящему, а не ответом с кодом.

#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

use axum::Extension;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::Request;
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::Response;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::service::TowerToHyperService;
use serde_json::{Value, json};
use tokio::sync::Notify;

/// Что делает создание сервера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateMode {
    /// 201 с описанием сервера.
    Ok,
    /// Сервер принят и записан, соединение закрыто без ответа.
    AcceptThenDrop,
    /// Сервер принят и записан, ответа нет вовсе: клиент упирается в таймаут.
    AcceptThenHang,
    /// Ответ с кодом сбоя, сервер не записан.
    Status(u16),
    /// Определённый отказ с сообщением.
    Reject { status: u16, message: String },
    /// Определённый отказ с заданным `error_code` тела (`no_free_node`,
    /// `configuration_requires_sales_request`, …).
    Coded { status: u16, error_code: String },
}

/// Что делает удаление сервера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteMode {
    /// 200 с `server_delete`, сервер убран.
    Ok,
    /// 204 без тела, сервер убран.
    NoContent,
    /// 200 с `is_moved_in_quarantine: true`, сервер убран из списков.
    Quarantine,
    /// То же 200 с `is_moved_in_quarantine: true`, но сервер ещё виден в списках проекта: о
    /// конце оплаты говорит только его отсутствие в полном списке.
    QuarantineListed,
    /// 200 с `hash`: нужен код подтверждения, сервер остаётся.
    WithHash,
    /// 423: удаление по SMS, сервер остаётся.
    Locked,
    /// Ответ с кодом сбоя, сервер остаётся.
    Status(u16),
}

/// Отказ одной страницы списка.
#[derive(Debug, Clone)]
pub struct FailPage {
    pub offset: usize,
    pub status: u16,
    /// Сколько раз отказать; `None` — каждый раз.
    pub times: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct FakeServer {
    pub id: i64,
    pub name: String,
    pub comment: String,
    pub project_id: i64,
    pub configurator_id: i64,
    pub cpu: u64,
    pub ram_mb: u64,
    pub disk_mb: u64,
    /// Сколько ещё запросов `GET /servers/{id}` ответят `installing`, прежде чем сервер
    /// станет `on` с адресом.
    pub boot_polls_left: u32,
    /// Сколько ещё листингов проекта (первых страниц) не покажут сервер.
    pub hidden_polls_left: u32,
    /// Публичный IPv4. Как у настоящего Timeweb, у нового сервера его нет, пока хаб не
    /// запросит (`POST /servers/{id}/ips`); `auto_ipv4` возвращает прежнее поведение.
    pub ipv4: Option<String>,
    pub request: Value,
}

/// Плавающий адрес аккаунта. После удаления сервера адрес остаётся без привязки.
#[derive(Debug, Clone)]
pub struct FakeFloatingIp {
    pub id: String,
    pub ip: String,
    pub resource_id: Option<i64>,
}

/// Что делает запрос IPv4 для сервера.
#[derive(Debug, Clone, PartialEq)]
pub enum IpAddMode {
    /// 201 с `server_ip`, адрес выдан.
    Ok,
    /// Ответ с кодом сбоя, адрес не выдан.
    Status(u16),
    /// Адрес выдан и записан, соединение закрыто без ответа.
    AcceptThenDrop,
    /// Определённый отказ с `error_code` и, если задан, `details.required_balance` (так
    /// Timeweb отвечает правилом против злоупотреблений: 400 `abuse_prepayment_required`).
    Refuse {
        status: u16,
        error_code: String,
        required_balance: Option<f64>,
    },
}

#[derive(Debug, Clone)]
pub struct FakeConfigurator {
    pub id: i64,
    pub location: String,
    pub cpu_frequency: String,
    /// Рубли в месяц за vCPU, за ГБ памяти, за ГБ диска.
    pub price_cpu: f64,
    pub price_ram: f64,
    pub price_disk: f64,
}

#[derive(Debug, Clone)]
pub struct FakeSshKey {
    pub id: i64,
    pub name: String,
    pub body: String,
}

#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    pub query: HashMap<String, String>,
    pub body: Option<Value>,
    pub authorization: Option<String>,
}

pub struct FakeState {
    pub expected_token: String,
    pub configurators: Vec<FakeConfigurator>,
    pub ssh_keys: Vec<FakeSshKey>,
    pub servers: BTreeMap<i64, FakeServer>,
    pub next_server_id: i64,
    pub next_key_id: i64,
    pub create: CreateMode,
    /// Отказы по конфигуратору: если для `configuration.configurator_id` запроса здесь есть
    /// запись, она заменяет `create` (годятся `Status`, `Reject`, `Coded`).
    pub create_by_configurator: BTreeMap<i64, CreateMode>,
    pub delete: DeleteMode,
    /// Новый сервер сразу получает публичный IPv4 (так было до живой проверки 22.11);
    /// `false` — как у настоящего Timeweb: адрес появляется только после `POST .../ips`.
    pub auto_ipv4: bool,
    pub ip_add: IpAddMode,
    /// Плавающие адреса аккаунта; выданный серверу адрес попадает сюда привязанным.
    pub floating_ips: Vec<FakeFloatingIp>,
    pub next_ip_host: u8,
    /// Код сбоя для `DELETE /floating-ips/{id}`; `None` — удаляет.
    pub floating_delete_status: Option<u16>,
    /// Коды, которыми отвечают подряд на любые следующие запросы (очередь).
    pub fail_next: VecDeque<u16>,
    /// Сколько следующих запросов отвечать 429.
    pub rate_limit_next: u32,
    /// Значение `Retry-After` в ответах 429.
    pub retry_after: Option<String>,
    /// Не больше стольких записей на странице списка, как бы ни просили.
    pub list_page_cap: Option<usize>,
    pub list_fail_page: Option<FailPage>,
    /// Игнорировать `limit` и `offset`: вся выборка одним ответом (как у ручки проекта).
    pub list_ignores_paging: bool,
    /// Подменить `meta.total`.
    pub list_total_override: Option<u64>,
    /// Сервер, созданный запросом, не виден в первых стольких листингах проекта. Счёт
    /// идёт по листингам, а не по часам: тест не зависит от скорости машины.
    pub listing_hidden_polls: u32,
    /// Сервер, созданный запросом, отвечает `installing` на первые столько запросов
    /// `GET /servers/{id}`, потом становится `on` с адресом.
    pub boot_polls: u32,
    /// Подмешивать в тексты ошибок заголовок Authorization, как делает неряшливый сервер.
    pub echo_authorization_in_errors: bool,
    pub requests: Vec<RecordedRequest>,
}

impl FakeState {
    fn new(token: &str) -> Self {
        Self {
            expected_token: token.to_owned(),
            configurators: vec![
                FakeConfigurator {
                    id: 151,
                    location: "ru-3".into(),
                    cpu_frequency: "3.0".into(),
                    price_cpu: 210.0,
                    price_ram: 200.0,
                    price_disk: 12.0,
                },
                FakeConfigurator {
                    id: 133,
                    location: "ru-1".into(),
                    cpu_frequency: "5.5".into(),
                    price_cpu: 1800.0,
                    price_ram: 700.0,
                    price_disk: 30.0,
                },
            ],
            ssh_keys: Vec::new(),
            servers: BTreeMap::new(),
            next_server_id: 5001,
            next_key_id: 71,
            create: CreateMode::Ok,
            create_by_configurator: BTreeMap::new(),
            delete: DeleteMode::Ok,
            auto_ipv4: true,
            ip_add: IpAddMode::Ok,
            floating_ips: Vec::new(),
            next_ip_host: 200,
            floating_delete_status: None,
            fail_next: VecDeque::new(),
            rate_limit_next: 0,
            retry_after: None,
            list_page_cap: None,
            list_fail_page: None,
            list_ignores_paging: false,
            list_total_override: None,
            listing_hidden_polls: 0,
            boot_polls: 0,
            echo_authorization_in_errors: false,
            requests: Vec::new(),
        }
    }

    /// Положить сервер напрямую, минуя запрос создания (для списков и сверки).
    pub fn add_server(&mut self, project_id: i64, name: &str) -> i64 {
        let id = self.next_server_id;
        self.next_server_id += 1;
        self.servers.insert(
            id,
            FakeServer {
                id,
                name: name.to_owned(),
                comment: String::new(),
                project_id,
                configurator_id: 151,
                cpu: 1,
                ram_mb: 1024,
                disk_mb: 10240,
                boot_polls_left: 0,
                hidden_polls_left: 0,
                ipv4: None,
                request: Value::Null,
            },
        );
        id
    }

    /// Убрать сервер, как это делает удаление: его адрес остаётся плавающим без привязки.
    pub fn remove_server(&mut self, id: i64) {
        self.servers.remove(&id);
        unbind_ips(self, id);
    }

    pub fn count(&self, method: &str, path_prefix: &str) -> usize {
        self.requests
            .iter()
            .filter(|r| r.method == method && r.path.starts_with(path_prefix))
            .count()
    }
}

pub struct FakeTimeweb {
    pub base_url: String,
    pub token: String,
    state: Arc<Mutex<FakeState>>,
    stop: Arc<Notify>,
}

impl Drop for FakeTimeweb {
    fn drop(&mut self) {
        self.stop.notify_waiters();
        self.stop.notify_one();
    }
}

/// Сигнал «закрой это соединение»: у каждого соединения свой.
#[derive(Clone)]
struct Kill(Arc<Notify>);

enum Action {
    Respond(Response),
    /// Закрыть соединение, не ответив.
    Drop,
    /// Никогда не отвечать.
    Hang,
}

impl FakeTimeweb {
    pub async fn start(token: &str) -> Self {
        let state = Arc::new(Mutex::new(FakeState::new(token)));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let stop = Arc::new(Notify::new());

        let app = {
            let state = state.clone();
            Router::new().fallback(move |Extension(kill): Extension<Kill>, request: Request| {
                let state = state.clone();
                async move { dispatch(state, kill, request).await }
            })
        };

        let stopper = stop.clone();
        tokio::spawn(async move {
            loop {
                let stream = tokio::select! {
                    accepted = listener.accept() => match accepted {
                        Ok((stream, _)) => stream,
                        Err(_) => continue,
                    },
                    () = stopper.notified() => return,
                };
                let kill = Kill(Arc::new(Notify::new()));
                let service = TowerToHyperService::new(app.clone().layer(Extension(kill.clone())));
                let stopper = stopper.clone();
                tokio::spawn(async move {
                    let builder =
                        hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
                    let connection = builder.serve_connection(TokioIo::new(stream), service);
                    tokio::select! {
                        _ = connection => {}
                        () = kill.0.notified() => {}
                        () = stopper.notified() => {}
                    }
                });
            }
        });

        Self {
            base_url: format!("http://{addr}"),
            token: token.to_owned(),
            state,
            stop,
        }
    }

    /// Ручки сбоев и содержимое поддельного облака.
    pub fn control(&self) -> MutexGuard<'_, FakeState> {
        self.state.lock().unwrap()
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.control().requests.clone()
    }

    pub fn server_count(&self) -> usize {
        self.control().servers.len()
    }
}

fn error_body(status: u16, message: &str) -> Value {
    json!({
        "status_code": status,
        "error_code": match status {
            400 => "bad_request",
            401 => "unauthorized",
            403 => "forbidden",
            404 => "not_found",
            409 => "conflict",
            423 => "locked",
            429 => "too_many_requests",
            _ => "internal_server_error",
        },
        "message": message,
        "response_id": "15095f25-aac3-4d60-a788-96cb5136f186",
    })
}

fn json_response(status: u16, body: &Value) -> Response {
    Response::builder()
        .status(StatusCode::from_u16(status).unwrap())
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn empty_response(status: u16) -> Response {
    Response::builder()
        .status(StatusCode::from_u16(status).unwrap())
        .body(Body::empty())
        .unwrap()
}

fn error_response(state: &FakeState, status: u16, message: &str, auth: Option<&str>) -> Response {
    let message = match (state.echo_authorization_in_errors, auth) {
        (true, Some(auth)) => format!("{message} (authorization: {auth})"),
        _ => message.to_owned(),
    };
    json_response(status, &error_body(status, &message))
}

fn server_json(server: &FakeServer) -> Value {
    let booted = server.boot_polls_left == 0;
    let ips = match (&server.ipv4, booted) {
        (Some(ip), true) => json!([{"type": "ipv4", "ip": ip, "is_main": true}]),
        _ => json!([]),
    };
    json!({
        "id": server.id,
        "name": server.name,
        "comment": server.comment,
        "os": {"id": 109, "name": "debian", "version": "13"},
        "software": null,
        "preset_id": null,
        "configurator_id": server.configurator_id,
        "created_at": "2026-11-21T10:00:00.000Z",
        "location": "ru-3",
        "boot_mode": "std",
        "status": if booted { "on" } else { "installing" },
        "start_at": null,
        "is_ddos_guard": false,
        "cpu": server.cpu,
        "cpu_frequency": "3.0",
        "ram": server.ram_mb,
        "disks": [{"id": 1, "size": server.disk_mb, "used": 0, "type": "nvme", "is_mounted": true, "is_system": true, "status": "started"}],
        "avatar_id": null,
        "avatar_link": null,
        "vnc_pass": "x",
        "root_pass": null,
        "image": null,
        "networks": [{"type": "public", "ips": ips}],
        "cloud_init": server.request["cloud_init"],
        "is_qemu_agent": true,
        "availability_zone": "msk-1",
        "is_master_ssh": false,
        "is_dedicated_cpu": false,
        "gpu": 0,
    })
}

fn parse_query(raw: Option<&str>) -> HashMap<String, String> {
    raw.unwrap_or_default()
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (key.to_owned(), value.to_owned())
        })
        .collect()
}

async fn dispatch(state: Arc<Mutex<FakeState>>, kill: Kill, request: Request) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let query = parse_query(request.uri().query());
    let auth = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let bytes = to_bytes(request.into_body(), 8 << 20)
        .await
        .unwrap_or_default();
    let body: Option<Value> = serde_json::from_slice(&bytes).ok();

    let action = {
        let mut st = state.lock().unwrap();
        st.requests.push(RecordedRequest {
            method: method.to_string(),
            path: path.clone(),
            query: query.clone(),
            body: body.clone(),
            authorization: auth.clone(),
        });
        route(&mut st, &method, &path, &query, body, auth.as_deref())
    };

    match action {
        Action::Respond(response) => response,
        Action::Drop => {
            kill.0.notify_one();
            std::future::pending().await
        }
        Action::Hang => std::future::pending().await,
    }
}

fn route(
    st: &mut FakeState,
    method: &Method,
    path: &str,
    query: &HashMap<String, String>,
    body: Option<Value>,
    auth: Option<&str>,
) -> Action {
    if auth != Some(format!("Bearer {}", st.expected_token).as_str()) {
        return Action::Respond(error_response(st, 401, "Unauthorized", auth));
    }
    if st.rate_limit_next > 0 {
        st.rate_limit_next -= 1;
        let mut response = error_response(st, 429, "Too many requests", auth);
        if let Some(after) = &st.retry_after {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_str(after).unwrap());
        }
        return Action::Respond(response);
    }
    if let Some(status) = st.fail_next.pop_front() {
        return Action::Respond(error_response(st, status, "Injected failure", auth));
    }

    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    match (method.as_str(), segments.as_slice()) {
        ("GET", ["api", "v1", "configurator", "servers"]) => {
            let items: Vec<Value> = st
                .configurators
                .iter()
                .map(|c| {
                    json!({
                        "id": c.id,
                        "location": c.location,
                        "disk_type": "nvme",
                        "is_allowed_local_network": true,
                        "cpu_frequency": c.cpu_frequency,
                        "prices": {"cpu": c.price_cpu, "ram": c.price_ram, "disk": c.price_disk, "bandwidth": null},
                        "requirements": {
                            "cpu_min": 1, "cpu_step": 1, "cpu_max": 104,
                            "ram_min": 1024, "ram_step": 1024, "ram_max": 786_432,
                            "disk_min": 10_240, "disk_step": 1024, "disk_max": 1_048_576,
                            "network_bandwidth_min": 200, "network_bandwidth_step": 100, "network_bandwidth_max": 1000,
                            "gpu_min": null, "gpu_max": null, "gpu_step": null,
                        },
                    })
                })
                .collect();
            let total = items.len();
            Action::Respond(json_response(
                200,
                &json!({"server_configurators": items, "meta": {"total": total}, "response_id": "r"}),
            ))
        }
        ("GET", ["api", "v1", "ssh-keys"]) => {
            let all: Vec<Value> = st
                .ssh_keys
                .iter()
                .map(|k| {
                    json!({
                        "id": k.id, "name": k.name, "body": k.body,
                        "created_at": "2026-11-21T10:00:00.000Z", "used_by": [], "is_default": false,
                    })
                })
                .collect();
            page_response(st, query, all, "ssh-keys")
        }
        ("POST", ["api", "v1", "ssh-keys"]) => {
            let Some(body) = body else {
                return Action::Respond(error_response(st, 400, "Bad body", auth));
            };
            let name = body["name"].as_str().unwrap_or_default().to_owned();
            let key = body["body"].as_str().unwrap_or_default().to_owned();
            let id = st.next_key_id;
            st.next_key_id += 1;
            st.ssh_keys.push(FakeSshKey {
                id,
                name: name.clone(),
                body: key.clone(),
            });
            Action::Respond(json_response(
                201,
                &json!({"ssh-key": {
                    "id": id, "name": name, "body": key,
                    "created_at": "2026-11-21T10:00:00.000Z", "used_by": [], "is_default": false,
                }, "response_id": "r"}),
            ))
        }
        ("POST", ["api", "v1", "servers"]) => create_server(st, body, auth),
        ("GET", ["api", "v1", "servers", id]) => {
            let found = id
                .parse::<i64>()
                .ok()
                .and_then(|id| st.servers.get_mut(&id));
            match found {
                Some(server) => {
                    let rendered = server_json(server);
                    server.boot_polls_left = server.boot_polls_left.saturating_sub(1);
                    Action::Respond(json_response(
                        200,
                        &json!({"server": rendered, "response_id": "r"}),
                    ))
                }
                None => Action::Respond(error_response(st, 404, "Resource not found", auth)),
            }
        }
        ("DELETE", ["api", "v1", "servers", id]) => delete_server(st, id, auth),
        ("POST", ["api", "v1", "servers", id, "ips"]) => add_ip(st, id, body, auth),
        ("GET", ["api", "v1", "floating-ips"]) => {
            let items: Vec<Value> = st
                .floating_ips
                .iter()
                .map(|fip| {
                    json!({
                        "id": fip.id,
                        "ip": fip.ip,
                        "resource_type": fip.resource_id.map(|_| "server"),
                        "resource_id": fip.resource_id,
                        "is_ddos_guard": false,
                    })
                })
                .collect();
            page_response(st, query, items, "ips")
        }
        ("DELETE", ["api", "v1", "floating-ips", id]) => {
            if let Some(status) = st.floating_delete_status {
                return Action::Respond(error_response(st, status, "Injected IP failure", auth));
            }
            let Some(at) = st.floating_ips.iter().position(|fip| fip.id == *id) else {
                return Action::Respond(error_response(st, 404, "Resource not found", auth));
            };
            if st.floating_ips[at].resource_id.is_some() {
                return Action::Respond(error_response(st, 409, "IP is bound to a resource", auth));
            }
            st.floating_ips.remove(at);
            Action::Respond(empty_response(204))
        }
        ("GET", ["api", "v1", "projects", project, "resources", "servers"]) => {
            let Ok(project_id) = project.parse::<i64>() else {
                return Action::Respond(error_response(st, 400, "Bad project", auth));
            };
            // Скрытый сервер «стареет» по листингам: счётчик убывает на первой странице.
            let first_page = query.get("offset").is_none_or(|offset| offset == "0");
            let mut visible: Vec<Value> = Vec::new();
            for server in st
                .servers
                .values_mut()
                .filter(|s| s.project_id == project_id)
            {
                if server.hidden_polls_left > 0 {
                    if first_page {
                        server.hidden_polls_left -= 1;
                    }
                    continue;
                }
                visible.push(server_json(server));
            }
            page_response(st, query, visible, "servers")
        }
        _ => Action::Respond(error_response(st, 404, "Resource not found", auth)),
    }
}

/// Страница списка с `limit`/`offset`, потолком страницы, отказом и подменой `total`.
fn page_response(
    st: &mut FakeState,
    query: &HashMap<String, String>,
    all: Vec<Value>,
    key: &str,
) -> Action {
    let total = all.len();
    let offset: usize = query
        .get("offset")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let limit: usize = query
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(100);
    let (offset, limit) = if st.list_ignores_paging {
        (0, usize::MAX)
    } else {
        (offset, st.list_page_cap.map_or(limit, |cap| cap.min(limit)))
    };

    if let Some(fail) = st.list_fail_page.as_mut()
        && !st.list_ignores_paging
        && fail.offset == offset
    {
        let status = fail.status;
        let spent = match fail.times.as_mut() {
            Some(0) => true,
            Some(times) => {
                *times -= 1;
                false
            }
            None => false,
        };
        if !spent {
            return Action::Respond(json_response(
                status,
                &error_body(status, "Injected page failure"),
            ));
        }
    }

    let items: Vec<Value> = all.into_iter().skip(offset).take(limit).collect();
    let reported = st.list_total_override.unwrap_or(total as u64);
    Action::Respond(json_response(
        200,
        &json!({key: items, "meta": {"total": reported}, "response_id": "r"}),
    ))
}

fn create_server(st: &mut FakeState, body: Option<Value>, auth: Option<&str>) -> Action {
    let by_configurator = body
        .as_ref()
        .and_then(|body| body["configuration"]["configurator_id"].as_i64())
        .and_then(|id| st.create_by_configurator.get(&id).cloned());
    let mode = by_configurator.unwrap_or_else(|| st.create.clone());
    match mode {
        CreateMode::Status(status) => {
            return Action::Respond(error_response(st, status, "Injected create failure", auth));
        }
        CreateMode::Reject { status, message } => {
            return Action::Respond(error_response(st, status, &message, auth));
        }
        CreateMode::Coded { status, error_code } => {
            let mut payload = error_body(status, "Injected create refusal");
            payload["error_code"] = Value::String(error_code);
            return Action::Respond(json_response(status, &payload));
        }
        CreateMode::Ok | CreateMode::AcceptThenDrop | CreateMode::AcceptThenHang => {}
    }
    let Some(body) = body else {
        return Action::Respond(error_response(st, 400, "Bad body", auth));
    };
    let configuration = &body["configuration"];
    let id = st.next_server_id;
    st.next_server_id += 1;
    let server = FakeServer {
        id,
        name: body["name"].as_str().unwrap_or_default().to_owned(),
        comment: body["comment"].as_str().unwrap_or_default().to_owned(),
        project_id: body["project_id"].as_i64().unwrap_or(0),
        configurator_id: configuration["configurator_id"].as_i64().unwrap_or(0),
        cpu: configuration["cpu"].as_u64().unwrap_or(0),
        ram_mb: configuration["ram"].as_u64().unwrap_or(0),
        disk_mb: configuration["disk"].as_u64().unwrap_or(0),
        ipv4: st.auto_ipv4.then(|| {
            let host = u8::try_from(id % 250).unwrap_or(1).max(1);
            format!("203.0.113.{host}")
        }),
        boot_polls_left: st.boot_polls,
        hidden_polls_left: st.listing_hidden_polls,
        request: body,
    };
    let rendered = server_json(&server);
    st.servers.insert(id, server);
    match mode {
        CreateMode::AcceptThenDrop => Action::Drop,
        CreateMode::AcceptThenHang => Action::Hang,
        _ => Action::Respond(json_response(
            201,
            &json!({"server": rendered, "response_id": "r"}),
        )),
    }
}

/// `POST /servers/{id}/ips`: адрес выдаётся серверу и записывается плавающим, привязанным.
fn add_ip(st: &mut FakeState, id: &str, body: Option<Value>, auth: Option<&str>) -> Action {
    let Some(id) = id
        .parse::<i64>()
        .ok()
        .filter(|id| st.servers.contains_key(id))
    else {
        return Action::Respond(error_response(st, 404, "Resource not found", auth));
    };
    if body.as_ref().and_then(|body| body["type"].as_str()) != Some("ipv4") {
        return Action::Respond(error_response(st, 400, "Only ipv4 is expected", auth));
    }
    let mode = st.ip_add.clone();
    if let IpAddMode::Status(status) = mode {
        return Action::Respond(error_response(st, status, "Injected IP failure", auth));
    }
    if let IpAddMode::Refuse {
        status,
        error_code,
        required_balance,
    } = mode
    {
        let mut payload = error_body(status, "Injected IP refusal");
        payload["error_code"] = Value::String(error_code);
        if let Some(balance) = required_balance {
            payload["details"] = json!({"required_balance": balance});
        }
        return Action::Respond(json_response(status, &payload));
    }
    let ip = format!("203.0.113.{}", st.next_ip_host);
    st.next_ip_host = st.next_ip_host.saturating_add(1);
    st.floating_ips.push(FakeFloatingIp {
        id: format!(
            "fip-{}",
            st.floating_ips.len() as u64 + u64::from(st.next_ip_host)
        ),
        ip: ip.clone(),
        resource_id: Some(id),
    });
    if let Some(server) = st.servers.get_mut(&id) {
        server.ipv4 = Some(ip.clone());
    }
    if mode == IpAddMode::AcceptThenDrop {
        return Action::Drop;
    }
    Action::Respond(json_response(
        201,
        &json!({"server_ip": {"type": "ipv4", "ip": ip, "ptr": "", "is_main": true},
                 "response_id": "r"}),
    ))
}

/// Сервер убран: его адрес остаётся на аккаунте плавающим, без привязки.
fn unbind_ips(st: &mut FakeState, server: i64) {
    for fip in st.floating_ips.iter_mut() {
        if fip.resource_id == Some(server) {
            fip.resource_id = None;
        }
    }
}

fn delete_server(st: &mut FakeState, id: &str, auth: Option<&str>) -> Action {
    let Some(id) = id
        .parse::<i64>()
        .ok()
        .filter(|id| st.servers.contains_key(id))
    else {
        return Action::Respond(error_response(st, 404, "Resource not found", auth));
    };
    match st.delete.clone() {
        DeleteMode::Ok => {
            st.servers.remove(&id);
            unbind_ips(st, id);
            Action::Respond(json_response(
                200,
                &json!({"server_delete": {"is_moved_in_quarantine": false}, "response_id": "r"}),
            ))
        }
        DeleteMode::NoContent => {
            st.servers.remove(&id);
            unbind_ips(st, id);
            Action::Respond(empty_response(204))
        }
        DeleteMode::Quarantine => {
            st.servers.remove(&id);
            unbind_ips(st, id);
            Action::Respond(json_response(
                200,
                &json!({"server_delete": {"is_moved_in_quarantine": true}, "response_id": "r"}),
            ))
        }
        DeleteMode::QuarantineListed => Action::Respond(json_response(
            200,
            &json!({"server_delete": {"is_moved_in_quarantine": true}, "response_id": "r"}),
        )),
        DeleteMode::WithHash => Action::Respond(json_response(
            200,
            &json!({"server_delete": {
                "hash": "15095f25-aac3-4d60-a788-96cb5136f186",
                "is_moved_in_quarantine": false,
            }, "response_id": "r"}),
        )),
        DeleteMode::Locked => {
            Action::Respond(error_response(st, 423, "Deletion by SMS is on", auth))
        }
        DeleteMode::Status(status) => {
            Action::Respond(error_response(st, status, "Injected delete failure", auth))
        }
    }
}
