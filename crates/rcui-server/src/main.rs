mod auth;
mod config;
mod db;
mod error;
mod providers;
mod routes;
mod services;
mod state;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::routing::{any, delete, get, patch, post, put};
use axum::Router;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

use config::AppConfig;
use state::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load .env file (ignore if not present)
    dotenvy::dotenv().ok();

    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    // Load configuration
    let config = AppConfig::from_env();
    let addr: SocketAddr = format!("{}:{}", config.host, config.server_port).parse()?;

    // Initialize database
    let pool = db::init_pool(&config.database_path).await?;
    tracing::info!("Database initialized at {:?}", config.database_path);

    // Get or create JWT secret
    let jwt_secret = match std::env::var("JWT_SECRET").ok().filter(|s| !s.is_empty()) {
        Some(secret) => {
            tracing::debug!("JWT secret loaded from environment variable");
            secret
        }
        None => {
            tracing::debug!("JWT secret generated and stored in database");
            db::app_config::get_or_create_jwt_secret(&pool).await?
        }
    };

    // Build application state
    let state = Arc::new(AppState::new(pool, jwt_secret, config.clone()));

    // Start file watcher for live session detection
    services::file_watcher::start_file_watcher(state.clone());

    // CORS layer
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any)
        .expose_headers([axum::http::header::HeaderName::from_static(
            "x-refreshed-token",
        )]);

    // Build router
    let app = Router::new()
        // Health check
        .route("/health", get(routes::health::health_check))
        // WebSocket
        .route("/ws", get(routes::ws::ws_handler))
        // Auth routes (public)
        .route("/api/auth/status", get(routes::auth::status))
        .route("/api/auth/register", post(routes::auth::register))
        .route("/api/auth/login", post(routes::auth::login))
        // Auth routes (protected)
        .route("/api/auth/user", get(routes::auth::user_info))
        .route("/api/auth/logout", post(routes::auth::logout))
        // Project routes
        .route("/api/projects", get(routes::projects::list_projects))
        .route("/api/projects/add", post(routes::projects::add_project))
        .route(
            "/api/projects/create-workspace",
            post(routes::projects::create_workspace),
        )
        .route(
            "/api/projects/{projectName}/rename",
            put(routes::projects::rename_project),
        )
        .route(
            "/api/projects/{projectName}",
            delete(routes::projects::delete_project),
        )
        // File tree and file operations
        .route(
            "/api/projects/{projectName}/files",
            get(routes::projects::list_files)
                .delete(routes::projects::delete_file),
        )
        .route(
            "/api/projects/{projectName}/files/create",
            post(routes::projects::create_file),
        )
        .route(
            "/api/projects/{projectName}/files/rename",
            put(routes::projects::rename_file),
        )
        // upload files: merge a sub-router with a larger body limit (30MB)
        .merge(
            Router::new()
                .route(
                    "/api/projects/{projectName}/files/upload",
                    post(routes::projects::upload_files),
                )
                .layer(DefaultBodyLimit::max(30 * 1024 * 1024)),
        )
        // upload-images: merge a sub-router with a larger body limit (30MB)
        .merge(
            Router::new()
                .route(
                    "/api/projects/{projectName}/upload-images",
                    post(routes::projects::upload_images),
                )
                .layer(DefaultBodyLimit::max(30 * 1024 * 1024)),
        )
        .route(
            "/api/projects/{projectName}/file",
            get(routes::projects::read_file)
                .put(routes::projects::save_file),
        )
        .route(
            "/api/projects/{projectName}/files/content",
            get(routes::projects::read_file_content),
        )
        .route(
            "/api/files/raw",
            get(routes::projects::read_raw_file),
        )
        // Session routes
        .route(
            "/api/projects/{projectName}/sessions",
            get(routes::sessions::list_sessions),
        )
        .route(
            "/api/projects/{projectName}/sessions/{sessionId}",
            delete(routes::sessions::delete_project_session),
        )
        .route(
            "/api/projects/{projectName}/sessions/{sessionId}/token-usage",
            get(routes::sessions::get_session_token_usage),
        )
        .route(
            "/api/sessions/{sessionId}/messages",
            get(routes::sessions::get_session_messages),
        )
        .route(
            "/api/sessions/{sessionId}",
            delete(routes::sessions::delete_session),
        )
        .route(
            "/api/codex/sessions/{sessionId}",
            delete(routes::sessions::delete_codex_session),
        )
        .route(
            "/api/gemini/sessions/{sessionId}",
            delete(routes::sessions::delete_gemini_session),
        )
        .route(
            "/api/sessions/{sessionId}/name",
            post(routes::sessions::set_session_name)
                .delete(routes::sessions::delete_session_name),
        )
        // Settings routes
        .route(
            "/api/settings/api-keys",
            get(routes::settings::list_api_keys).post(routes::settings::create_api_key),
        )
        .route(
            "/api/settings/api-keys/{keyId}",
            delete(routes::settings::delete_api_key),
        )
        .route(
            "/api/settings/api-keys/{keyId}/toggle",
            patch(routes::settings::toggle_api_key),
        )
        .route(
            "/api/settings/credentials",
            get(routes::settings::list_credentials).post(routes::settings::create_credential),
        )
        .route(
            "/api/settings/credentials/{credentialId}",
            delete(routes::settings::delete_credential),
        )
        .route(
            "/api/settings/credentials/{credentialId}/toggle",
            patch(routes::settings::toggle_credential),
        )
        .route(
            "/api/settings/notification-preferences",
            get(routes::settings::get_notification_preferences)
                .put(routes::settings::update_notification_preferences),
        )
        .route(
            "/api/settings/push/vapid-public-key",
            get(routes::settings::get_vapid_public_key),
        )
        .route(
            "/api/settings/push/subscribe",
            post(routes::settings::push_subscribe),
        )
        .route(
            "/api/settings/push/unsubscribe",
            post(routes::settings::push_unsubscribe),
        )
        // User routes
        .route(
            "/api/user/git-config",
            get(routes::user::get_git_config).post(routes::user::update_git_config),
        )
        .route(
            "/api/user/complete-onboarding",
            post(routes::user::complete_onboarding),
        )
        .route(
            "/api/user/onboarding-status",
            get(routes::user::get_onboarding_status),
        )
        // Git routes
        .route("/api/git/status", get(routes::git::status))
        .route("/api/git/diff", get(routes::git::diff))
        .route("/api/git/file-with-diff", get(routes::git::file_with_diff))
        .route(
            "/api/git/initial-commit",
            post(routes::git::initial_commit),
        )
        .route("/api/git/commit", post(routes::git::commit))
        .route(
            "/api/git/revert-local-commit",
            post(routes::git::revert_local_commit),
        )
        .route("/api/git/branches", get(routes::git::branches))
        .route("/api/git/checkout", post(routes::git::checkout))
        .route("/api/git/create-branch", post(routes::git::create_branch))
        .route("/api/git/delete-branch", post(routes::git::delete_branch))
        .route("/api/git/commits", get(routes::git::commits))
        .route("/api/git/commit-diff", get(routes::git::commit_diff))
        .route("/api/git/remote-status", get(routes::git::remote_status))
        .route("/api/git/fetch", post(routes::git::fetch))
        .route("/api/git/pull", post(routes::git::pull))
        .route("/api/git/push", post(routes::git::push))
        .route("/api/git/publish", post(routes::git::publish))
        .route("/api/git/discard", post(routes::git::discard))
        .route(
            "/api/git/delete-untracked",
            post(routes::git::delete_untracked),
        )
        // MCP routes — Claude CLI
        .route("/api/mcp/cli/list", get(routes::mcp::cli_list))
        .route("/api/mcp/cli/add", post(routes::mcp::cli_add))
        .route("/api/mcp/cli/add-json", post(routes::mcp::cli_add_json))
        .route(
            "/api/mcp/cli/{name}",
            get(routes::mcp::cli_get).delete(routes::mcp::cli_remove),
        )
        // MCP routes — Config direct
        .route("/api/mcp/config/read", get(routes::mcp::config_read))
        // MCP routes — Cursor
        .route("/api/cursor/mcp", get(routes::mcp::cursor_mcp_list))
        .route("/api/cursor/mcp/add", post(routes::mcp::cursor_mcp_add))
        .route(
            "/api/cursor/mcp/add-json",
            post(routes::mcp::cursor_mcp_add_json),
        )
        .route(
            "/api/cursor/mcp/{name}",
            delete(routes::mcp::cursor_mcp_remove),
        )
        // MCP routes — Utilities
        .route(
            "/api/mcp-utils/all-servers",
            get(routes::mcp::all_servers),
        )
        // Command routes
        .route(
            "/api/commands/list",
            post(routes::commands::list_commands),
        )
        .route(
            "/api/commands/load",
            post(routes::commands::load_command),
        )
        .route(
            "/api/commands/execute",
            post(routes::commands::execute_command),
        )
        // Stub routes — endpoints called by frontend but not yet implemented
        .route("/api/plugins", get(stub_plugins))
        .route(
            "/api/cli/{provider}/status",
            get(stub_cli_provider_status),
        )
        .route(
            "/api/taskmaster/installation-status",
            get(stub_not_implemented),
        )
        .route(
            "/api/mcp-utils/taskmaster-server",
            get(stub_not_implemented),
        )
        // Catch-all for unregistered /api/* paths — return JSON 404 instead of SPA fallback
        .route("/api/{*rest}", any(api_fallback));

    tracing::debug!("Router configured with all routes");

    // Serve static frontend files (SPA with fallback to index.html)
    let app = if let Some(ref static_dir) = config.static_dir {
        tracing::info!("Serving static files from {static_dir:?}");
        let index_file = static_dir.join("index.html");
        app.fallback_service(
            ServeDir::new(static_dir).fallback(ServeFile::new(index_file)),
        )
    } else {
        app
    };

    let app = app
        // Layers
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        // State
        .with_state(state);

    tracing::info!(%addr, "RCUI server starting");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

// ─── Stub handlers for unimplemented frontend-required endpoints ─────────────

async fn stub_plugins() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({ "plugins": [] }))
}

async fn stub_cli_provider_status() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "authenticated": false,
        "email": null,
        "error": null,
        "method": null
    }))
}

async fn stub_not_implemented() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "installed": false,
        "available": false
    }))
}

/// Return a JSON 404 for any unregistered /api/* path,
/// preventing the SPA fallback from serving index.html for API requests.
async fn api_fallback(uri: axum::http::Uri) -> axum::http::Response<axum::body::Body> {
    tracing::debug!(%uri, "Unregistered API path requested");
    axum::http::Response::builder()
        .status(axum::http::StatusCode::NOT_FOUND)
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            r#"{"error":"Not Found","message":"This API endpoint is not implemented"}"#,
        ))
        .unwrap()
}
