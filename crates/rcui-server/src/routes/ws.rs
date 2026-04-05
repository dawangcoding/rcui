use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Query, State, WebSocketUpgrade};
use axum::response::IntoResponse;
use serde::Deserialize;
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, info, warn};

use crate::auth;
use crate::services::chat::{execute_command, ChatCommand, ChatResponse};
use crate::state::{AppState, BroadcastMessage};

#[derive(Deserialize)]
pub struct WsQuery {
    pub token: Option<String>,
}

/// GET /ws — WebSocket upgrade endpoint.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    Query(query): Query<WsQuery>,
) -> impl IntoResponse {
    // Authenticate via token query parameter
    let token = query.token.clone().unwrap_or_default();
    let user_id = auth::jwt::verify_token(&state.jwt_secret, &token)
        .map(|data| data.claims.user_id)
        .ok();

    if user_id.is_some() {
        debug!(user_id = ?user_id, "WebSocket connection authenticated");
    } else {
        warn!("WebSocket connection attempt without valid token");
    }

    ws.on_upgrade(move |socket| handle_socket(socket, state, user_id))
}

async fn handle_socket(socket: WebSocket, state: Arc<AppState>, user_id: Option<i64>) {
    use futures_util::{SinkExt, StreamExt};

    let (mut ws_tx, mut ws_rx) = socket.split();

    // Channel for sending messages back to the WebSocket
    let (chat_tx, mut chat_rx) = mpsc::unbounded_channel::<ChatResponse>();

    // Check authentication
    if user_id.is_none() {
        warn!("WebSocket connection rejected: authentication required");
        let err = ChatResponse::error("Authentication required", None, "system");
        if let Ok(json) = serde_json::to_string(&err) {
            let _ = ws_tx.send(Message::Text(json.into())).await;
        }
        let _ = ws_tx.close().await;
        return;
    }

    info!(user_id = ?user_id, "WebSocket connection established");

    // Subscribe to broadcast channel for project/session updates
    let mut broadcast_rx = state.broadcast_tx.subscribe();
    let forward_state = state.clone();

    // Spawn a task to forward chat responses AND broadcast messages to the WebSocket
    let forward_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                response = chat_rx.recv() => {
                    match response {
                        Some(resp) => {
                            if let Ok(json) = serde_json::to_string(&resp) {
                                if ws_tx.send(Message::Text(json.into())).await.is_err() {
                                    break;
                                }
                            }
                        }
                        None => break,
                    }
                }
                broadcast = broadcast_rx.recv() => {
                    let msg = match broadcast {
                        Ok(BroadcastMessage::ProjectsUpdated { changed_file, .. }) => {
                            let projects = {
                                let cache = forward_state.project_cache.read().await;
                                cache.clone().unwrap_or_default()
                            };
                            serde_json::json!({
                                "type": "projects_updated",
                                "projects": projects,
                                "changedFile": changed_file,
                            })
                        }
                        Ok(BroadcastMessage::LoadingProgress { progress, message }) => {
                            let phase = if progress >= 100 { "complete" } else { "scanning" };
                            serde_json::json!({
                                "type": "loading_progress",
                                "phase": phase,
                                "current": progress,
                                "total": 100,
                                "currentProject": message,
                            })
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            debug!("Broadcast receiver lagged by {n} messages");
                            continue;
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        if ws_tx.send(Message::Text(json.into())).await.is_err() {
                            break;
                        }
                    }
                }
            }
        }
        let _ = ws_tx.close().await;
    });

    // Process incoming WebSocket messages
    while let Some(msg_result) = ws_rx.next().await {
        let msg = match msg_result {
            Ok(m) => m,
            Err(e) => {
                debug!(error = %e, "WebSocket receive error");
                break;
            }
        };

        match msg {
            Message::Text(text) => {
                let text_str: &str = &text;
                match serde_json::from_str::<ChatCommand>(text_str) {
                    Ok(cmd) => {
                        debug!(command = ?std::mem::discriminant(&cmd), "WebSocket command received");
                        let tx = chat_tx.clone();
                        let st = state.clone();
                        // Spawn command execution in a separate task
                        tokio::spawn(async move {
                            execute_command(cmd, tx, st).await;
                        });
                    }
                    Err(e) => {
                        warn!(error = %e, "Invalid WebSocket command");
                        let err_msg = ChatResponse::error(
                            &format!("Invalid command: {e}"),
                            None,
                            "system",
                        );
                        let _ = chat_tx.send(err_msg);
                    }
                }
            }
            Message::Close(_) => {
                debug!("WebSocket close frame received");
                break;
            }
            _ => {} // Ignore ping/pong/binary
        }
    }

    info!(user_id = ?user_id, "WebSocket connection closed");

    // Clean up
    drop(chat_tx); // Signal the forward task to stop
    let _ = forward_task.await;
}
