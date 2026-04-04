use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::state::AppState;

// ─── Incoming Messages (Client → Server) ─────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ChatCommand {
    ClaudeCommand {
        command: String,
        #[serde(default)]
        options: CommandOptions,
    },
    CursorCommand {
        command: String,
        #[serde(default)]
        session_id: Option<String>,
        #[serde(default)]
        options: CommandOptions,
    },
    CodexCommand {
        command: String,
        #[serde(default)]
        options: CommandOptions,
    },
    GeminiCommand {
        command: String,
        #[serde(default)]
        options: CommandOptions,
    },
    AbortSession {
        #[serde(rename = "sessionId")]
        session_id: String,
        provider: String,
    },
    CheckSessionStatus {
        #[serde(rename = "sessionId")]
        session_id: String,
        provider: String,
    },
    GetActiveSessions,
    GetPendingPermissions {
        #[serde(rename = "sessionId")]
        session_id: String,
    },
    ClaudePermissionResponse {
        #[serde(rename = "requestId")]
        request_id: String,
        allow: bool,
        #[serde(default)]
        updated_input: Option<Value>,
        #[serde(default)]
        message: Option<String>,
    },
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandOptions {
    pub project_path: Option<String>,
    pub cwd: Option<String>,
    pub session_id: Option<String>,
    pub resume: Option<bool>,
    pub model: Option<String>,
    pub permission_mode: Option<String>,
    pub session_summary: Option<String>,
    pub skip_permissions: Option<bool>,
    #[serde(default)]
    pub tools_settings: Option<Value>,
}

// ─── Outgoing Messages (Server → Client) ──────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatResponse {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_new_session: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aborted: Option<bool>,
    // Tool/reasoning metadata
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_input: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    // Extra provider-specific data
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<Value>,
}

impl ChatResponse {
    pub fn stream_delta(content: &str, session_id: &str, provider: &str) -> Self {
        Self {
            kind: "stream_delta".to_string(),
            content: Some(content.to_string()),
            session_id: Some(session_id.to_string()),
            provider: provider.to_string(),
            ..Self::empty()
        }
    }

    pub fn session_created(session_id: &str, provider: &str) -> Self {
        Self {
            kind: "session_created".to_string(),
            new_session_id: Some(session_id.to_string()),
            session_id: Some(session_id.to_string()),
            provider: provider.to_string(),
            ..Self::empty()
        }
    }

    pub fn complete(session_id: &str, provider: &str, exit_code: i32, aborted: bool) -> Self {
        Self {
            kind: "complete".to_string(),
            session_id: Some(session_id.to_string()),
            provider: provider.to_string(),
            exit_code: Some(exit_code),
            aborted: if aborted { Some(true) } else { None },
            ..Self::empty()
        }
    }

    pub fn error(message: &str, session_id: Option<&str>, provider: &str) -> Self {
        Self {
            kind: "error".to_string(),
            content: Some(message.to_string()),
            session_id: session_id.map(|s| s.to_string()),
            provider: provider.to_string(),
            ..Self::empty()
        }
    }

    pub fn permission_request(
        request_id: &str,
        tool_name: &str,
        input: Value,
        session_id: &str,
    ) -> Self {
        Self {
            kind: "permission_request".to_string(),
            request_id: Some(request_id.to_string()),
            tool_name: Some(tool_name.to_string()),
            tool_input: Some(input),
            session_id: Some(session_id.to_string()),
            provider: "claude".to_string(),
            ..Self::empty()
        }
    }

    fn empty() -> Self {
        Self {
            kind: String::new(),
            content: None,
            session_id: None,
            provider: String::new(),
            new_session_id: None,
            exit_code: None,
            is_new_session: None,
            aborted: None,
            tool_name: None,
            tool_input: None,
            tool_result: None,
            request_id: None,
            role: None,
            extra: None,
        }
    }
}

// ─── Provider CLI Execution ──────────────────────────────────────────────────

/// Execute a chat command by dispatching to the appropriate provider.
pub async fn execute_command(
    cmd: ChatCommand,
    tx: mpsc::UnboundedSender<ChatResponse>,
    state: Arc<AppState>,
) {
    match cmd {
        ChatCommand::ClaudeCommand { command, options } => {
            info!(provider = "claude", cwd = ?options.cwd, session_id = ?options.session_id, "Executing command");
            spawn_claude(command, options, tx, state).await;
        }
        ChatCommand::CursorCommand {
            command,
            session_id,
            options,
        } => {
            info!(provider = "cursor", cwd = ?options.cwd, "Executing command");
            let mut opts = options;
            if opts.session_id.is_none() {
                opts.session_id = session_id;
            }
            spawn_cursor(command, opts, tx).await;
        }
        ChatCommand::CodexCommand { command, options } => {
            info!(provider = "codex", cwd = ?options.cwd, "Executing command");
            spawn_codex(command, options, tx).await;
        }
        ChatCommand::GeminiCommand { command, options } => {
            info!(provider = "gemini", cwd = ?options.cwd, "Executing command");
            spawn_gemini(command, options, tx).await;
        }
        ChatCommand::AbortSession {
            session_id,
            provider,
        } => {
            info!(%session_id, %provider, "Aborting session");
            abort_session(&session_id, &provider, &state).await;
            let _ = tx.send(ChatResponse::complete(&session_id, &provider, 1, true));
        }
        ChatCommand::CheckSessionStatus {
            session_id,
            provider,
        } => {
            let active = state
                .active_sessions
                .contains_key(&format!("{provider}:{session_id}"));
            debug!(%session_id, %provider, active, "Session status check");
            let _ = tx.send(ChatResponse {
                kind: "session_status".to_string(),
                session_id: Some(session_id),
                provider,
                extra: Some(json!({ "active": active })),
                ..ChatResponse::empty()
            });
        }
        ChatCommand::GetActiveSessions => {
            let sessions: Vec<String> = state
                .active_sessions
                .iter()
                .map(|e| e.key().clone())
                .collect();
            debug!(count = sessions.len(), "Active sessions queried");
            let _ = tx.send(ChatResponse {
                kind: "active_sessions".to_string(),
                provider: "system".to_string(),
                extra: Some(json!({ "sessions": sessions })),
                ..ChatResponse::empty()
            });
        }
        ChatCommand::GetPendingPermissions { session_id } => {
            // Permission management would require a more complex state machine
            // For now, return empty list
            let _ = tx.send(ChatResponse {
                kind: "pending_permissions".to_string(),
                session_id: Some(session_id),
                provider: "claude".to_string(),
                extra: Some(json!({ "permissions": [] })),
                ..ChatResponse::empty()
            });
        }
        ChatCommand::ClaudePermissionResponse { .. } => {
            // Permission responses need to be forwarded to the Claude process stdin
            // This will be enhanced when we add stdin writing support
        }
    }
}

// ─── Claude CLI Spawner ─────────────────────────────────────────────────────

async fn spawn_claude(
    prompt: String,
    opts: CommandOptions,
    tx: mpsc::UnboundedSender<ChatResponse>,
    state: Arc<AppState>,
) {
    let provider = "claude";
    let cwd = opts
        .cwd
        .as_deref()
        .or(opts.project_path.as_deref())
        .unwrap_or(".");

    let mut args: Vec<String> = vec![
        "--output-format".to_string(),
        "stream-json".to_string(),
        "-p".to_string(),
        prompt,
    ];

    if let Some(ref sid) = opts.session_id {
        if opts.resume.unwrap_or(false) {
            args.push("--resume".to_string());
            args.push(sid.clone());
        }
    }

    if let Some(ref model) = opts.model {
        args.push("--model".to_string());
        args.push(model.clone());
    }

    match opts.permission_mode.as_deref() {
        Some("bypassPermissions") => {
            args.push("--dangerously-skip-permissions".to_string());
        }
        Some("acceptEdits") => {
            args.push("--allowedTools".to_string());
            args.push("Edit,Write,MultiEdit".to_string());
        }
        _ => {}
    }

    info!(provider, %cwd, args = ?args, "Spawning CLI process");

    let result = Command::new("claude")
        .args(&args)
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn();

    let mut child = match result {
        Ok(c) => c,
        Err(e) => {
            error!(provider, error = %e, "Failed to spawn CLI process");
            let _ = tx.send(ChatResponse::error(
                &format!("Failed to spawn claude: {e}"),
                None,
                provider,
            ));
            return;
        }
    };

    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => {
            error!(provider, "No stdout from CLI process");
            let _ = tx.send(ChatResponse::error("No stdout from claude", None, provider));
            return;
        }
    };

    // Register active session
    let session_key = format!("{provider}:pending");
    let (abort_tx, mut abort_rx) = tokio::sync::oneshot::channel();
    state.active_sessions.insert(
        session_key.clone(),
        Arc::new(tokio::sync::Mutex::new(crate::state::ActiveSession {
            child,
            abort_tx,
        })),
    );

    let mut session_id = String::new();
    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();

    loop {
        tokio::select! {
            line_result = lines.next_line() => {
                match line_result {
                    Ok(Some(line)) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }

                        if let Ok(event) = serde_json::from_str::<Value>(trimmed) {
                            let parsed = parse_claude_stream_event(&event, &session_id);

                            // Capture session ID from first event
                            if session_id.is_empty() {
                                if let Some(sid) = event.get("session_id").and_then(|v| v.as_str()) {
                                    session_id = sid.to_string();
                                    info!(provider, %session_id, "Session created");
                                    // Re-register with real session ID
                                    if let Some((_, session)) = state.active_sessions.remove(&session_key) {
                                        state.active_sessions.insert(
                                            format!("{provider}:{session_id}"),
                                            session,
                                        );
                                    }
                                    let _ = tx.send(ChatResponse::session_created(&session_id, provider));
                                }
                            }

                            for msg in parsed {
                                let _ = tx.send(msg);
                            }
                        } else {
                            // Non-JSON line: treat as raw text
                            let sid = if session_id.is_empty() { "unknown" } else { &session_id };
                            let _ = tx.send(ChatResponse::stream_delta(trimmed, sid, provider));
                        }
                    }
                    Ok(None) => {
                        debug!(provider, %session_id, "Stream EOF");
                        break;
                    }
                    Err(e) => {
                        error!(provider, %session_id, error = %e, "Stream read error");
                        let _ = tx.send(ChatResponse::error(
                            &format!("Read error: {e}"),
                            Some(&session_id),
                            provider,
                        ));
                        break;
                    }
                }
            }
            _ = &mut abort_rx => {
                info!(provider, %session_id, "Session aborted");
                let _ = tx.send(ChatResponse::complete(&session_id, provider, 1, true));
                state.active_sessions.remove(&format!("{provider}:{session_id}"));
                return;
            }
        }
    }

    // Process finished
    state
        .active_sessions
        .remove(&format!("{provider}:{session_id}"));
    info!(provider, %session_id, exit_code = 0, "Process completed");
    let _ = tx.send(ChatResponse::complete(&session_id, provider, 0, false));
}

/// Parse a Claude stream-json event into ChatResponse messages.
fn parse_claude_stream_event(event: &Value, session_id: &str) -> Vec<ChatResponse> {
    let provider = "claude";
    let mut msgs = Vec::new();

    let kind = event.get("type").and_then(|v| v.as_str()).unwrap_or("");

    match kind {
        "assistant" => {
            // Text content from assistant
            if let Some(content) = event.get("message").and_then(|m| m.get("content")) {
                if let Some(text) = content.as_str() {
                    msgs.push(ChatResponse::stream_delta(text, session_id, provider));
                } else if let Some(arr) = content.as_array() {
                    for part in arr {
                        if part.get("type").and_then(|v| v.as_str()) == Some("text") {
                            if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                                msgs.push(ChatResponse::stream_delta(t, session_id, provider));
                            }
                        }
                    }
                }
            }
        }
        "content_block_delta" => {
            if let Some(delta) = event.get("delta") {
                if let Some(text) = delta.get("text").and_then(|v| v.as_str()) {
                    msgs.push(ChatResponse::stream_delta(text, session_id, provider));
                }
            }
        }
        "tool_use" | "tool_use_begin" => {
            let tool_name = event
                .get("tool")
                .or(event.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let input = event.get("input").cloned().unwrap_or(Value::Null);

            debug!(provider, %session_id, %tool_name, "Tool use event");

            msgs.push(ChatResponse {
                kind: "tool_use".to_string(),
                tool_name: Some(tool_name.to_string()),
                tool_input: Some(input),
                session_id: Some(session_id.to_string()),
                provider: provider.to_string(),
                ..ChatResponse::empty()
            });
        }
        "tool_result" => {
            let result = event.get("result").or(event.get("output")).cloned();
            msgs.push(ChatResponse {
                kind: "tool_result".to_string(),
                tool_result: result,
                session_id: Some(session_id.to_string()),
                provider: provider.to_string(),
                ..ChatResponse::empty()
            });
        }
        "result" => {
            // Final result message
            if let Some(result_text) = event.get("result").and_then(|v| v.as_str()) {
                msgs.push(ChatResponse::stream_delta(result_text, session_id, provider));
            }
        }
        "error" => {
            let error_msg = event
                .get("error")
                .and_then(|v| v.as_str())
                .or(event.get("message").and_then(|v| v.as_str()))
                .unwrap_or("Unknown error");
            warn!(provider, %session_id, %error_msg, "Stream error event");
            msgs.push(ChatResponse::error(error_msg, Some(session_id), provider));
        }
        _ => {
            // Pass through other events as stream_delta if they have content
            if let Some(text) = event.get("content").and_then(|v| v.as_str()) {
                msgs.push(ChatResponse::stream_delta(text, session_id, provider));
            }
        }
    }

    msgs
}

// ─── Cursor CLI Spawner ─────────────────────────────────────────────────────

async fn spawn_cursor(
    prompt: String,
    opts: CommandOptions,
    tx: mpsc::UnboundedSender<ChatResponse>,
) {
    let provider = "cursor";
    let cwd = opts
        .cwd
        .as_deref()
        .or(opts.project_path.as_deref())
        .unwrap_or(".");

    let mut args: Vec<String> = Vec::new();

    if let Some(ref sid) = opts.session_id {
        if opts.resume.unwrap_or(false) {
            args.push(format!("--resume={sid}"));
        }
    }

    args.push("-p".to_string());
    args.push(prompt);
    args.push("--output-format".to_string());
    args.push("stream-json".to_string());

    if let Some(ref model) = opts.model {
        args.push("--model".to_string());
        args.push(model.clone());
    }

    args.push("-f".to_string()); // force mode

    info!(provider, %cwd, args = ?args, "Spawning CLI process");

    let result = Command::new("cursor-agent")
        .args(&args)
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn();

    let mut child = match result {
        Ok(c) => c,
        Err(e) => {
            error!(provider, error = %e, "Failed to spawn CLI process");
            let _ = tx.send(ChatResponse::error(
                &format!("Failed to spawn cursor-agent: {e}"),
                None,
                provider,
            ));
            return;
        }
    };

    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => {
            error!(provider, "No stdout from CLI process");
            let _ = tx.send(ChatResponse::error(
                "No stdout from cursor-agent",
                None,
                provider,
            ));
            return;
        }
    };

    let mut session_id = opts.session_id.clone().unwrap_or_default();
    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();

    while let Ok(Some(line)) = lines.next_line().await {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Ok(event) = serde_json::from_str::<Value>(trimmed) {
            let msg_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");

            match msg_type {
                "system" => {
                    // System init message — capture session ID
                    if let Some(sid) = event.get("sessionId").and_then(|v| v.as_str()) {
                        session_id = sid.to_string();
                        info!(provider, %session_id, "Session created");
                        let _ = tx.send(ChatResponse::session_created(&session_id, provider));
                    }
                }
                "assistant" | "text" => {
                    let content = event
                        .get("content")
                        .or(event.get("text"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if !content.is_empty() {
                        let _ =
                            tx.send(ChatResponse::stream_delta(content, &session_id, provider));
                    }
                }
                "result" => {
                    if let Some(text) = event.get("result").and_then(|v| v.as_str()) {
                        let _ = tx.send(ChatResponse::stream_delta(text, &session_id, provider));
                    }
                }
                "error" => {
                    let msg = event
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Cursor error");
                    warn!(provider, %session_id, %msg, "Stream error event");
                    let _ = tx.send(ChatResponse::error(msg, Some(&session_id), provider));
                }
                _ => {
                    // Unknown JSON event — pass content if available
                    if let Some(text) = event.get("content").and_then(|v| v.as_str()) {
                        let _ = tx.send(ChatResponse::stream_delta(text, &session_id, provider));
                    }
                }
            }
        } else {
            // Non-JSON line: treat as raw text output
            if !session_id.is_empty() {
                let _ = tx.send(ChatResponse::stream_delta(trimmed, &session_id, provider));
            }
        }
    }

    // Wait for process exit
    let exit_code = match child.wait().await {
        Ok(status) => status.code().unwrap_or(1),
        Err(_) => 1,
    };

    info!(provider, %session_id, exit_code, "Process completed");

    let _ = tx.send(ChatResponse::complete(
        &session_id,
        provider,
        exit_code,
        false,
    ));
}

// ─── Codex CLI Spawner ──────────────────────────────────────────────────────

async fn spawn_codex(
    prompt: String,
    opts: CommandOptions,
    tx: mpsc::UnboundedSender<ChatResponse>,
) {
    let provider = "codex";
    let cwd = opts
        .cwd
        .as_deref()
        .or(opts.project_path.as_deref())
        .unwrap_or(".");

    let mut args: Vec<String> = vec!["--quiet".to_string()];

    if let Some(ref sid) = opts.session_id {
        if opts.resume.unwrap_or(false) {
            args.push("--resume".to_string());
            args.push(sid.clone());
        }
    }

    if let Some(ref model) = opts.model {
        args.push("--model".to_string());
        args.push(model.clone());
    }

    // Approval mode
    if opts.skip_permissions.unwrap_or(false) {
        args.push("--full-auto".to_string());
    }

    args.push(prompt);

    info!(provider, %cwd, args = ?args, "Spawning CLI process");

    let result = Command::new("codex")
        .args(&args)
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn();

    let mut child = match result {
        Ok(c) => c,
        Err(e) => {
            error!(provider, error = %e, "Failed to spawn CLI process");
            let _ = tx.send(ChatResponse::error(
                &format!("Failed to spawn codex: {e}"),
                None,
                provider,
            ));
            return;
        }
    };

    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => {
            error!(provider, "No stdout from CLI process");
            let _ = tx.send(ChatResponse::error("No stdout from codex", None, provider));
            return;
        }
    };

    let session_id = opts
        .session_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    info!(provider, %session_id, "Session created");
    let _ = tx.send(ChatResponse::session_created(&session_id, provider));

    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();

    while let Ok(Some(line)) = lines.next_line().await {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Ok(event) = serde_json::from_str::<Value>(trimmed) {
            let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");

            match event_type {
                "item" => {
                    // SDK event format
                    if let Some(item) = event.get("item") {
                        let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
                        match item_type {
                            "message" => {
                                if let Some(content) =
                                    item.get("content").and_then(|v| v.as_array())
                                {
                                    for part in content {
                                        if let Some(text) =
                                            part.get("text").and_then(|v| v.as_str())
                                        {
                                            let _ = tx.send(ChatResponse::stream_delta(
                                                text,
                                                &session_id,
                                                provider,
                                            ));
                                        }
                                    }
                                }
                            }
                            "function_call" => {
                                let name = item
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("unknown");
                                let arguments = item
                                    .get("arguments")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                debug!(provider, %session_id, %name, "Tool use event");
                                let _ = tx.send(ChatResponse {
                                    kind: "tool_use".to_string(),
                                    tool_name: Some(name.to_string()),
                                    tool_input: serde_json::from_str(arguments).ok(),
                                    session_id: Some(session_id.clone()),
                                    provider: provider.to_string(),
                                    ..ChatResponse::empty()
                                });
                            }
                            "function_call_output" => {
                                let output = item.get("output").cloned();
                                let _ = tx.send(ChatResponse {
                                    kind: "tool_result".to_string(),
                                    tool_result: output,
                                    session_id: Some(session_id.clone()),
                                    provider: provider.to_string(),
                                    ..ChatResponse::empty()
                                });
                            }
                            _ => {}
                        }
                    }
                }
                "error" => {
                    let msg = event
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Codex error");
                    warn!(provider, %session_id, %msg, "Stream error event");
                    let _ = tx.send(ChatResponse::error(msg, Some(&session_id), provider));
                }
                _ => {
                    if let Some(text) = event.get("content").and_then(|v| v.as_str()) {
                        let _ = tx.send(ChatResponse::stream_delta(text, &session_id, provider));
                    }
                }
            }
        } else {
            // Raw text
            let _ = tx.send(ChatResponse::stream_delta(trimmed, &session_id, provider));
        }
    }

    let exit_code = match child.wait().await {
        Ok(status) => status.code().unwrap_or(1),
        Err(_) => 1,
    };

    info!(provider, %session_id, exit_code, "Process completed");

    let _ = tx.send(ChatResponse::complete(
        &session_id,
        provider,
        exit_code,
        false,
    ));
}

// ─── Gemini CLI Spawner ─────────────────────────────────────────────────────

async fn spawn_gemini(
    prompt: String,
    opts: CommandOptions,
    tx: mpsc::UnboundedSender<ChatResponse>,
) {
    let provider = "gemini";
    let cwd = opts
        .cwd
        .as_deref()
        .or(opts.project_path.as_deref())
        .unwrap_or(".");

    let mut args: Vec<String> = vec![
        "--prompt".to_string(),
        prompt,
        "--output-format".to_string(),
        "stream-json".to_string(),
    ];

    if let Some(ref model) = opts.model {
        args.push("--model".to_string());
        args.push(model.clone());
    }

    if opts.skip_permissions.unwrap_or(false) {
        args.push("--yolo".to_string());
    }

    if let Some(ref sid) = opts.session_id {
        if opts.resume.unwrap_or(false) {
            args.push("--resume".to_string());
            args.push(sid.clone());
        }
    }

    info!(provider, %cwd, args = ?args, "Spawning CLI process");

    let result = Command::new("gemini")
        .args(&args)
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn();

    let mut child = match result {
        Ok(c) => c,
        Err(e) => {
            error!(provider, error = %e, "Failed to spawn CLI process");
            let _ = tx.send(ChatResponse::error(
                &format!("Failed to spawn gemini: {e}"),
                None,
                provider,
            ));
            return;
        }
    };

    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => {
            error!(provider, "No stdout from CLI process");
            let _ = tx.send(ChatResponse::error(
                "No stdout from gemini",
                None,
                provider,
            ));
            return;
        }
    };

    let session_id = opts
        .session_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    info!(provider, %session_id, "Session created");
    let _ = tx.send(ChatResponse::session_created(&session_id, provider));

    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();

    // Timeout: 120 seconds of inactivity
    let timeout_duration = std::time::Duration::from_secs(120);

    loop {
        match tokio::time::timeout(timeout_duration, lines.next_line()).await {
            Ok(Ok(Some(line))) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                if let Ok(event) = serde_json::from_str::<Value>(trimmed) {
                    let kind = event.get("type").and_then(|v| v.as_str()).unwrap_or("");

                    match kind {
                        "message" | "delta" | "content" => {
                            let text = event
                                .get("content")
                                .or(event.get("text"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            if !text.is_empty() {
                                let _ = tx.send(ChatResponse::stream_delta(
                                    text,
                                    &session_id,
                                    provider,
                                ));
                            }
                        }
                        "tool_use" => {
                            let name = event
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown");
                            let input = event.get("input").cloned().unwrap_or(Value::Null);
                            debug!(provider, %session_id, %name, "Tool use event");
                            let _ = tx.send(ChatResponse {
                                kind: "tool_use".to_string(),
                                tool_name: Some(name.to_string()),
                                tool_input: Some(input),
                                session_id: Some(session_id.clone()),
                                provider: provider.to_string(),
                                ..ChatResponse::empty()
                            });
                        }
                        "tool_result" => {
                            let result = event.get("output").cloned();
                            let _ = tx.send(ChatResponse {
                                kind: "tool_result".to_string(),
                                tool_result: result,
                                session_id: Some(session_id.clone()),
                                provider: provider.to_string(),
                                ..ChatResponse::empty()
                            });
                        }
                        "result" => {
                            if let Some(text) = event.get("result").and_then(|v| v.as_str()) {
                                let _ = tx.send(ChatResponse::stream_delta(
                                    text,
                                    &session_id,
                                    provider,
                                ));
                            }
                        }
                        "error" => {
                            let msg = event
                                .get("error")
                                .and_then(|v| v.as_str())
                                .unwrap_or("Gemini error");
                            warn!(provider, %session_id, %msg, "Stream error event");
                            let _ =
                                tx.send(ChatResponse::error(msg, Some(&session_id), provider));
                        }
                        _ => {
                            if let Some(text) = event.get("content").and_then(|v| v.as_str()) {
                                let _ = tx.send(ChatResponse::stream_delta(
                                    text,
                                    &session_id,
                                    provider,
                                ));
                            }
                        }
                    }
                } else {
                    let _ = tx.send(ChatResponse::stream_delta(trimmed, &session_id, provider));
                }
            }
            Ok(Ok(None)) => {
                debug!(provider, %session_id, "Stream EOF");
                break;
            }
            Ok(Err(e)) => {
                error!(provider, %session_id, error = %e, "Stream read error");
                let _ = tx.send(ChatResponse::error(
                    &format!("Read error: {e}"),
                    Some(&session_id),
                    provider,
                ));
                break;
            }
            Err(_) => {
                // Timeout
                warn!(provider, %session_id, "Session timed out (120s inactivity)");
                let _ = tx.send(ChatResponse::error(
                    "Gemini session timed out (120s inactivity)",
                    Some(&session_id),
                    provider,
                ));
                let _ = child.kill().await;
                break;
            }
        }
    }

    let exit_code = match child.wait().await {
        Ok(status) => status.code().unwrap_or(1),
        Err(_) => 1,
    };

    info!(provider, %session_id, exit_code, "Process completed");

    let _ = tx.send(ChatResponse::complete(
        &session_id,
        provider,
        exit_code,
        false,
    ));
}

// ─── Abort ──────────────────────────────────────────────────────────────────

async fn abort_session(session_id: &str, provider: &str, state: &AppState) {
    let key = format!("{provider}:{session_id}");
    if let Some((_, session)) = state.active_sessions.remove(&key) {
        info!(%session_id, %provider, "Killing session process");
        let mut guard = session.lock().await;
        let _ = guard.child.kill().await;
    } else {
        debug!(%session_id, %provider, "Session not found for abort");
    }
}
