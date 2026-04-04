/// Hash a password using bcrypt with cost factor 12 (matching Node.js implementation).
/// This is CPU-bound so we run it on a blocking thread.
pub async fn hash_password(password: &str) -> Result<String, bcrypt::BcryptError> {
    let password = password.to_string();
    tokio::task::spawn_blocking(move || bcrypt::hash(password, 12))
        .await
        .expect("bcrypt hash task panicked")
}

/// Verify a password against a bcrypt hash.
/// This is CPU-bound so we run it on a blocking thread.
pub async fn verify_password(password: &str, hash: &str) -> Result<bool, bcrypt::BcryptError> {
    let password = password.to_string();
    let hash = hash.to_string();
    tokio::task::spawn_blocking(move || bcrypt::verify(password, &hash))
        .await
        .expect("bcrypt verify task panicked")
}
