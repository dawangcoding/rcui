use sqlx::SqlitePool;

/// Get notification preferences for a user.
pub async fn get_preferences(
    pool: &SqlitePool,
    user_id: i64,
) -> Result<Option<String>, sqlx::Error> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT preferences_json FROM user_notification_preferences WHERE user_id = ?",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(json,)| json))
}

/// Update notification preferences.
pub async fn update_preferences(
    pool: &SqlitePool,
    user_id: i64,
    preferences_json: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO user_notification_preferences (user_id, preferences_json, updated_at)
         VALUES (?, ?, CURRENT_TIMESTAMP)
         ON CONFLICT(user_id) DO UPDATE SET preferences_json = excluded.preferences_json, updated_at = CURRENT_TIMESTAMP",
    )
    .bind(user_id)
    .bind(preferences_json)
    .execute(pool)
    .await?;
    Ok(())
}
