use serde::Serialize;
use sqlx::SqlitePool;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct PushSubscription {
    pub id: i64,
    pub user_id: i64,
    pub endpoint: String,
    pub keys_p256dh: String,
    pub keys_auth: String,
    pub created_at: Option<String>,
}

/// Save a push subscription.
pub async fn save(
    pool: &SqlitePool,
    user_id: i64,
    endpoint: &str,
    keys_p256dh: &str,
    keys_auth: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO push_subscriptions (user_id, endpoint, keys_p256dh, keys_auth, created_at)
         VALUES (?, ?, ?, ?, CURRENT_TIMESTAMP)
         ON CONFLICT(endpoint) DO UPDATE SET user_id = excluded.user_id, keys_p256dh = excluded.keys_p256dh, keys_auth = excluded.keys_auth",
    )
    .bind(user_id)
    .bind(endpoint)
    .bind(keys_p256dh)
    .bind(keys_auth)
    .execute(pool)
    .await?;
    Ok(())
}

/// Get all subscriptions for a user.
pub async fn get_for_user(
    pool: &SqlitePool,
    user_id: i64,
) -> Result<Vec<PushSubscription>, sqlx::Error> {
    sqlx::query_as::<_, PushSubscription>(
        "SELECT * FROM push_subscriptions WHERE user_id = ?",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
}

/// Remove a subscription by endpoint.
pub async fn remove(pool: &SqlitePool, endpoint: &str) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM push_subscriptions WHERE endpoint = ?")
        .bind(endpoint)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Get or create VAPID keys.
pub async fn get_or_create_vapid_keys(
    pool: &SqlitePool,
) -> Result<(String, String), sqlx::Error> {
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT public_key, private_key FROM vapid_keys ORDER BY id ASC LIMIT 1",
    )
    .fetch_optional(pool)
    .await?;

    if let Some(keys) = row {
        return Ok(keys);
    }

    // Generate new VAPID keys - placeholder; real implementation needs p256 curve generation
    let public_key = "placeholder_public_key".to_string();
    let private_key = "placeholder_private_key".to_string();

    sqlx::query(
        "INSERT INTO vapid_keys (public_key, private_key, created_at) VALUES (?, ?, CURRENT_TIMESTAMP)",
    )
    .bind(&public_key)
    .bind(&private_key)
    .execute(pool)
    .await?;

    Ok((public_key, private_key))
}
