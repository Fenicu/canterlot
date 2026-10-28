#![allow(dead_code)]
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use axum::{
    body::{Body, Bytes},
    http::Method,
};
use canterlot_hub::{
    attachments::{BlobStore, LocalBlobStore, PublishStep, PublishTrace},
    config::AttachmentsConfig,
    http::{self, AppState},
    testkit::{self, TestClient, TestResponse},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

pub type Hook = Arc<dyn Fn(PublishStep) + Send + Sync>;

/// Записывает шаги публикации; может прервать публикацию после шага как сбой процесса.
#[derive(Default)]
pub struct Recorder {
    pub steps: Mutex<Vec<PublishStep>>,
    pub crash_after: Mutex<Option<PublishStep>>,
    pub hook: Mutex<Option<Hook>>,
}

impl Recorder {
    pub fn steps(&self) -> Vec<PublishStep> {
        self.steps.lock().unwrap().clone()
    }
    pub fn clear(&self) {
        self.steps.lock().unwrap().clear();
    }
    pub fn crash_after(&self, step: Option<PublishStep>) {
        *self.crash_after.lock().unwrap() = step;
    }
    pub fn hook(&self, hook: Option<Hook>) {
        *self.hook.lock().unwrap() = hook;
    }
}

impl PublishTrace for Recorder {
    fn step(&self, step: PublishStep) -> Result<(), ()> {
        self.steps.lock().unwrap().push(step);
        let hook = self.hook.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook(step);
        }
        if *self.crash_after.lock().unwrap() == Some(step) {
            return Err(());
        }
        Ok(())
    }
}

pub struct Fixture {
    pub pool: PgPool,
    pub owner: Uuid,
    pub machine: Uuid,
    pub conversation: Uuid,
    pub state: AppState,
    pub client: TestClient,
    pub store: Arc<LocalBlobStore>,
    pub trace: Arc<Recorder>,
    pub config: AttachmentsConfig,
    _dir: tempfile::TempDir,
}

pub fn sha(bytes: &[u8]) -> String {
    canterlot_hub::attachments::blobstore::hex(&Sha256::digest(bytes))
}

/// Детерминированное содержимое заданной длины.
pub fn content(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u32).wrapping_mul(2_654_435_761).to_le_bytes()[3] ^ seed)
        .collect()
}

impl Fixture {
    pub async fn new(pool: PgPool) -> Self {
        Self::with(pool, |_| {}).await
    }

    pub async fn with(pool: PgPool, tune: impl FnOnce(&mut AttachmentsConfig)) -> Self {
        let (mut deps, dir) =
            testkit::test_app_deps(pool.clone(), testkit::test_http_config(), None).await;
        tune(&mut deps.attachments);
        let trace = Arc::new(Recorder::default());
        let store = Arc::new(LocalBlobStore::open(&deps.attachments, trace.clone()).unwrap());
        deps.blobs = store.clone();
        let config = deps.attachments.clone();
        let owner = deps.installation.owner_id;
        let machine = Uuid::now_v7();
        sqlx::query("insert into machines(id,owner_id,name) values($1,$2,'test')")
            .bind(machine)
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();
        testkit::seed_catalog(&pool, machine).await;
        sqlx::query("update machines set capabilities='{pilot_v1}' where id=$1")
            .bind(machine)
            .execute(&pool)
            .await
            .unwrap();
        let conversation = testkit::seed_conversation(&pool, owner, machine).await;
        let state = AppState::new(deps);
        let client = TestClient::in_process(http::router(state.clone())).as_owner();
        Self {
            pool,
            owner,
            machine,
            conversation,
            state,
            client,
            store,
            trace,
            config,
            _dir: dir,
        }
    }

    pub fn blobs(&self) -> &dyn BlobStore {
        &*self.store
    }

    pub fn blob_path(&self, id: Uuid) -> PathBuf {
        self.store.blob_path(id)
    }

    pub fn staging_path(&self, id: Uuid) -> PathBuf {
        self.store.staging_path(id)
    }

    pub async fn conversation(&self) -> Uuid {
        testkit::seed_conversation(&self.pool, self.owner, self.machine).await
    }

    pub async fn create_in(
        &self,
        conversation: Uuid,
        name: &str,
        mime: &str,
        bytes: &[u8],
    ) -> TestResponse {
        self.client
            .send(
                Method::POST,
                "/api/v1/attachments",
                Some(&format!("create_{}", Uuid::now_v7().simple())),
                Some(json!({"conversation_id":conversation,"name":name,"mime":mime,"size":bytes.len(),"sha256":sha(bytes)})),
            )
            .await
    }

    pub async fn create(&self, name: &str, mime: &str, bytes: &[u8]) -> Uuid {
        let response = self.create_in(self.conversation, name, mime, bytes).await;
        assert_eq!(response.status, 201, "{:?}", response.body);
        id_of(&response)
    }

    pub async fn patch(&self, id: Uuid, offset: u64, body: Body) -> TestResponse {
        self.client
            .send_body(
                Method::PATCH,
                &format!("/api/v1/attachments/{id}/content"),
                &[("upload-offset", offset.to_string())],
                body,
            )
            .await
    }

    pub async fn patch_bytes(&self, id: Uuid, offset: u64, bytes: &[u8]) -> TestResponse {
        self.patch(id, offset, Body::from(Bytes::copy_from_slice(bytes)))
            .await
    }

    pub async fn head(&self, id: Uuid) -> TestResponse {
        self.client
            .send(
                Method::HEAD,
                &format!("/api/v1/attachments/{id}/content"),
                None,
                None,
            )
            .await
    }

    pub async fn complete(&self, id: Uuid, key: &str, body: Option<Value>) -> TestResponse {
        self.client
            .send(
                Method::POST,
                &format!("/api/v1/attachments/{id}/complete"),
                Some(key),
                body,
            )
            .await
    }

    pub async fn cancel(&self, id: Uuid, key: &str) -> TestResponse {
        self.client
            .send(
                Method::DELETE,
                &format!("/api/v1/attachments/{id}"),
                Some(key),
                None,
            )
            .await
    }

    pub async fn upload(&self, id: Uuid, bytes: &[u8]) {
        let response = self.patch_bytes(id, 0, bytes).await;
        assert_eq!(response.status, 204, "{:?}", response.body);
    }

    pub async fn ready_in(&self, conversation: Uuid, name: &str, mime: &str, bytes: &[u8]) -> Uuid {
        let response = self.create_in(conversation, name, mime, bytes).await;
        assert_eq!(response.status, 201, "{:?}", response.body);
        let id = id_of(&response);
        self.upload(id, bytes).await;
        let done = self
            .complete(id, &format!("complete_{}", Uuid::now_v7().simple()), None)
            .await;
        assert_eq!(done.status, 200, "{:?}", done.body);
        id
    }

    pub async fn ready(&self, name: &str, mime: &str, bytes: &[u8]) -> Uuid {
        self.ready_in(self.conversation, name, mime, bytes).await
    }

    pub async fn submit_to(
        &self,
        conversation: Uuid,
        key: &str,
        text: &str,
        ids: &[Uuid],
    ) -> TestResponse {
        self.client
            .send(
                Method::POST,
                &format!("/api/v1/conversations/{conversation}/inputs"),
                Some(key),
                Some(json!({"content":[{"type":"text","text":text}],"attachment_ids":ids})),
            )
            .await
    }

    pub async fn submit(&self, key: &str, text: &str, ids: &[Uuid]) -> TestResponse {
        self.submit_to(self.conversation, key, text, ids).await
    }

    pub async fn state_of(&self, id: Uuid) -> String {
        sqlx::query_scalar("select state from attachments where id=$1")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    pub async fn count(&self, sql: &'static str) -> i64 {
        sqlx::query_scalar(sql).fetch_one(&self.pool).await.unwrap()
    }

    pub async fn age(&self, id: Uuid, hours: i64) {
        sqlx::query(
            "update attachments set updated_at = clock_timestamp() - $2 * interval '1 hour' where id=$1",
        )
        .bind(id)
        .bind(hours as f64)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    pub async fn events(&self, kind: &str) -> Vec<Value> {
        sqlx::query_scalar(
            "select payload from events where scope=$1 and kind=$2 order by scope_seq",
        )
        .bind(format!("conversation:{}", self.conversation))
        .bind(kind)
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }
}

pub fn id_of(response: &TestResponse) -> Uuid {
    response.json::<Value>()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

pub fn error_code(response: &TestResponse) -> String {
    response.json::<Value>()["error"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

pub fn upload_offset(response: &TestResponse) -> Option<u64> {
    response
        .headers
        .get("upload-offset")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
}

pub fn assert_error(response: &TestResponse, status: u16, code: &str, retry: &str) {
    assert_eq!(response.status.as_u16(), status, "{:?}", response.body);
    let body: Value = response.json();
    assert_eq!(body["error"]["code"], code, "{body}");
    assert_eq!(body["error"]["retry"], retry, "{body}");
}
