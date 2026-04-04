use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::HeaderMap;
use tracing::{debug, warn};

use crate::db;
use crate::error::AppError;
use crate::state::AppState;

use super::jwt;

/// Authenticated user extracted from request.
/// Used as an Axum extractor on protected routes.
pub struct AuthUser {
    pub user_id: i64,
    pub username: String,
    /// If the token was auto-refreshed, this contains the new token.
    /// Route handlers should set the `X-Refreshed-Token` response header.
    pub refreshed_token: Option<String>,
}

impl FromRequestParts<Arc<AppState>> for AuthUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        // Platform mode: bypass JWT, use first database user
        if state.config.is_platform {
            debug!("Platform mode: bypassing JWT authentication");
            let user = db::users::get_first_user(&state.db)
                .await
                .map_err(|e| AppError::Internal(e.into()))?
                .ok_or_else(|| AppError::Unauthorized("No users configured".to_string()))?;
            return Ok(AuthUser {
                user_id: user.id,
                username: user.username,
                refreshed_token: None,
            });
        }

        // Extract token from Authorization header or query parameter
        let token = extract_token(&parts.headers, &parts.uri)
            .ok_or_else(|| {
                warn!(uri = %parts.uri, "Missing authentication token");
                AppError::Unauthorized("Missing authentication token".to_string())
            })?;

        // Verify JWT
        let token_data = jwt::verify_token(&state.jwt_secret, &token)
            .map_err(|_| {
                warn!("Invalid or expired token");
                AppError::Unauthorized("Invalid or expired token".to_string())
            })?;

        let claims = token_data.claims;
        debug!(user_id = claims.user_id, %claims.username, "User authenticated");

        // Auto-refresh if past 50% lifetime
        let refreshed_token = if jwt::should_refresh(&claims) {
            jwt::generate_token(&state.jwt_secret, claims.user_id, &claims.username).ok()
        } else {
            None
        };

        Ok(AuthUser {
            user_id: claims.user_id,
            username: claims.username,
            refreshed_token,
        })
    }
}

/// API key authentication extractor for agent routes.
pub struct ApiKeyAuth {
    pub user_id: i64,
}

impl FromRequestParts<Arc<AppState>> for ApiKeyAuth {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        // Platform mode: use first user
        if state.config.is_platform {
            debug!("ApiKeyAuth: platform mode, using first user");
            let user = db::users::get_first_user(&state.db)
                .await
                .map_err(|e| AppError::Internal(e.into()))?
                .ok_or_else(|| AppError::Unauthorized("No users configured".to_string()))?;
            return Ok(ApiKeyAuth {
                user_id: user.id,
            });
        }

        // Extract API key from header or query
        let api_key = parts
            .headers
            .get("x-api-key")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .or_else(|| extract_query_param(&parts.uri, "apiKey"))
            .ok_or_else(|| {
                warn!("Missing API key");
                AppError::Unauthorized("Missing API key".to_string())
            })?;

        let user_id = db::api_keys::validate_key(&state.db, &api_key)
            .await
            .map_err(|e| AppError::Internal(e.into()))?
            .ok_or_else(|| {
                warn!("Invalid or inactive API key");
                AppError::Unauthorized("Invalid or inactive API key".to_string())
            })?;

        debug!(user_id, "API key authenticated");

        Ok(ApiKeyAuth { user_id })
    }
}

/// Extract Bearer token from Authorization header or `token` query parameter.
fn extract_token(headers: &HeaderMap, uri: &axum::http::Uri) -> Option<String> {
    // Try Authorization header first
    if let Some(auth_header) = headers.get("authorization") {
        if let Ok(value) = auth_header.to_str() {
            if let Some(token) = value.strip_prefix("Bearer ") {
                debug!("Token extracted from Authorization header");
                return Some(token.to_string());
            }
        }
    }

    // Fall back to query parameter (for SSE and WebSocket)
    let token = extract_query_param(uri, "token");
    if token.is_some() {
        debug!("Token extracted from query parameter");
    }
    token
}

/// Extract a named parameter from the URI query string.
fn extract_query_param(uri: &axum::http::Uri, name: &str) -> Option<String> {
    uri.query().and_then(|q| {
        url::form_urlencoded::parse(q.as_bytes())
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.into_owned())
    })
}
