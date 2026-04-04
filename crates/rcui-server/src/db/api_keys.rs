use serde::Serialize;
use sqlx::SqlitePool;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct ApiKey {
    pub id: i64,
    pub user_id: i64,
    pub api_key: String,
    pub key_name: Option<String>,
    pub is_active: bool,
    pub created_at: Option<String>,
    pub last_used: Option<String>,
}

/// Generate a random API key string.
pub fn generate_key() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    let bytes: Vec<u8> = (0..32).map(|_| rng.random::<u8>()).collect();
    format!("rcui_{}", hex::encode(bytes))
}

/// Create a new API key.
pub async fn create(
    pool: &SqlitePool,
    user_id: i64,
    key_name: &str,
) -> Result<ApiKey, sqlx::Error> {
    let key = generate_key();
    let id = sqlx::query(
        "INSERT INTO api_keys (user_id, api_key, key_name, created_at) VALUES (?, ?, ?, CURRENT_TIMESTAMP)",
    )
    .bind(user_id)
    .bind(&key)
    .bind(key_name)
    .execute(pool)
    .await?
    .last_insert_rowid();

    Ok(ApiKey {
        id,
        user_id,
        api_key: key,
        key_name: Some(key_name.to_string()),
        is_active: true,
        created_at: None,
        last_used: None,
    })
}

/// List all API keys for a user (keys will be redacted by the route handler).
pub async fn get_all_for_user(
    pool: &SqlitePool,
    user_id: i64,
) -> Result<Vec<ApiKey>, sqlx::Error> {
    sqlx::query_as::<_, ApiKey>("SELECT * FROM api_keys WHERE user_id = ? ORDER BY created_at DESC")
        .bind(user_id)
        .fetch_all(pool)
        .await
}

/// Validate an API key and return the associated user_id if active.
pub async fn validate_key(pool: &SqlitePool, key: &str) -> Result<Option<i64>, sqlx::Error> {
    let row: Option<(i64,)> =
        sqlx::query_as("SELECT user_id FROM api_keys WHERE api_key = ? AND is_active = 1")
            .bind(key)
            .fetch_optional(pool)
            .await?;

    if let Some((user_id,)) = row {
        // Update last_used
        sqlx::query("UPDATE api_keys SET last_used = CURRENT_TIMESTAMP WHERE api_key = ?")
            .bind(key)
            .execute(pool)
            .await?;
        Ok(Some(user_id))
    } else {
        Ok(None)
    }
}

/// Delete an API key.
pub async fn delete(pool: &SqlitePool, key_id: i64, user_id: i64) -> Result<bool, sqlx::Error> {
    let result =
        sqlx::query("DELETE FROM api_keys WHERE id = ? AND user_id = ?")
            .bind(key_id)
            .bind(user_id)
            .execute(pool)
            .await?;
    Ok(result.rows_affected() > 0)
}

/// Toggle active status.
pub async fn toggle(
    pool: &SqlitePool,
    key_id: i64,
    user_id: i64,
    is_active: bool,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE api_keys SET is_active = ? WHERE id = ? AND user_id = ?",
    )
    .bind(is_active)
    .bind(key_id)
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}
