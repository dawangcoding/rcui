use std::collections::HashMap;
use std::sync::Arc;

use dashmap::DashMap;
use sqlx::SqlitePool;
use tokio::sync::{Mutex, RwLock, broadcast};

use crate::config::AppConfig;

/// Broadcast message sent to all connected WebSocket clients.
#[derive(Debug, Clone)]
pub enum BroadcastMessage {
    ProjectsUpdated {
        change_type: String,
        changed_file: Option<String>,
        watch_provider: Option<String>,
    },
    LoadingProgress {
        progress: u32,
        message: String,
    },
}

/// Tracks an active CLI session.
pub struct ActiveSession {
    pub child: tokio::process::Child,
    pub abort_tx: tokio::sync::oneshot::Sender<()>,
}

/// PTY session info for shell WebSocket reuse.
pub struct PtySession {
    pub project_path: String,
    pub session_id: String,
    pub buffer: Vec<String>,
    // PTY master will be added when portable-pty is integrated
}

/// Central application state shared across all handlers.
pub struct AppState {
    /// Database connection pool
    pub db: SqlitePool,

    /// JWT signing secret
    pub jwt_secret: String,

    /// Application configuration
    pub config: AppConfig,

    /// Broadcast channel for real-time updates to WebSocket clients
    pub broadcast_tx: broadcast::Sender<BroadcastMessage>,

    /// Active CLI sessions per provider: key = "provider:session_id"
    pub active_sessions: DashMap<String, Arc<Mutex<ActiveSession>>>,

    /// PTY sessions for shell WebSocket reuse
    pub pty_sessions: Arc<Mutex<HashMap<String, PtySession>>>,

    /// Cached project list
    pub project_cache: Arc<RwLock<Option<Vec<serde_json::Value>>>>,
}

impl AppState {
    pub fn new(db: SqlitePool, jwt_secret: String, config: AppConfig) -> Self {
        let (broadcast_tx, _) = broadcast::channel(256);

        Self {
            db,
            jwt_secret,
            config,
            broadcast_tx,
            active_sessions: DashMap::new(),
            pty_sessions: Arc::new(Mutex::new(HashMap::new())),
            project_cache: Arc::new(RwLock::new(None)),
        }
    }
}
