use std::collections::{HashSet, VecDeque};
use std::io::Read;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Query, State, WebSocketUpgrade};
use axum::response::IntoResponse;
use portable_pty::PtySize;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::auth;
use crate::routes::ws::WsQuery;
use crate::services::pty_manager;
use crate::state::{AppState, PtySession, PTY_BUFFER_CAP};

// ─── Message Types ───────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ShellIncoming {
    #[serde(rename = "init")]
    Init {
        #[serde(rename = "projectPath", default)]
        project_path: String,
        #[serde(rename = "sessionId")]
        session_id: Option<String>,
        #[serde(rename = "hasSession", default)]
        has_session: bool,
        #[serde(default = "default_provider")]
        provider: String,
        #[serde(default = "default_cols")]
        cols: u16,
        #[serde(default = "default_rows")]
        rows: u16,
        #[serde(rename = "initialCommand")]
        initial_command: Option<String>,
        #[serde(rename = "isPlainShell", default)]
        is_plain_shell: bool,
    },
    #[serde(rename = "input")]
    Input { data: String },
    #[serde(rename = "resize")]
    Resize {
        #[serde(default = "default_cols")]
        cols: u16,
        #[serde(default = "default_rows")]
        rows: u16,
    },
}

fn default_provider() -> String {
    "claude".to_string()
}
fn default_cols() -> u16 {
    80
}
fn default_rows() -> u16 {
    24
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum ShellOutgoing {
    #[serde(rename = "output")]
    Output { data: String },
    #[serde(rename = "auth_url")]
    AuthUrl { url: String },
}

/// Maximum number of concurrent PTY sessions.
const MAX_PTY_SESSIONS: usize = 10;

/// Timeout before killing a disconnected PTY session (30 minutes).
const PTY_SESSION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30 * 60);

// ─── Handler ─────────────────────────────────────────────────────────────────

/// GET /shell — WebSocket upgrade for interactive shell.
pub async fn shell_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    Query(query): Query<WsQuery>,
) -> impl IntoResponse {
    let token = query.token.clone().unwrap_or_default();
    let user_id = auth::jwt::verify_token(&state.jwt_secret, &token)
        .map(|data| data.claims.user_id)
        .ok();

    if user_id.is_some() {
        debug!(user_id = ?user_id, "Shell WebSocket authenticated");
    } else {
        warn!("Shell WebSocket connection without valid token");
    }

    ws.on_upgrade(move |socket| handle_shell_socket(socket, state, user_id))
}

// ─── Socket Handler ──────────────────────────────────────────────────────────

async fn handle_shell_socket(socket: WebSocket, state: Arc<AppState>, user_id: Option<i64>) {
    use futures_util::{SinkExt, StreamExt};

    let (mut ws_tx, mut ws_rx) = socket.split();

    // Check authentication
    if user_id.is_none() {
        warn!("Shell WebSocket rejected: authentication required");
        let msg = serde_json::to_string(&ShellOutgoing::Output {
            data: "\x1b[31mAuthentication required\x1b[0m\r\n".to_string(),
        })
        .unwrap_or_default();
        let _ = ws_tx.send(Message::Text(msg.into())).await;
        let _ = ws_tx.close().await;
        return;
    }

    info!(user_id = ?user_id, "Shell WebSocket connected");

    // Channel for PTY output → WebSocket forwarding
    let (shell_tx, mut shell_rx) = mpsc::unbounded_channel::<String>();

    // Forward task: reads from shell_rx and sends to ws_tx
    let forward_task = tokio::spawn(async move {
        while let Some(json_msg) = shell_rx.recv().await {
            if ws_tx
                .send(Message::Text(json_msg.into()))
                .await
                .is_err()
            {
                break;
            }
        }
        let _ = ws_tx.close().await;
    });

    // Track the current session key for this connection
    let mut current_session_key: Option<String> = None;

    // Process incoming WebSocket messages
    while let Some(msg_result) = ws_rx.next().await {
        let msg = match msg_result {
            Ok(m) => m,
            Err(e) => {
                debug!(error = %e, "Shell WebSocket receive error");
                break;
            }
        };

        match msg {
            Message::Text(text) => {
                let text_str: &str = &text;
                match serde_json::from_str::<ShellIncoming>(text_str) {
                    Ok(ShellIncoming::Init {
                        project_path,
                        session_id,
                        has_session,
                        provider,
                        cols,
                        rows,
                        initial_command,
                        is_plain_shell,
                    }) => {
                        debug!(
                            %provider, %project_path, ?session_id, %is_plain_shell,
                            "Shell init received"
                        );

                        if let Err(e) = handle_init(
                            &state,
                            &shell_tx,
                            &mut current_session_key,
                            &project_path,
                            session_id.as_deref(),
                            has_session,
                            &provider,
                            cols,
                            rows,
                            initial_command.as_deref(),
                            is_plain_shell,
                        )
                        .await
                        {
                            warn!(error = %e, "Shell init failed");
                            let msg = serde_json::to_string(&ShellOutgoing::Output {
                                data: format!("\x1b[31mError: {e}\x1b[0m\r\n"),
                            })
                            .unwrap_or_default();
                            let _ = shell_tx.send(msg);
                        }
                    }
                    Ok(ShellIncoming::Input { data }) => {
                        if let Some(ref key) = current_session_key {
                            handle_input(&state, key, &data).await;
                        }
                    }
                    Ok(ShellIncoming::Resize { cols, rows }) => {
                        if let Some(ref key) = current_session_key {
                            handle_resize(&state, key, cols, rows).await;
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, raw = %text_str, "Invalid shell message");
                    }
                }
            }
            Message::Close(_) => {
                debug!("Shell WebSocket close frame");
                break;
            }
            _ => {}
        }
    }

    info!(user_id = ?user_id, "Shell WebSocket disconnected");

    // Detach WebSocket from PTY session and start cleanup timer
    if let Some(ref key) = current_session_key {
        detach_session(&state, key).await;
    }

    // Signal forward task to stop
    drop(shell_tx);
    let _ = forward_task.await;
}

// ─── Init ────────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn handle_init(
    state: &Arc<AppState>,
    shell_tx: &mpsc::UnboundedSender<String>,
    current_session_key: &mut Option<String>,
    project_path: &str,
    session_id: Option<&str>,
    has_session: bool,
    provider: &str,
    cols: u16,
    rows: u16,
    initial_command: Option<&str>,
    is_plain_shell: bool,
) -> Result<(), crate::error::AppError> {
    // Validate inputs
    let resolved_path = pty_manager::validate_project_path(project_path)?;
    if let Some(sid) = session_id {
        pty_manager::validate_session_id(sid)?;
    }

    let is_login = pty_manager::is_login_command(initial_command);

    let session_key = pty_manager::compute_session_key(
        project_path,
        session_id,
        initial_command,
        is_plain_shell,
    );

    let mut sessions = state.pty_sessions.lock().await;

    // For login commands, always kill existing session and create fresh
    if is_login && let Some(mut existing) = sessions.remove(&session_key) {
        info!(%session_key, "Killing existing session for login command");
        let _ = existing.child.kill();
        if let Some(h) = existing.cleanup_handle.take() {
            h.abort();
        }
        if let Some(h) = existing.reader_handle.take() {
            h.abort();
        }
    }

    // Check for existing session (reconnect)
    if let Some(session) = sessions.get_mut(&session_key) {
        info!(%session_key, "Reconnecting to existing PTY session");

        // Cancel cleanup timer
        if let Some(handle) = session.cleanup_handle.take() {
            handle.abort();
        }

        // Replay buffer
        for chunk in &session.buffer {
            let msg = serde_json::to_string(&ShellOutgoing::Output {
                data: chunk.clone(),
            })
            .unwrap_or_default();
            let _ = shell_tx.send(msg);
        }

        // Attach new WebSocket
        session.ws_tx = Some(shell_tx.clone());
        *current_session_key = Some(session_key);

        // Resize to current terminal dimensions
        let _ = session.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });

        return Ok(());
    }

    // Check session limit
    if sessions.len() >= MAX_PTY_SESSIONS {
        return Err(crate::error::AppError::BadRequest(
            "Too many active PTY sessions. Close some terminals first.".to_string(),
        ));
    }

    // Build and spawn
    let (shell, args) = pty_manager::build_shell_command(
        provider,
        has_session,
        session_id,
        initial_command,
        is_plain_shell,
    );

    let cwd = resolved_path.to_string_lossy().to_string();
    let spawned = pty_manager::spawn_pty(&shell, &args, &cwd, cols, rows)?;

    info!(
        %session_key, %provider, %cwd,
        "New PTY session created"
    );

    // Start PTY reader task
    let reader_tx = shell_tx.clone();
    let reader_state = state.clone();
    let reader_key = session_key.clone();
    let mut reader = spawned.reader;

    let reader_handle = tokio::task::spawn_blocking(move || {
        let mut buf = [0u8; 4096];
        let mut url_buffer = String::new();
        let mut announced_urls = HashSet::new();

        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let data = String::from_utf8_lossy(&buf[..n]).to_string();

                    // Detect auth URLs
                    let detected = pty_manager::detect_auth_urls(
                        &mut url_buffer,
                        &data,
                        &mut announced_urls,
                    );
                    for (url, _auto_open) in detected {
                        let auth_msg =
                            serde_json::to_string(&ShellOutgoing::AuthUrl { url })
                                .unwrap_or_default();
                        let _ = reader_tx.send(auth_msg);
                    }

                    // Buffer the data and forward to WebSocket
                    let output_msg = serde_json::to_string(&ShellOutgoing::Output {
                        data: data.clone(),
                    })
                    .unwrap_or_default();

                    // We need to update the session buffer.
                    // Since we're in a blocking context, use try_lock or a channel.
                    // We'll forward the data and let the main task handle buffering.
                    // Actually, we buffer directly using a sync approach.
                    // The reader_tx.send doubles as the output AND the buffer signal.
                    let _ = reader_tx.send(output_msg);

                    // Buffer update via runtime handle
                    let state2 = reader_state.clone();
                    let key2 = reader_key.clone();
                    let data2 = data;
                    // Use Handle::block_on to update buffer from blocking context
                    if let Ok(handle) = tokio::runtime::Handle::try_current() {
                        handle.spawn(async move {
                            let mut sessions = state2.pty_sessions.lock().await;
                            if let Some(session) = sessions.get_mut(&key2) {
                                pty_manager::push_buffer(&mut session.buffer, data2);
                            }
                        });
                    }
                }
                Err(e) => {
                    debug!(error = %e, "PTY read ended");
                    break;
                }
            }
        }

        // Process exited — send exit message and clean up
        info!(key = %reader_key, "PTY process ended");

        // Try to get exit status
        let exit_msg = {
            let state3 = reader_state.clone();
            let key3 = reader_key.clone();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let exit_code = handle.block_on(async {
                    let mut sessions = state3.pty_sessions.lock().await;
                    if let Some(session) = sessions.get_mut(&key3) {
                        session.child.try_wait().ok().flatten().map(|s| {
                            s.exit_code()
                        })
                    } else {
                        None
                    }
                });
                match exit_code {
                    Some(code) => format!(
                        "\r\n\x1b[33mProcess exited with code {code}\x1b[0m\r\n"
                    ),
                    None => "\r\n\x1b[33mProcess exited\x1b[0m\r\n".to_string(),
                }
            } else {
                "\r\n\x1b[33mProcess exited\x1b[0m\r\n".to_string()
            }
        };

        let exit_out = serde_json::to_string(&ShellOutgoing::Output { data: exit_msg })
            .unwrap_or_default();
        let _ = reader_tx.send(exit_out);

        // Remove session from map
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let state4 = reader_state.clone();
            let key4 = reader_key;
            handle.spawn(async move {
                let mut sessions = state4.pty_sessions.lock().await;
                if let Some(mut removed) = sessions.remove(&key4)
                    && let Some(h) = removed.cleanup_handle.take()
                {
                    h.abort();
                }
            });
        }
    });

    // Register session
    let pty_session = PtySession {
        master: spawned.master,
        writer: spawned.writer,
        child: spawned.child,
        project_path: project_path.to_string(),
        session_id: session_id.unwrap_or("").to_string(),
        provider: provider.to_string(),
        buffer: VecDeque::with_capacity(PTY_BUFFER_CAP),
        ws_tx: Some(shell_tx.clone()),
        reader_handle: Some(reader_handle),
        cleanup_handle: None,
    };

    sessions.insert(session_key.clone(), pty_session);
    *current_session_key = Some(session_key);

    Ok(())
}

// ─── Input ───────────────────────────────────────────────────────────────────

async fn handle_input(state: &Arc<AppState>, session_key: &str, data: &str) {
    let mut sessions = state.pty_sessions.lock().await;
    if let Some(session) = sessions.get_mut(session_key)
        && let Err(e) = session.writer.write_all(data.as_bytes())
    {
        warn!(error = %e, "Failed to write to PTY");
    }
}

// ─── Resize ──────────────────────────────────────────────────────────────────

async fn handle_resize(state: &Arc<AppState>, session_key: &str, cols: u16, rows: u16) {
    let sessions = state.pty_sessions.lock().await;
    if let Some(session) = sessions.get(session_key)
        && let Err(e) = session.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
    {
        warn!(error = %e, "Failed to resize PTY");
    }
}

// ─── Detach / Cleanup ────────────────────────────────────────────────────────

async fn detach_session(state: &Arc<AppState>, session_key: &str) {
    let mut sessions = state.pty_sessions.lock().await;
    if let Some(session) = sessions.get_mut(session_key) {
        // Detach WebSocket
        session.ws_tx = None;

        // Start cleanup timer
        let state2 = state.clone();
        let key = session_key.to_string();
        let cleanup_handle = tokio::spawn(async move {
            tokio::time::sleep(PTY_SESSION_TIMEOUT).await;
            info!(%key, "PTY session timeout — killing");
            let mut sessions = state2.pty_sessions.lock().await;
            if let Some(mut removed) = sessions.remove(&key) {
                let _ = removed.child.kill();
                if let Some(h) = removed.reader_handle.take() {
                    h.abort();
                }
            }
        });

        session.cleanup_handle = Some(cleanup_handle);
        info!(%session_key, "PTY session detached, cleanup timer started (30 min)");
    }
}
