use std::{sync::Arc, time::Duration};

use canterlot_hub::events::{
    NewEvent, ReadAfter, Scope, StoredEvent, append, boundary, purge_before, read_after,
};
use serde_json::json;
use sqlx::PgPool;
use tokio::{sync::Barrier, task::JoinSet, time::timeout};
use uuid::Uuid;

fn new_event(scope: Scope) -> NewEvent {
    NewEvent {
        scope,
        kind: "block_delta",
        conversation_id: None,
        execution_id: None,
        payload: json!({"block_id": Uuid::now_v7(), "base_rev": 0, "rev": 1, "append": "x"}),
        correlation_id: Uuid::now_v7(),
    }
}

fn events(result: ReadAfter) -> Vec<StoredEvent> {
    match result {
        ReadAfter::Events(events) => events,
        ReadAfter::ResyncRequired {
            oldest_available_seq,
        } => {
            panic!("unexpected resync required at {oldest_available_seq}")
        }
    }
}

fn sequences(result: ReadAfter) -> Vec<i64> {
    events(result).iter().map(|event| event.scope_seq).collect()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn seq_is_contiguous_per_scope(pool: PgPool) -> Result<(), sqlx::Error> {
    let owner_id = Uuid::now_v7();
    let conversation_id = Uuid::now_v7();
    let execution_id = Uuid::now_v7();
    let scope_a = Scope::Conversations { owner_id };
    let scope_b = Scope::Conversation {
        id: conversation_id,
    };
    assert_eq!(scope_a.key(), format!("owner:{owner_id}:conversations"));
    assert_eq!(scope_b.key(), format!("conversation:{conversation_id}"));

    let mut conn = pool.acquire().await?;
    assert_eq!(boundary(&mut conn, &scope_a).await?, 0);
    assert!(events(read_after(&pool, &scope_a, 0, 100).await?).is_empty());
    drop(conn);

    for index in 1..=5 {
        let mut tx = pool.begin().await?;
        let mut event = new_event(Scope::Conversations { owner_id });
        event.conversation_id = Some(conversation_id);
        event.execution_id = Some(execution_id);
        let payload = event.payload.clone();
        let correlation_id = event.correlation_id;
        let stored = append(&mut tx, event).await?;
        assert_eq!(stored.id.get_version_num(), 7);
        assert_eq!(stored.scope, scope_a.key());
        assert_eq!(stored.scope_seq, index);
        assert_eq!(stored.kind, "block_delta");
        assert_eq!(stored.conversation_id, Some(conversation_id));
        assert_eq!(stored.execution_id, Some(execution_id));
        assert_eq!(stored.payload, payload);
        assert_eq!(stored.correlation_id, correlation_id);
        assert_eq!(boundary(&mut tx, &scope_a).await?, index);
        tx.commit().await?;

        let read = events(read_after(&pool, &scope_a, index - 1, 1).await?);
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].id, stored.id);
        assert_eq!(read[0].scope, stored.scope);
        assert_eq!(read[0].scope_seq, stored.scope_seq);
        assert_eq!(read[0].kind, stored.kind);
        assert_eq!(read[0].conversation_id, stored.conversation_id);
        assert_eq!(read[0].execution_id, stored.execution_id);
        assert_eq!(read[0].payload, stored.payload);
        assert_eq!(read[0].correlation_id, stored.correlation_id);
        assert_eq!(read[0].committed_at, stored.committed_at);

        if index <= 3 {
            let mut tx = pool.begin().await?;
            let stored = append(
                &mut tx,
                new_event(Scope::Conversation {
                    id: conversation_id,
                }),
            )
            .await?;
            assert_eq!(stored.scope_seq, index);
            assert_eq!(stored.conversation_id, None);
            assert_eq!(stored.execution_id, None);
            tx.commit().await?;
        }
    }

    assert_eq!(
        sequences(read_after(&pool, &scope_a, 0, 100).await?),
        (1..=5).collect::<Vec<_>>()
    );
    assert_eq!(
        sequences(read_after(&pool, &scope_b, 0, 100).await?),
        (1..=3).collect::<Vec<_>>()
    );
    assert_eq!(sequences(read_after(&pool, &scope_a, 2, 2).await?), [3, 4]);
    assert!(events(read_after(&pool, &scope_a, 5, 100).await?).is_empty());
    assert!(matches!(
        read_after(&pool, &scope_a, i64::MAX, 100).await?,
        ReadAfter::ResyncRequired {
            oldest_available_seq: 1
        }
    ));
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn rolled_back_append_leaves_no_gap(pool: PgPool) -> Result<(), sqlx::Error> {
    let id = Uuid::now_v7();
    let scope = Scope::Conversation { id };
    let mut tx = pool.begin().await?;
    let rolled_back = append(&mut tx, new_event(Scope::Conversation { id })).await?;
    assert_eq!(rolled_back.scope_seq, 1);
    assert_eq!(boundary(&mut tx, &scope).await?, 1);
    let mut conn = pool.acquire().await?;
    assert_eq!(boundary(&mut conn, &scope).await?, 0);
    drop(conn);
    assert!(events(read_after(&pool, &scope, 0, 100).await?).is_empty());
    tx.rollback().await?;

    let mut tx = pool.begin().await?;
    let stored = append(&mut tx, new_event(Scope::Conversation { id })).await?;
    assert_eq!(stored.scope_seq, 1);
    assert_ne!(stored.id, rolled_back.id);
    tx.commit().await?;

    let mut tx = pool.begin().await?;
    assert_eq!(
        append(&mut tx, new_event(Scope::Conversation { id }))
            .await?
            .scope_seq,
        2
    );
    tx.rollback().await?;
    let mut tx = pool.begin().await?;
    assert_eq!(
        append(&mut tx, new_event(Scope::Conversation { id }))
            .await?
            .scope_seq,
        2
    );
    tx.commit().await?;
    assert_eq!(sequences(read_after(&pool, &scope, 0, 100).await?), [1, 2]);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn snapshot_boundary_has_no_lost_event(pool: PgPool) -> Result<(), sqlx::Error> {
    timeout(Duration::from_secs(30), async {
        let id = Uuid::now_v7();
        let scope = Scope::Conversation { id };
        let start = Arc::new(Barrier::new(9));
        let first_commits = Arc::new(Barrier::new(9));
        let mut writers = JoinSet::new();
        for writer in 0..8 {
            let pool = pool.clone();
            let start = start.clone();
            let first_commits = first_commits.clone();
            writers.spawn(async move {
                start.wait().await;
                for index in 0..50 {
                    let mut tx = pool.begin().await?;
                    append(&mut tx, new_event(Scope::Conversation { id })).await?;
                    if (writer + index) % 4 == 0 {
                        sqlx::query("select pg_sleep(0.001)")
                            .execute(&mut *tx)
                            .await?;
                    }
                    tx.commit().await?;
                    if index == 0 {
                        first_commits.wait().await;
                    }
                }
                Ok::<_, sqlx::Error>(())
            });
        }

        start.wait().await;
        let mut snapshot_checks = 0;
        let mut observed_partial = false;
        let mut writers_released = false;
        while !writers.is_empty() {
            let mut tx = pool.begin().await?;
            sqlx::query("set transaction isolation level repeatable read")
                .execute(&mut *tx)
                .await?;
            let boundary = boundary(&mut tx, &scope).await?;
            sqlx::query("select pg_sleep(0.001)")
                .execute(&mut *tx)
                .await?;
            let count: i64 = sqlx::query_scalar(
                "select count(*) from events where scope = $1 and scope_seq <= $2",
            )
            .bind(scope.key())
            .bind(boundary)
            .fetch_one(&mut *tx)
            .await?;
            assert_eq!(count, boundary, "snapshot boundary {boundary}");
            snapshot_checks += 1;
            observed_partial |= boundary > 0 && boundary < 400;
            tx.commit().await?;

            if !writers_released && boundary == 8 {
                first_commits.wait().await;
                writers_released = true;
            }
            while let Some(result) = writers.try_join_next() {
                result.expect("writer task panicked")?;
            }
        }

        assert!(writers_released);
        assert!(observed_partial);
        assert!(snapshot_checks > 1);
        assert_eq!(
            sequences(read_after(&pool, &scope, 0, 1000).await?),
            (1..=400).collect::<Vec<_>>()
        );
        let mut conn = pool.acquire().await?;
        assert_eq!(boundary(&mut conn, &scope).await?, 400);
        println!("snapshot checks={snapshot_checks}, partial=true, final boundary=400");
        Ok::<_, sqlx::Error>(())
    })
    .await
    .expect("concurrent event test timed out")
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn stale_cursor_requires_resync(pool: PgPool) -> Result<(), sqlx::Error> {
    let id = Uuid::now_v7();
    let scope = Scope::Conversation { id };
    let other_id = Uuid::now_v7();
    let other_scope = Scope::Conversation { id: other_id };
    for _ in 0..10 {
        let mut tx = pool.begin().await?;
        append(&mut tx, new_event(Scope::Conversation { id })).await?;
        append(&mut tx, new_event(Scope::Conversation { id: other_id })).await?;
        tx.commit().await?;
    }

    assert_eq!(purge_before(&pool, &scope, 6).await?, 5);
    assert_eq!(purge_before(&pool, &scope, 6).await?, 0);
    assert!(matches!(
        read_after(&pool, &scope, 2, 100).await?,
        ReadAfter::ResyncRequired {
            oldest_available_seq: 6
        }
    ));
    assert_eq!(
        sequences(read_after(&pool, &scope, 5, 100).await?),
        (6..=10).collect::<Vec<_>>()
    );
    assert_eq!(
        sequences(read_after(&pool, &other_scope, 0, 100).await?),
        (1..=10).collect::<Vec<_>>()
    );

    assert_eq!(purge_before(&pool, &scope, 11).await?, 5);
    assert!(matches!(
        read_after(&pool, &scope, 9, 100).await?,
        ReadAfter::ResyncRequired {
            oldest_available_seq: 11
        }
    ));
    assert!(events(read_after(&pool, &scope, 10, 100).await?).is_empty());
    let mut tx = pool.begin().await?;
    assert_eq!(boundary(&mut tx, &scope).await?, 10);
    assert_eq!(
        append(&mut tx, new_event(Scope::Conversation { id }))
            .await?
            .scope_seq,
        11
    );
    tx.commit().await?;
    assert_eq!(sequences(read_after(&pool, &scope, 10, 100).await?), [11]);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn read_after_rejects_invalid_arguments(pool: PgPool) -> Result<(), sqlx::Error> {
    let scope = Scope::Conversation { id: Uuid::now_v7() };
    for (after_seq, limit) in [(-1, 100), (0, 0), (0, -1)] {
        assert!(matches!(
            read_after(&pool, &scope, after_seq, limit).await,
            Err(sqlx::Error::InvalidArgument(_))
        ));
    }
    Ok(())
}
