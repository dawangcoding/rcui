use sqlx::SqlitePool;

/// Get a config value by key.
pub async fn get(pool: &SqlitePool, key: &str) -> Result<Option<String>, sqlx::Error> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT value FROM app_config WHERE key = ?")
            .bind(key)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(v,)| v))
}

/// Set a config value (upsert).
pub async fn set(pool: &SqlitePool, key: &str, value: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO app_config (key, value, created_at) VALUES (?, ?, CURRENT_TIMESTAMP)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(key)
    .bind(value)
    .execute(pool)
    .await?;
    Ok(())
}

/// Get or create the JWT secret (auto-generates 64 random bytes on first call).
pub async fn get_or_create_jwt_secret(pool: &SqlitePool) -> Result<String, sqlx::Error> {
    if let Some(secret) = get(pool, "jwt_secret").await? {
        return Ok(secret);
    }

    use rand::Rng;
    let mut rng = rand::rng();
    let bytes: Vec<u8> = (0..64).map(|_| rng.random::<u8>()).collect();
    let secret = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes);

    set(pool, "jwt_secret", &secret).await?;
    Ok(secret)
}
