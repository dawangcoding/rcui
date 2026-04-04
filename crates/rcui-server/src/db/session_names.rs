use sqlx::SqlitePool;

/// Set or update a custom session name.
pub async fn set_name(
    pool: &SqlitePool,
    session_id: &str,
    provider: &str,
    custom_name: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO session_names (session_id, provider, custom_name, created_at, updated_at)
         VALUES (?, ?, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
         ON CONFLICT(session_id, provider)
         DO UPDATE SET custom_name = excluded.custom_name, updated_at = CURRENT_TIMESTAMP",
    )
    .bind(session_id)
    .bind(provider)
    .bind(custom_name)
    .execute(pool)
    .await?;
    Ok(())
}

/// Get a custom name for a session.
pub async fn get_name(
    pool: &SqlitePool,
    session_id: &str,
    provider: &str,
) -> Result<Option<String>, sqlx::Error> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT custom_name FROM session_names WHERE session_id = ? AND provider = ?",
    )
    .bind(session_id)
    .bind(provider)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(name,)| name))
}

/// Get custom names for multiple sessions in batch (returns a map of session_id -> name).
pub async fn get_names_batch(
    pool: &SqlitePool,
    session_ids: &[String],
    provider: &str,
) -> Result<std::collections::HashMap<String, String>, sqlx::Error> {
    if session_ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }

    // Build IN clause with placeholders
    let placeholders: Vec<&str> = session_ids.iter().map(|_| "?").collect();
    let query = format!(
        "SELECT session_id, custom_name FROM session_names WHERE provider = ? AND session_id IN ({})",
        placeholders.join(",")
    );

    let mut q = sqlx::query_as::<_, (String, String)>(&query).bind(provider);
    for sid in session_ids {
        q = q.bind(sid);
    }

    let rows = q.fetch_all(pool).await?;
    Ok(rows.into_iter().collect())
}

/// Delete a custom session name.
pub async fn delete_name(
    pool: &SqlitePool,
    session_id: &str,
    provider: &str,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "DELETE FROM session_names WHERE session_id = ? AND provider = ?",
    )
    .bind(session_id)
    .bind(provider)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}
