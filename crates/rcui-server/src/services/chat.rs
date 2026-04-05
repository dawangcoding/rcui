use std::sync::Arc;

use base64::Engine;
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
    #[serde(default)]
    pub images: Option<Vec<Value>>,
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
    pub tool_id: Option<String>,
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
            tool_id: None,
            request_id: None,
            role: None,
            extra: None,
        }
    }
}

// ─── Stderr Drain Helper ─────────────────────────────────────────────────────

/// Spawn a background task to drain a child process's stderr, preventing pipe
/// buffer deadlock. Lines are logged at warn level for diagnostics.
fn drain_stderr(stderr: tokio::process::ChildStderr, provider: &'static str) {
    tokio::spawn(async move {
        let reader = BufReader::new(stderr);
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                warn!(provider, stderr = %trimmed, "CLI stderr");
            }
        }
    });
}

/// Inactivity timeout for CLI processes (120 seconds).
const INACTIVITY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

// ─── Image Handling ──────────────────────────────────────────────────────────

/// Result of processing images for CLI consumption.
struct ImageProcessingResult {
    /// The prompt with image file paths appended.
    modified_command: String,
    /// Temporary directory containing image files (for cleanup).
    temp_dir: Option<std::path::PathBuf>,
}

/// Save base64 data-URI images to temporary files and append their paths to the prompt.
/// Returns the modified prompt and the temp directory path for cleanup.
async fn handle_images(command: &str, images: &[Value], cwd: &str) -> ImageProcessingResult {
    let mut temp_image_paths = Vec::new();
    let temp_dir = std::path::PathBuf::from(cwd)
        .join(".tmp")
        .join("images")
        .join(format!(
            "{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        ));

    if let Err(e) = tokio::fs::create_dir_all(&temp_dir).await {
        warn!(error = %e, "Failed to create temp image directory");
        return ImageProcessingResult {
            modified_command: command.to_string(),
            temp_dir: None,
        };
    }

    for (index, image) in images.iter().enumerate() {
        let data_uri = match image.get("data").and_then(|v| v.as_str()) {
            Some(d) => d,
            None => continue,
        };

        // Parse data URI: data:<mimeType>;base64,<base64data>
        let base64_data = if let Some(pos) = data_uri.find(";base64,") {
            &data_uri[pos + 8..]
        } else {
            // Not a data URI, try as raw base64
            data_uri
        };

        let mime_type = if data_uri.starts_with("data:") {
            data_uri
                .strip_prefix("data:")
                .and_then(|s| s.split(';').next())
                .unwrap_or("image/png")
        } else {
            image
                .get("mimeType")
                .and_then(|v| v.as_str())
                .unwrap_or("image/png")
        };

        let extension = mime_type.split('/').nth(1).unwrap_or("png");
        let filename = format!("image_{index}.{extension}");
        let filepath = temp_dir.join(&filename);

        match base64::engine::general_purpose::STANDARD.decode(base64_data) {
            Ok(bytes) => {
                if let Err(e) = tokio::fs::write(&filepath, &bytes).await {
                    warn!(error = %e, "Failed to write temp image file");
                    continue;
                }
                temp_image_paths.push(filepath);
            }
            Err(e) => {
                warn!(error = %e, "Failed to decode base64 image data");
                continue;
            }
        }
    }

    if temp_image_paths.is_empty() {
        // No images were processed; clean up the empty directory
        let _ = tokio::fs::remove_dir_all(&temp_dir).await;
        return ImageProcessingResult {
            modified_command: command.to_string(),
            temp_dir: None,
        };
    }

    // Append image file paths to the prompt
    let image_note = format!(
        "\n\n[Images provided at the following paths:]\n{}",
        temp_image_paths
            .iter()
            .enumerate()
            .map(|(i, p)| format!("{}. {}", i + 1, p.display()))
            .collect::<Vec<_>>()
            .join("\n")
    );

    info!(count = temp_image_paths.len(), "Images saved to temp files");

    ImageProcessingResult {
        modified_command: format!("{command}{image_note}"),
        temp_dir: Some(temp_dir),
    }
}

/// Delay before cleaning up temporary image files.
/// The CLI mode requires images on disk for Claude to read via tools, and the
/// frontend may also load referenced files after the session completes.
const IMAGE_CLEANUP_DELAY: std::time::Duration = std::time::Duration::from_secs(300);

/// Schedule deferred cleanup of temporary image files and directory.
fn schedule_temp_image_cleanup(temp_dir: Option<std::path::PathBuf>) {
    if let Some(dir) = temp_dir {
        tokio::spawn(async move {
            tokio::time::sleep(IMAGE_CLEANUP_DELAY).await;
            if let Err(e) = tokio::fs::remove_dir_all(&dir).await {
                // Directory may already be gone — that's fine.
                if e.kind() != std::io::ErrorKind::NotFound {
                    warn!(error = %e, path = %dir.display(), "Failed to clean up temp image directory");
                }
            }
        });
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
            spawn_cursor(command, opts, tx, state).await;
        }
        ChatCommand::CodexCommand { command, options } => {
            info!(provider = "codex", cwd = ?options.cwd, "Executing command");
            spawn_codex(command, options, tx, state).await;
        }
        ChatCommand::GeminiCommand { command, options } => {
            info!(provider = "gemini", cwd = ?options.cwd, "Executing command");
            spawn_gemini(command, options, tx, state).await;
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
            let _ = tx.send(ChatResponse {
                kind: "pending_permissions".to_string(),
                session_id: Some(session_id),
                provider: "claude".to_string(),
                extra: Some(json!({ "permissions": [] })),
                ..ChatResponse::empty()
            });
        }
        ChatCommand::ClaudePermissionResponse {
            request_id,
            allow,
            updated_input,
            message,
        } => {
            info!(%request_id, %allow, "Forwarding permission response to Claude CLI");
            // Find the active Claude session and forward the response via stdin
            let mut target_key = None;
            for entry in state.active_sessions.iter() {
                if entry.key().starts_with("claude:") {
                    target_key = Some(entry.key().clone());
                    break;
                }
            }
            if let Some(key) = target_key {
                if let Some(entry) = state.active_sessions.get(&key) {
                    let session = entry.lock().await;
                    if let Some(ref stdin_tx) = session.stdin_tx {
                        // Build the permission response JSON for Claude CLI stdin.
                        let mut response = json!({
                            "type": "permission_response",
                            "requestId": request_id,
                            "result": if allow { "allow" } else { "deny" },
                        });
                        if let Some(input) = updated_input {
                            response
                                .as_object_mut()
                                .unwrap()
                                .insert("updatedInput".to_string(), input);
                        }
                        if let Some(msg) = message {
                            response
                                .as_object_mut()
                                .unwrap()
                                .insert("message".to_string(), json!(msg));
                        }
                        let _ = stdin_tx.send(response.to_string());
                    } else {
                        warn!(%request_id, "No stdin channel for Claude session");
                    }
                }
            } else {
                warn!(%request_id, "No active Claude session found for permission response");
            }
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

    // Handle images: save to temp files and modify prompt with file paths
    let image_result = if let Some(ref images) = opts.images {
        if !images.is_empty() {
            Some(handle_images(&prompt, images, cwd).await)
        } else {
            None
        }
    } else {
        None
    };

    let final_prompt = match image_result {
        Some(ref r) => r.modified_command.clone(),
        None => prompt,
    };

    let mut args: Vec<String> = vec![
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--verbose".to_string(),
        "-p".to_string(),
        final_prompt,
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

    // Pass allowedTools from frontend settings (localStorage-based tool permissions).
    // These are saved by the user via the permission UI and sent in toolsSettings.
    if let Some(ref tools_settings) = opts.tools_settings {
        if tools_settings
            .get("skipPermissions")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
            && !args.contains(&"--dangerously-skip-permissions".to_string())
        {
            args.push("--dangerously-skip-permissions".to_string());
        }

        if let Some(allowed) = tools_settings.get("allowedTools").and_then(|v| v.as_array()) {
            let tools: Vec<&str> = allowed.iter().filter_map(|v| v.as_str()).collect();
            if !tools.is_empty() {
                // Merge with existing --allowedTools if present, or add new
                if let Some(idx) = args.iter().position(|a| a == "--allowedTools") {
                    // Append to existing value
                    if let Some(existing) = args.get_mut(idx + 1) {
                        existing.push(',');
                        existing.push_str(&tools.join(","));
                    }
                } else {
                    args.push("--allowedTools".to_string());
                    args.push(tools.join(","));
                }
            }
        }
    }

    info!(provider, %cwd, args = ?args, "Spawning CLI process");

    let result = Command::new("claude")
        .args(&args)
        .current_dir(cwd)
        .env("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn();

    let temp_dir = image_result.and_then(|r| r.temp_dir);

    let mut child = match result {
        Ok(c) => c,
        Err(e) => {
            error!(provider, error = %e, "Failed to spawn CLI process");
            let _ = tx.send(ChatResponse::error(
                &format!("Failed to spawn claude: {e}"),
                None,
                provider,
            ));
            schedule_temp_image_cleanup(temp_dir);
            return;
        }
    };

    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => {
            error!(provider, "No stdout from CLI process");
            let _ = tx.send(ChatResponse::error("No stdout from claude", None, provider));
            schedule_temp_image_cleanup(temp_dir);
            return;
        }
    };

    // Drain stderr to prevent pipe buffer deadlock
    if let Some(stderr) = child.stderr.take() {
        drain_stderr(stderr, "claude");
    }

    // Set up stdin channel for writing permission responses
    let stdin_tx = if let Some(child_stdin) = child.stdin.take() {
        let (tx_stdin, mut rx_stdin) = mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let mut stdin = child_stdin;
            while let Some(line) = rx_stdin.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                if stdin.write_all(b"\n").await.is_err() {
                    break;
                }
                let _ = stdin.flush().await;
            }
        });
        Some(tx_stdin)
    } else {
        None
    };

    // Register active session
    let session_key = format!("{provider}:pending");
    let (abort_tx, mut abort_rx) = tokio::sync::oneshot::channel();
    state.active_sessions.insert(
        session_key.clone(),
        Arc::new(tokio::sync::Mutex::new(crate::state::ActiveSession {
            child,
            abort_tx,
            stdin_tx,
        })),
    );

    let mut session_id = String::new();
    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();
    let mut last_activity = tokio::time::Instant::now();
    let mut tracker = ClaudeStreamTracker::new();

    loop {
        tokio::select! {
            line_result = lines.next_line() => {
                last_activity = tokio::time::Instant::now();
                match line_result {
                    Ok(Some(line)) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }

                        if let Ok(event) = serde_json::from_str::<Value>(trimmed) {
                            let parsed = parse_claude_stream_event(&event, &session_id, &mut tracker);

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
                schedule_temp_image_cleanup(temp_dir);
                return;
            }
            _ = tokio::time::sleep_until(last_activity + INACTIVITY_TIMEOUT) => {
                warn!(provider, %session_id, "Session timed out (120s inactivity)");
                let _ = tx.send(ChatResponse::error(
                    "Session timed out (120s inactivity)",
                    Some(&session_id),
                    provider,
                ));
                break;
            }
        }
    }

    // Process finished — clean up active session and temp images
    let real_key = if session_id.is_empty() {
        session_key
    } else {
        format!("{provider}:{session_id}")
    };
    state.active_sessions.remove(&real_key);
    schedule_temp_image_cleanup(temp_dir);
    info!(provider, %session_id, exit_code = 0, "Process completed");
    let _ = tx.send(ChatResponse::complete(&session_id, provider, 0, false));
}

/// State tracker for Claude stream-json content blocks.
/// Accumulates thinking text and tool input across multiple delta events,
/// emitting complete messages on content_block_stop.
struct ClaudeStreamTracker {
    /// The type of the current content block being streamed.
    current_block_type: Option<String>,
    /// Accumulated thinking text for the current thinking block.
    thinking_buffer: String,
    /// Accumulated partial JSON for the current tool_use input.
    tool_input_buffer: String,
    /// Tool name from the current tool_use content_block_start.
    current_tool_name: Option<String>,
    /// Tool ID from the current tool_use content_block_start.
    current_tool_id: Option<String>,
}

impl ClaudeStreamTracker {
    fn new() -> Self {
        Self {
            current_block_type: None,
            thinking_buffer: String::new(),
            tool_input_buffer: String::new(),
            current_tool_name: None,
            current_tool_id: None,
        }
    }
}

/// Parse a Claude stream-json event into ChatResponse messages.
fn parse_claude_stream_event(
    event: &Value,
    session_id: &str,
    tracker: &mut ClaudeStreamTracker,
) -> Vec<ChatResponse> {
    let provider = "claude";
    let mut msgs = Vec::new();

    let kind = event.get("type").and_then(|v| v.as_str()).unwrap_or("");

    match kind {
        "assistant" => {
            // Initial message event — content is typically empty in stream mode.
            // If content is populated (non-streaming or final), extract all parts.
            if let Some(content) = event.get("message").and_then(|m| m.get("content")) {
                if let Some(text) = content.as_str() {
                    msgs.push(ChatResponse::stream_delta(text, session_id, provider));
                } else if let Some(arr) = content.as_array() {
                    for part in arr {
                        let part_type =
                            part.get("type").and_then(|v| v.as_str()).unwrap_or("");
                        match part_type {
                            "text" => {
                                if let Some(t) =
                                    part.get("text").and_then(|v| v.as_str())
                                {
                                    if !t.is_empty() {
                                        msgs.push(ChatResponse::stream_delta(
                                            t, session_id, provider,
                                        ));
                                    }
                                }
                            }
                            "thinking" => {
                                if let Some(t) =
                                    part.get("thinking").and_then(|v| v.as_str())
                                {
                                    if !t.is_empty() {
                                        msgs.push(ChatResponse {
                                            kind: "thinking".to_string(),
                                            content: Some(t.to_string()),
                                            session_id: Some(
                                                session_id.to_string(),
                                            ),
                                            provider: provider.to_string(),
                                            ..ChatResponse::empty()
                                        });
                                    }
                                }
                            }
                            "tool_use" => {
                                let tool_name = part
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("unknown");
                                let tool_id = part
                                    .get("id")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string());
                                let input = part
                                    .get("input")
                                    .cloned()
                                    .unwrap_or(Value::Null);
                                msgs.push(ChatResponse {
                                    kind: "tool_use".to_string(),
                                    tool_name: Some(tool_name.to_string()),
                                    tool_input: Some(input),
                                    tool_id,
                                    session_id: Some(
                                        session_id.to_string(),
                                    ),
                                    provider: provider.to_string(),
                                    ..ChatResponse::empty()
                                });
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        "content_block_start" => {
            if let Some(block) = event.get("content_block") {
                let block_type =
                    block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                tracker.current_block_type = Some(block_type.to_string());
                match block_type {
                    "thinking" => {
                        tracker.thinking_buffer.clear();
                    }
                    "tool_use" => {
                        tracker.current_tool_name = block
                            .get("name")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        tracker.current_tool_id = block
                            .get("id")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        tracker.tool_input_buffer.clear();
                    }
                    _ => {}
                }
            }
        }
        "content_block_delta" => {
            if let Some(delta) = event.get("delta") {
                let delta_type =
                    delta.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match delta_type {
                    "thinking_delta" => {
                        if let Some(thinking) =
                            delta.get("thinking").and_then(|v| v.as_str())
                        {
                            tracker.thinking_buffer.push_str(thinking);
                        }
                    }
                    "input_json_delta" => {
                        if let Some(partial) =
                            delta.get("partial_json").and_then(|v| v.as_str())
                        {
                            tracker.tool_input_buffer.push_str(partial);
                        }
                    }
                    _ => {
                        // text_delta or other — extract text
                        if let Some(text) =
                            delta.get("text").and_then(|v| v.as_str())
                        {
                            msgs.push(ChatResponse::stream_delta(
                                text, session_id, provider,
                            ));
                        }
                    }
                }
            }
        }
        "content_block_stop" => {
            if let Some(ref block_type) = tracker.current_block_type {
                match block_type.as_str() {
                    "thinking" => {
                        if !tracker.thinking_buffer.is_empty() {
                            msgs.push(ChatResponse {
                                kind: "thinking".to_string(),
                                content: Some(std::mem::take(
                                    &mut tracker.thinking_buffer,
                                )),
                                session_id: Some(session_id.to_string()),
                                provider: provider.to_string(),
                                ..ChatResponse::empty()
                            });
                        }
                    }
                    "tool_use" => {
                        let input: Value =
                            if !tracker.tool_input_buffer.is_empty() {
                                serde_json::from_str(&tracker.tool_input_buffer)
                                    .unwrap_or(Value::String(std::mem::take(
                                        &mut tracker.tool_input_buffer,
                                    )))
                            } else {
                                Value::Null
                            };
                        tracker.tool_input_buffer.clear();
                        let tool_name = tracker
                            .current_tool_name
                            .take()
                            .unwrap_or_else(|| "unknown".to_string());
                        let tool_id = tracker.current_tool_id.take();
                        debug!(provider, %session_id, %tool_name, "Tool use from content block");
                        msgs.push(ChatResponse {
                            kind: "tool_use".to_string(),
                            tool_name: Some(tool_name),
                            tool_input: Some(input),
                            tool_id,
                            session_id: Some(session_id.to_string()),
                            provider: provider.to_string(),
                            ..ChatResponse::empty()
                        });
                    }
                    _ => {}
                }
            }
            tracker.current_block_type = None;
        }
        "tool_use" | "tool_use_begin" => {
            // CLI-specific tool_use events (non-content-block format)
            let tool_name = event
                .get("tool")
                .or(event.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let input = event.get("input").cloned().unwrap_or(Value::Null);
            let tool_id = event
                .get("id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            debug!(provider, %session_id, %tool_name, "Tool use event");

            msgs.push(ChatResponse {
                kind: "tool_use".to_string(),
                tool_name: Some(tool_name.to_string()),
                tool_input: Some(input),
                tool_id,
                session_id: Some(session_id.to_string()),
                provider: provider.to_string(),
                ..ChatResponse::empty()
            });
        }
        "tool_result" => {
            let result = event.get("result").or(event.get("output")).cloned();
            let tool_id = event
                .get("tool_use_id")
                .or(event.get("id"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            msgs.push(ChatResponse {
                kind: "tool_result".to_string(),
                tool_result: result,
                tool_id,
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
        // Permission request events from Claude CLI stream-json.
        // Multiple event type names for compatibility across CLI versions.
        "tool_use_permission" | "permission_request" | "ask_permission" => {
            let request_id = event
                .get("requestId")
                .or(event.get("request_id"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let tool_name = event
                .get("toolName")
                .or(event.get("tool"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let input = event
                .get("input")
                .or(event.get("toolInput"))
                .cloned()
                .unwrap_or(Value::Null);

            if !request_id.is_empty() {
                info!(provider, %session_id, %request_id, %tool_name, "Permission request");
                msgs.push(ChatResponse::permission_request(
                    request_id, tool_name, input, session_id,
                ));
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
    state: Arc<AppState>,
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

    // Drain stderr to prevent pipe buffer deadlock
    if let Some(stderr) = child.stderr.take() {
        drain_stderr(stderr, "cursor");
    }

    // Register active session with abort support
    let session_key = format!("{provider}:pending");
    let (abort_tx, mut abort_rx) = tokio::sync::oneshot::channel();
    state.active_sessions.insert(
        session_key.clone(),
        Arc::new(tokio::sync::Mutex::new(crate::state::ActiveSession {
            child,
            abort_tx,
            stdin_tx: None,
        })),
    );

    let mut session_id = opts.session_id.clone().unwrap_or_default();
    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();
    let mut last_activity = tokio::time::Instant::now();

    loop {
        tokio::select! {
            line_result = lines.next_line() => {
                last_activity = tokio::time::Instant::now();
                match line_result {
                    Ok(Some(line)) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }

                        if let Ok(event) = serde_json::from_str::<Value>(trimmed) {
                            let msg_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");

                            match msg_type {
                                "system" => {
                                    if let Some(sid) = event.get("sessionId").and_then(|v| v.as_str()) {
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
                                "assistant" | "text" => {
                                    let content = event
                                        .get("content")
                                        .or(event.get("text"))
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("");
                                    if !content.is_empty() {
                                        let _ = tx.send(ChatResponse::stream_delta(content, &session_id, provider));
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
                let real_key = if session_id.is_empty() { &session_key } else { &format!("{provider}:{session_id}") };
                state.active_sessions.remove(real_key);
                return;
            }
            _ = tokio::time::sleep_until(last_activity + INACTIVITY_TIMEOUT) => {
                warn!(provider, %session_id, "Session timed out (120s inactivity)");
                let _ = tx.send(ChatResponse::error(
                    "Session timed out (120s inactivity)",
                    Some(&session_id),
                    provider,
                ));
                break;
            }
        }
    }

    // Process finished — clean up active session
    let real_key = if session_id.is_empty() {
        session_key
    } else {
        format!("{provider}:{session_id}")
    };
    state.active_sessions.remove(&real_key);
    info!(provider, %session_id, exit_code = 0, "Process completed");
    let _ = tx.send(ChatResponse::complete(&session_id, provider, 0, false));
}

// ─── Codex CLI Spawner ──────────────────────────────────────────────────────

async fn spawn_codex(
    prompt: String,
    opts: CommandOptions,
    tx: mpsc::UnboundedSender<ChatResponse>,
    state: Arc<AppState>,
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

    // Drain stderr to prevent pipe buffer deadlock
    if let Some(stderr) = child.stderr.take() {
        drain_stderr(stderr, "codex");
    }

    let session_id = opts
        .session_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    info!(provider, %session_id, "Session created");
    let _ = tx.send(ChatResponse::session_created(&session_id, provider));

    // Register active session with abort support
    let session_key = format!("{provider}:{session_id}");
    let (abort_tx, mut abort_rx) = tokio::sync::oneshot::channel();
    state.active_sessions.insert(
        session_key.clone(),
        Arc::new(tokio::sync::Mutex::new(crate::state::ActiveSession {
            child,
            abort_tx,
            stdin_tx: None,
        })),
    );

    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();
    let mut last_activity = tokio::time::Instant::now();

    loop {
        tokio::select! {
            line_result = lines.next_line() => {
                last_activity = tokio::time::Instant::now();
                match line_result {
                    Ok(Some(line)) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }

                        if let Ok(event) = serde_json::from_str::<Value>(trimmed) {
                            let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");

                            match event_type {
                                "item" => {
                                    if let Some(item) = event.get("item") {
                                        let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
                                        match item_type {
                                            "message" => {
                                                if let Some(content) = item.get("content").and_then(|v| v.as_array()) {
                                                    for part in content {
                                                        if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                                                            let _ = tx.send(ChatResponse::stream_delta(text, &session_id, provider));
                                                        }
                                                    }
                                                }
                                            }
                                            "function_call" => {
                                                let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("unknown");
                                                let arguments = item.get("arguments").and_then(|v| v.as_str()).unwrap_or("");
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
                                    let msg = event.get("message").and_then(|v| v.as_str()).unwrap_or("Codex error");
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
                state.active_sessions.remove(&session_key);
                return;
            }
            _ = tokio::time::sleep_until(last_activity + INACTIVITY_TIMEOUT) => {
                warn!(provider, %session_id, "Session timed out (120s inactivity)");
                let _ = tx.send(ChatResponse::error(
                    "Session timed out (120s inactivity)",
                    Some(&session_id),
                    provider,
                ));
                break;
            }
        }
    }

    // Process finished — clean up active session
    state.active_sessions.remove(&session_key);
    info!(provider, %session_id, exit_code = 0, "Process completed");
    let _ = tx.send(ChatResponse::complete(&session_id, provider, 0, false));
}

// ─── Gemini CLI Spawner ─────────────────────────────────────────────────────

async fn spawn_gemini(
    prompt: String,
    opts: CommandOptions,
    tx: mpsc::UnboundedSender<ChatResponse>,
    state: Arc<AppState>,
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

    // Drain stderr to prevent pipe buffer deadlock
    if let Some(stderr) = child.stderr.take() {
        drain_stderr(stderr, "gemini");
    }

    let session_id = opts
        .session_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    info!(provider, %session_id, "Session created");
    let _ = tx.send(ChatResponse::session_created(&session_id, provider));

    // Register active session with abort support
    let session_key = format!("{provider}:{session_id}");
    let (abort_tx, mut abort_rx) = tokio::sync::oneshot::channel();
    state.active_sessions.insert(
        session_key.clone(),
        Arc::new(tokio::sync::Mutex::new(crate::state::ActiveSession {
            child,
            abort_tx,
            stdin_tx: None,
        })),
    );

    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();

    loop {
        tokio::select! {
            line_result = tokio::time::timeout(INACTIVITY_TIMEOUT, lines.next_line()) => {
                match line_result {
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
                                        let _ = tx.send(ChatResponse::stream_delta(text, &session_id, provider));
                                    }
                                }
                                "tool_use" => {
                                    let name = event.get("name").and_then(|v| v.as_str()).unwrap_or("unknown");
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
                                        let _ = tx.send(ChatResponse::stream_delta(text, &session_id, provider));
                                    }
                                }
                                "error" => {
                                    let msg = event.get("error").and_then(|v| v.as_str()).unwrap_or("Gemini error");
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
                            "Session timed out (120s inactivity)",
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
                state.active_sessions.remove(&session_key);
                return;
            }
        }
    }

    // Process finished — clean up active session
    state.active_sessions.remove(&session_key);
    info!(provider, %session_id, exit_code = 0, "Process completed");
    let _ = tx.send(ChatResponse::complete(&session_id, provider, 0, false));
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
