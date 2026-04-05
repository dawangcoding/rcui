use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::sync::Arc;

use dashmap::DashMap;
use portable_pty::MasterPty;
use sqlx::SqlitePool;
use tokio::sync::{Mutex, RwLock, broadcast, mpsc};

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
    /// Channel for writing to the CLI process's stdin (e.g. permission responses).
    pub stdin_tx: Option<tokio::sync::mpsc::UnboundedSender<String>>,
}

/// Maximum number of entries in the PTY circular replay buffer.
pub const PTY_BUFFER_CAP: usize = 5000;

/// PTY session for interactive shell WebSocket.
pub struct PtySession {
    /// PTY master handle — used for resize operations.
    pub master: Box<dyn MasterPty + Send>,
    /// PTY master writer — used to send input to the child process.
    pub writer: Box<dyn Write + Send>,
    /// Child process handle.
    pub child: Box<dyn portable_pty::Child + Send + Sync>,
    /// Project path this session was spawned in.
    pub project_path: String,
    /// Session ID (may be empty for plain-shell).
    pub session_id: String,
    /// Provider name (claude, cursor, codex, gemini, plain-shell).
    pub provider: String,
    /// Circular replay buffer for reconnection.
    pub buffer: VecDeque<String>,
    /// Channel to forward PTY output to the currently attached WebSocket.
    /// `None` when no WebSocket is connected (session is idle/detached).
    pub ws_tx: Option<mpsc::UnboundedSender<String>>,
    /// Background task that reads from PTY stdout and forwards output.
    pub reader_handle: Option<tokio::task::JoinHandle<()>>,
    /// Delayed cleanup task (spawned on WebSocket disconnect, cancelled on reconnect).
    pub cleanup_handle: Option<tokio::task::JoinHandle<()>>,
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

    /// PTY sessions for shell WebSocket reuse: key = session_key
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
