use sqlx::PgPool;
use tonic::{Request, Status};
use uuid::Uuid;

use crate::machines;

#[derive(Clone, Copy)]
pub(super) struct AuthenticatedMachine(pub Uuid);

// Перехватчик асинхронный: штатный tonic::Interceptor не может ожидать PostgreSQL.
pub(super) async fn intercept<T>(pool: &PgPool, request: &mut Request<T>) -> Result<(), Status> {
    if request.metadata().get_all("authorization").iter().count() != 1 {
        return Err(Status::unauthenticated("invalid machine credential"));
    }
    let credential = request
        .metadata_mut()
        .get_mut("authorization")
        .ok_or_else(|| Status::unauthenticated("invalid machine credential"))?;
    credential.set_sensitive(true);
    let token = credential
        .to_str()
        .ok()
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|token| !token.is_empty() && !token.chars().any(char::is_whitespace))
        .ok_or_else(|| Status::unauthenticated("invalid machine credential"))?;
    let machine_id = machines::verify(pool, token)
        .await
        .map_err(|_| Status::internal("machine authentication unavailable"))?
        .ok_or_else(|| Status::unauthenticated("invalid machine credential"))?;
    request
        .extensions_mut()
        .insert(AuthenticatedMachine(machine_id));
    Ok(())
}
