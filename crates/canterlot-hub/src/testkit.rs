use sqlx::{PgPool, types::Json};
use uuid::Uuid;

use crate::model::HoldReason;

pub mod tls {
    use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair};

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
