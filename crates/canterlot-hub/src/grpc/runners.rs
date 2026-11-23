//! Раннеры на потоке управления (`runners_v1`): список раннеров несущей, токен по запросу и
//! наблюдения. Допуск один на все сообщения: `machines::carrier_gate`. Сообщения непостоянные:
//! ни в outbox, ни в журнал, ни в лог они не попадают, токен — тем более.

use canterlot_proto::worker::v1::{
    AssignedRunner, ControlDown, OwnerJobAccepted, OwnerJobRefused, OwnerJobSubmit,
    RunnerObservation, RunnerToken, RunnerTokenRefused, RunnerTokenRequest, RunnersAssigned,
    control_down,
};
use tonic::Status;
use uuid::Uuid;

use super::HubGrpc;
use crate::machines::{self, RunnerRefusal};

/// Что хаб последним сказал этой несущей: пусто, пока не говорил.
pub(super) type Sent = Option<Vec<(Uuid, String, String)>>;

fn refused(runner: &str, code: &str) -> ControlDown {
    ControlDown {
        msg: Some(control_down::Msg::RunnerTokenRefused(RunnerTokenRefused {
            machine_id: runner.to_owned(),
            code: code.to_owned(),
        })),
    }
}

/// Отказ в журнал: только идентификаторы и код, никаких значений.
fn warn_refused(caller: Uuid, runner: &str, what: &'static str, code: &'static str) {
    tracing::warn!(
        kind = "runner_message_refused",
        what,
        code,
        machine_id = %caller,
        runner_id = runner,
        "runner message refused"
    );
}

impl HubGrpc {
    /// Ответ на `RunnerTokenRequest`: токен или отказ с кодом. `Err` — только сбой базы.
    pub(super) async fn runner_token_reply(
        &self,
        caller: Uuid,
        request: &RunnerTokenRequest,
    ) -> Result<ControlDown, Status> {
        let Ok(runner) = Uuid::parse_str(&request.machine_id) else {
            warn_refused(caller, "invalid", "token_request", "unknown_machine");
            return Ok(refused(&request.machine_id, "unknown_machine"));
        };
        match machines::request_runner_token(&self.pool, caller, runner, request.reset_local_state)
            .await
        {
            Ok(token) => Ok(ControlDown {
                msg: Some(control_down::Msg::RunnerToken(RunnerToken {
                    machine_id: runner.to_string(),
                    installation_id: self.installation.installation_id.to_string(),
                    restore_generation: self.installation.restore_generation.to_string(),
                    token,
                })),
            }),
            Err(refusal) => match refusal.code() {
                Some(code) => {
                    warn_refused(caller, &runner.to_string(), "token_request", code);
                    Ok(refused(&runner.to_string(), code))
                }
                None => Err(Status::internal("runner token unavailable")),
            },
        }
    }

    /// Ответ на `OwnerJobSubmit`: идентификатор принятой задачи или отказ с кодом. `Err` —
    /// только сбой базы. Описание задачи в журнал не попадает (в `Debug` у него лишь размер).
    pub(super) async fn owner_job_reply(
        &self,
        caller: Uuid,
        submit: &OwnerJobSubmit,
    ) -> Result<ControlDown, Status> {
        let message = match crate::inplace_jobs::owner::submit_with_cloud(
            &self.pool,
            &self.registry,
            self.cloud
                .as_ref()
                .map(|cloud| cloud.config().labels.as_slice()),
            caller,
            &submit.spec_json,
        )
        .await
        {
            Ok(job_id) => control_down::Msg::OwnerJobAccepted(OwnerJobAccepted {
                request_id: submit.request_id.clone(),
                job_id: job_id.to_string(),
            }),
            Err(refusal) if refusal.code == "internal" => {
                return Err(Status::internal("owner job unavailable"));
            }
            Err(refusal) => control_down::Msg::OwnerJobRefused(OwnerJobRefused {
                request_id: submit.request_id.clone(),
                code: refusal.code,
                message: refusal.message,
            }),
        };
        Ok(ControlDown { msg: Some(message) })
    }

    /// `RunnerObservation`: после допуска в `runner_observations`. Отказ отбрасывает сообщение
    /// (ответить нечем) и пишет предупреждение; поток остаётся.
    pub(super) async fn runner_observation(
        &self,
        caller: Uuid,
        observation: &RunnerObservation,
    ) -> Result<(), Status> {
        let Ok(runner) = Uuid::parse_str(&observation.machine_id) else {
            warn_refused(caller, "invalid", "observation", "unknown_machine");
            return Ok(());
        };
        let now = chrono::Utc::now();
        // Часы несущей приводятся к часам хаба; без образца берётся «сейчас».
        let observed_at = self
            .registry
            .to_hub_time(caller, observation.observed_at)
            .unwrap_or(now);
        match machines::record_runner_observation(
            &self.pool,
            caller,
            runner,
            observation.reachable,
            observation.consecutive_failures,
            observed_at,
        )
        .await
        {
            Ok(()) => {
                // Наблюдение могло сменить доступность раннера: клиенты узнают об этом сразу,
                // а не на следующем обходе проектора.
                if let Err(error) = crate::machine_info::project_availability(
                    &self.pool,
                    &self.registry,
                    Uuid::now_v7(),
                )
                .await
                {
                    tracing::warn!(
                        kind = "runner_availability_projection",
                        error = %crate::dberr::describe(&error),
                        "runner availability projection failed"
                    );
                }
                Ok(())
            }
            Err(RunnerRefusal::Internal) => Err(Status::internal("runner observation unavailable")),
            Err(refusal) => {
                warn_refused(
                    caller,
                    &runner.to_string(),
                    "observation",
                    refusal.code().unwrap_or("internal"),
                );
                Ok(())
            }
        }
    }

    /// Список раннеров, если он изменился с прошлой отправки (или это первая отправка несущей).
    /// Не несущая ничего не получает; несущая, потерявшая флаг, получает пустой список.
    pub(super) async fn runners_assigned_if_changed(
        &self,
        machine_id: Uuid,
        sent: &mut Sent,
    ) -> Result<Option<ControlDown>, sqlx::Error> {
        let carrier: bool = sqlx::query_scalar(
            "select coalesce((select role = 'worker' and is_carrier from machines where id = $1), false)",
        )
        .bind(machine_id)
        .fetch_one(&self.pool)
        .await?;
        let current: Vec<(Uuid, String, String)> = if carrier {
            machines::runners_of(&self.pool, machine_id)
                .await?
                .into_iter()
                .map(|runner| (runner.machine_id, runner.name, runner.root_dir))
                .collect()
        } else {
            Vec::new()
        };
        let changed = match sent {
            None => carrier,
            Some(previous) => *previous != current,
        };
        if !changed {
            return Ok(None);
        }
        let message = ControlDown {
            msg: Some(control_down::Msg::RunnersAssigned(RunnersAssigned {
                runners: current
                    .iter()
                    .map(|(id, name, root_dir)| AssignedRunner {
                        machine_id: id.to_string(),
                        name: name.clone(),
                        root_dir: root_dir.clone(),
                    })
                    .collect(),
            })),
        };
        *sent = Some(current);
        Ok(Some(message))
    }
}
