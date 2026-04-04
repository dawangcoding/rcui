use serde::Serialize;
use sqlx::SqlitePool;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Credential {
    pub id: i64,
    pub user_id: i64,
    pub credential_type: String,
    pub credential_name: String,
    #[serde(skip_serializing)]
    pub credential_value: String,
    pub description: Option<String>,
    pub is_active: bool,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

/// Create a new credential.
pub async fn create(
    pool: &SqlitePool,
    user_id: i64,
    credential_type: &str,
    credential_name: &str,
    credential_value: &str,
    description: Option<&str>,
) -> Result<Credential, sqlx::Error> {
    let id = sqlx::query(
        "INSERT INTO user_credentials (user_id, credential_type, credential_name, credential_value, description, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
    )
    .bind(user_id)
    .bind(credential_type)
    .bind(credential_name)
    .bind(credential_value)
    .bind(description)
    .execute(pool)
    .await?
    .last_insert_rowid();

    sqlx::query_as::<_, Credential>("SELECT * FROM user_credentials WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await
}

/// Get all credentials for a user, optionally filtered by type.
pub async fn get_all(
    pool: &SqlitePool,
    user_id: i64,
    credential_type: Option<&str>,
) -> Result<Vec<Credential>, sqlx::Error> {
    if let Some(ctype) = credential_type {
        sqlx::query_as::<_, Credential>(
            "SELECT * FROM user_credentials WHERE user_id = ? AND credential_type = ? ORDER BY created_at DESC",
        )
        .bind(user_id)
        .bind(ctype)
        .fetch_all(pool)
        .await
    } else {
        sqlx::query_as::<_, Credential>(
            "SELECT * FROM user_credentials WHERE user_id = ? ORDER BY created_at DESC",
        )
        .bind(user_id)
        .fetch_all(pool)
        .await
    }
}

/// Get active credentials of a given type.
pub async fn get_active(
    pool: &SqlitePool,
    user_id: i64,
    credential_type: &str,
) -> Result<Vec<Credential>, sqlx::Error> {
    sqlx::query_as::<_, Credential>(
        "SELECT * FROM user_credentials WHERE user_id = ? AND credential_type = ? AND is_active = 1 ORDER BY created_at DESC",
    )
    .bind(user_id)
    .bind(credential_type)
    .fetch_all(pool)
    .await
}

/// Delete a credential.
pub async fn delete(pool: &SqlitePool, credential_id: i64, user_id: i64) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "DELETE FROM user_credentials WHERE id = ? AND user_id = ?",
    )
    .bind(credential_id)
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Toggle active status.
pub async fn toggle(
    pool: &SqlitePool,
    credential_id: i64,
    user_id: i64,
    is_active: bool,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE user_credentials SET is_active = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ? AND user_id = ?",
    )
    .bind(is_active)
    .bind(credential_id)
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}
