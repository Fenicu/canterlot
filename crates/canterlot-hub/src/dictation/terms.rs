//! Словарь замен владельца в базе: строки идут в порядке `position`, замена
//! списка — одной транзакцией (удалить всё и вставить заново).

use sqlx::PgConnection;
use uuid::Uuid;

use super::dictionary::Term;

pub async fn load(conn: &mut PgConnection, owner_id: Uuid) -> Result<Vec<Term>, sqlx::Error> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "select pattern, replacement from dictation_terms
          where owner_id=$1 order by position",
    )
    .bind(owner_id)
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(pattern, replacement)| Term {
            pattern,
            replacement,
        })
        .collect())
}

pub async fn replace(
    conn: &mut PgConnection,
    owner_id: Uuid,
    terms: &[Term],
) -> Result<(), sqlx::Error> {
    sqlx::query("delete from dictation_terms where owner_id=$1")
        .bind(owner_id)
        .execute(&mut *conn)
        .await?;
    for (position, term) in terms.iter().enumerate() {
        sqlx::query(
            "insert into dictation_terms(owner_id, position, pattern, replacement)
             values($1,$2,$3,$4)",
        )
        .bind(owner_id)
        .bind(position as i32)
        .bind(&term.pattern)
        .bind(&term.replacement)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}
