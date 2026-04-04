use chrono::{Duration, Utc};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, TokenData, Validation};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

/// JWT claims payload.
#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub user_id: i64,
    pub username: String,
    /// Issued at (Unix timestamp)
    pub iat: i64,
    /// Expiration (Unix timestamp)
    pub exp: i64,
}

/// Token expiration: 7 days (matching Node.js implementation).
const TOKEN_EXPIRY_DAYS: i64 = 7;

/// Generate a JWT token for a user.
pub fn generate_token(
    secret: &str,
    user_id: i64,
    username: &str,
) -> Result<String, jsonwebtoken::errors::Error> {
    let now = Utc::now();
    let exp = now + Duration::days(TOKEN_EXPIRY_DAYS);

    let claims = Claims {
        user_id,
        username: username.to_string(),
        iat: now.timestamp(),
        exp: exp.timestamp(),
    };

    debug!(user_id, %username, "Generating JWT token");

    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
}

/// Verify and decode a JWT token.
pub fn verify_token(
    secret: &str,
    token: &str,
) -> Result<TokenData<Claims>, jsonwebtoken::errors::Error> {
    let mut validation = Validation::default();
    validation.validate_exp = true;

    let result = decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    );

    if let Err(ref e) = result {
        warn!(error = %e, "JWT token verification failed");
    }

    result
}

/// Check if a token should be refreshed (past 50% of its lifetime).
pub fn should_refresh(claims: &Claims) -> bool {
    let now = Utc::now().timestamp();
    let total_lifetime = claims.exp - claims.iat;
    let elapsed = now - claims.iat;
    let needs_refresh = elapsed > total_lifetime / 2;
    if needs_refresh {
        debug!(user_id = claims.user_id, "Token eligible for refresh");
    }
    needs_refresh
}
