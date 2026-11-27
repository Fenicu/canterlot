//! `WorkerEnroll`: обмен кода привязки на личность машины. Служба стоит рядом с потоками
//! воркера на том же порту и TLS, но без `auth::intercept`: у новой машины токена ещё нет,
//! её удостоверяет только код.

use std::sync::Arc;

use canterlot_proto::enroll::v1::{
    GetCaRequest, GetCaResponse, JoinRequest, JoinResponse, worker_enroll_server::WorkerEnroll,
};
use sqlx::PgPool;
use tonic::{Request, Response, Status};

use crate::{
    enroll::{self, EnrollError, EnrollSettings, JoinLimiter},
    installation::Installation,
};

/// Имя машины и версия воркера идут только в журнал: длиннее этого не пишем.
const LOGGED_FIELD_MAX: usize = 128;

#[derive(Clone)]
pub(super) struct EnrollService {
    pub pool: PgPool,
    pub installation: Installation,
    pub settings: Arc<EnrollSettings>,
    pub limiter: Arc<JoinLimiter>,
}

fn ca_unavailable() -> Status {
    Status::failed_precondition("ca_unavailable")
}

fn clipped(value: &str) -> &str {
    let end = value.floor_char_boundary(value.len().min(LOGGED_FIELD_MAX));
    &value[..end]
}

#[tonic::async_trait]
impl WorkerEnroll for EnrollService {
    async fn get_ca(
        &self,
        _request: Request<GetCaRequest>,
    ) -> Result<Response<GetCaResponse>, Status> {
        let ca_der = self.settings.ca_der.clone().ok_or_else(ca_unavailable)?;
        Ok(Response::new(GetCaResponse { ca_der }))
    }

    async fn join(&self, request: Request<JoinRequest>) -> Result<Response<JoinResponse>, Status> {
        let peer = request.remote_addr().map(|addr| addr.ip());
        if !self.limiter.admit(peer) {
            tracing::warn!(kind = "join_rate_limited", peer = ?peer, "join attempt rate limited");
            return Err(Status::resource_exhausted("rate_limited"));
        }
        // Без CA воркеру нечем проверить хаб: код не тратится.
        let ca_der = self.settings.ca_der.clone().ok_or_else(ca_unavailable)?;
        let JoinRequest {
            code,
            hostname,
            worker_version,
        } = request.into_inner();
        let redeemed = match enroll::redeem(&self.pool, &code).await {
            Ok(redeemed) => redeemed,
            Err(EnrollError::Invalid) => {
                tracing::warn!(kind = "join_code_rejected", peer = ?peer, "join code rejected");
                return Err(Status::unauthenticated("join_code_invalid"));
            }
            Err(error) => {
                tracing::error!(kind = "join_failed", error = %error, "join code redemption failed");
                return Err(Status::internal("enroll unavailable"));
            }
        };
        tracing::info!(
            kind = "machine_joined",
            machine_id = %redeemed.machine_id,
            peer = ?peer,
            hostname = ?clipped(&hostname),
            worker_version = ?clipped(&worker_version),
            "machine redeemed a join code"
        );
        Ok(Response::new(JoinResponse {
            machine_id: redeemed.machine_id.to_string(),
            installation_id: self.installation.installation_id.to_string(),
            restore_generation: self.installation.restore_generation.to_string(),
            token: redeemed.token,
            ca_der,
            grpc_endpoint: self.settings.grpc_endpoint.clone().unwrap_or_default(),
        }))
    }
}
