use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Query, State, WebSocketUpgrade};
use axum::response::IntoResponse;
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::auth;
use crate::services::chat::{ChatCommand, ChatResponse, execute_command};
use crate::state::AppState;

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

    ws.on_upgrade(move |socket| handle_socket(socket, state, user_id))
}

async fn handle_socket(socket: WebSocket, state: Arc<AppState>, user_id: Option<i64>) {
    use futures_util::{SinkExt, StreamExt};

    let (mut ws_tx, mut ws_rx) = socket.split();

    // Channel for sending messages back to the WebSocket
    let (chat_tx, mut chat_rx) = mpsc::unbounded_channel::<ChatResponse>();

    // Check authentication
    if user_id.is_none() {
        let err = ChatResponse::error("Authentication required", None, "system");
        if let Ok(json) = serde_json::to_string(&err) {
            let _ = ws_tx.send(Message::Text(json.into())).await;
        }
        let _ = ws_tx.close().await;
        return;
    }

    // Spawn a task to forward chat responses to the WebSocket
    let forward_task = tokio::spawn(async move {
        while let Some(response) = chat_rx.recv().await {
            if let Ok(json) = serde_json::to_string(&response) {
                if ws_tx.send(Message::Text(json.into())).await.is_err() {
                    break;
                }
            }
        }
        let _ = ws_tx.close().await;
    });

    // Process incoming WebSocket messages
    while let Some(msg_result) = ws_rx.next().await {
        let msg = match msg_result {
            Ok(m) => m,
            Err(_) => break,
        };

        match msg {
            Message::Text(text) => {
                let text_str: &str = &text;
                match serde_json::from_str::<ChatCommand>(text_str) {
                    Ok(cmd) => {
                        let tx = chat_tx.clone();
                        let st = state.clone();
                        // Spawn command execution in a separate task
                        tokio::spawn(async move {
                            execute_command(cmd, tx, st).await;
                        });
                    }
                    Err(e) => {
                        let err_msg = ChatResponse::error(
                            &format!("Invalid command: {e}"),
                            None,
                            "system",
                        );
                        let _ = chat_tx.send(err_msg);
                    }
                }
            }
            Message::Close(_) => break,
            _ => {} // Ignore ping/pong/binary
        }
    }

    // Clean up
    drop(chat_tx); // Signal the forward task to stop
    let _ = forward_task.await;
}
