use serde::Serialize;
use sqlx::SqlitePool;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct User {
    pub id: i64,
    pub username: String,
    #[serde(skip_serializing)]
    pub password_hash: String,
    pub git_name: Option<String>,
    pub git_email: Option<String>,
    pub has_completed_onboarding: bool,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub last_login: Option<String>,
}

/// Check if any users exist (for setup detection).
pub async fn has_users(pool: &SqlitePool) -> Result<bool, sqlx::Error> {
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users")
        .fetch_one(pool)
        .await?;
    Ok(row.0 > 0)
}

/// Create a new user.
pub async fn create_user(
    pool: &SqlitePool,
    username: &str,
    password_hash: &str,
) -> Result<User, sqlx::Error> {
    let id = sqlx::query(
        "INSERT INTO users (username, password_hash, created_at, updated_at) VALUES (?, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
    )
    .bind(username)
    .bind(password_hash)
    .execute(pool)
    .await?
    .last_insert_rowid();

    get_by_id(pool, id)
        .await?
        .ok_or(sqlx::Error::RowNotFound)
}

/// Look up a user by username.
pub async fn get_by_username(
    pool: &SqlitePool,
    username: &str,
) -> Result<Option<User>, sqlx::Error> {
    sqlx::query_as::<_, User>("SELECT * FROM users WHERE username = ?")
        .bind(username)
        .fetch_optional(pool)
        .await
}

/// Look up a user by ID.
pub async fn get_by_id(pool: &SqlitePool, id: i64) -> Result<Option<User>, sqlx::Error> {
    sqlx::query_as::<_, User>("SELECT * FROM users WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
}

/// Get the first registered user (for platform mode).
pub async fn get_first_user(pool: &SqlitePool) -> Result<Option<User>, sqlx::Error> {
    sqlx::query_as::<_, User>("SELECT * FROM users ORDER BY id ASC LIMIT 1")
        .fetch_optional(pool)
        .await
}

/// Update last_login timestamp.
pub async fn update_last_login(pool: &SqlitePool, user_id: i64) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE users SET last_login = CURRENT_TIMESTAMP WHERE id = ?")
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Update git configuration.
pub async fn update_git_config(
    pool: &SqlitePool,
    user_id: i64,
    git_name: Option<&str>,
    git_email: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE users SET git_name = COALESCE(?, git_name), git_email = COALESCE(?, git_email), updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(git_name)
    .bind(git_email)
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Mark onboarding as completed.
pub async fn complete_onboarding(pool: &SqlitePool, user_id: i64) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE users SET has_completed_onboarding = 1, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(())
}
