pub mod users;
pub mod api_keys;
pub mod app_config;
pub mod credentials;
pub mod notifications;
pub mod push_subscriptions;
pub mod session_names;

use std::path::Path;

use sqlx::SqlitePool;
use sqlx::sqlite::SqliteConnectOptions;

/// Initialize the database: create the file if needed, run migrations, enable WAL.
pub async fn init_pool(db_path: &Path) -> Result<SqlitePool, sqlx::Error> {
    // Ensure parent directory exists
    if let Some(parent) = db_path.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }

    let options = SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);

    let pool = SqlitePool::connect_with(options).await?;

    // Run migrations from the embedded SQL
    run_migrations(&pool).await?;

    Ok(pool)
}

/// Execute the initial schema migration.
async fn run_migrations(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let migration_sql = include_str!("../../../../migrations/001_initial.sql");

    // Split by semicolon and execute each statement
    for statement in migration_sql.split(';') {
        let trimmed: &str = statement.trim();
        if !trimmed.is_empty() {
            sqlx::query(trimmed).execute(pool).await?;
        }
    }

    Ok(())
}
