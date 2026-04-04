use std::path::PathBuf;

use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::middleware::AuthUser;
use crate::error::AppError;

// ─── Types ───────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListCommandsRequest {
    pub project_path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadCommandRequest {
    pub command_path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecuteCommandRequest {
    pub command_name: String,
    pub command_path: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub context: Option<Value>,
}

// ─── Endpoints ───────────────────────────────────────────────────────────────

/// POST /api/commands/list — List all available commands (built-in + custom).
pub async fn list_commands(
    _auth: AuthUser,
    Json(body): Json<ListCommandsRequest>,
) -> Result<Json<Value>, AppError> {
    let home = dirs::home_dir().unwrap_or_default();

    // Built-in commands
    let built_in = vec![
        json!({ "name": "rewind", "description": "Rewind conversation by N steps", "builtIn": true }),
        json!({ "name": "summarize", "description": "Summarize the current conversation", "builtIn": true }),
        json!({ "name": "analyze", "description": "Analyze conversation content", "builtIn": true }),
    ];

    // Custom commands from user-level and project-level
    let user_commands_dir = home.join(".claude").join("commands");
    let project_commands_dir = PathBuf::from(&body.project_path)
        .join(".claude")
        .join("commands");

    let mut custom = Vec::new();
    for dir in [&user_commands_dir, &project_commands_dir] {
        if let Ok(entries) = scan_command_dir(dir).await {
            custom.extend(entries);
        }
    }

    Ok(Json(json!({
        "builtIn": built_in,
        "custom": custom
    })))
}

/// POST /api/commands/load — Load command metadata and content.
pub async fn load_command(
    _auth: AuthUser,
    Json(body): Json<LoadCommandRequest>,
) -> Result<Json<Value>, AppError> {
    let path = PathBuf::from(&body.command_path);

    if !path.exists() {
        return Err(AppError::NotFound("Command file not found".into()));
    }

    let content = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;

    // Parse frontmatter metadata (---\n...\n--- block)
    let (metadata, body_content) = parse_frontmatter(&content);

    Ok(Json(json!({
        "path": body.command_path,
        "metadata": metadata,
        "content": body_content
    })))
}

/// POST /api/commands/execute — Execute a command with argument substitution.
pub async fn execute_command(
    _auth: AuthUser,
    Json(body): Json<ExecuteCommandRequest>,
) -> Result<Json<Value>, AppError> {
    // Handle built-in commands
    match body.command_name.as_str() {
        "rewind" => {
            let steps: u32 = body
                .args
                .first()
                .and_then(|s| s.parse().ok())
                .unwrap_or(1);
            return Ok(Json(json!({
                "type": "builtin",
                "action": "rewind",
                "data": { "steps": steps, "message": format!("Rewinding {steps} step(s)") }
            })));
        }
        "summarize" => {
            return Ok(Json(json!({
                "type": "builtin",
                "action": "summarize",
                "data": { "message": "Summarizing conversation..." }
            })));
        }
        "analyze" => {
            return Ok(Json(json!({
                "type": "builtin",
                "action": "analyze",
                "data": { "message": "Analyzing conversation content..." }
            })));
        }
        _ => {}
    }

    // Custom command: load and process
    let command_path = match body.command_path {
        Some(ref p) => PathBuf::from(p),
        None => {
            return Err(AppError::BadRequest(
                "commandPath is required for custom commands".into(),
            ))
        }
    };

    if !command_path.exists() {
        return Err(AppError::NotFound("Command file not found".into()));
    }

    let content = tokio::fs::read_to_string(&command_path)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;

    let (metadata, mut body_content) = parse_frontmatter(&content);

    // Perform argument substitution ($1, $2, etc.)
    for (i, arg) in body.args.iter().enumerate() {
        let placeholder = format!("${}", i + 1);
        body_content = body_content.replace(&placeholder, arg);
    }

    // Check for special patterns
    let has_file_includes = body_content.contains("@file:");
    let has_bash_commands = body_content.contains("```bash") || body_content.contains("$ ");

    Ok(Json(json!({
        "type": "custom",
        "command": body.command_name,
        "content": body_content,
        "metadata": metadata,
        "hasFileIncludes": has_file_includes,
        "hasBashCommands": has_bash_commands
    })))
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

async fn scan_command_dir(dir: &PathBuf) -> Result<Vec<Value>, std::io::Error> {
    let mut commands = Vec::new();

    if !dir.exists() {
        return Ok(commands);
    }

    let mut read_dir = tokio::fs::read_dir(dir).await?;
    while let Ok(Some(entry)) = read_dir.next_entry().await {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

        if ext != "md" && ext != "txt" {
            continue;
        }

        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();

        if name.is_empty() {
            continue;
        }

        // Quick scan for description from frontmatter
        let mut description = String::new();
        if let Ok(content) = tokio::fs::read_to_string(&path).await {
            let (meta, _) = parse_frontmatter(&content);
            if let Some(desc) = meta.get("description").and_then(|v| v.as_str()) {
                description = desc.to_string();
            }
        }

        commands.push(json!({
            "name": name,
            "path": path.to_string_lossy(),
            "description": description,
            "builtIn": false
        }));
    }

    Ok(commands)
}

/// Parse YAML frontmatter from a document (---\n...\n--- block).
fn parse_frontmatter(content: &str) -> (Value, String) {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return (json!({}), content.to_string());
    }

    // Find the closing ---
    if let Some(end) = trimmed[3..].find("\n---") {
        let frontmatter_str = &trimmed[3..end + 3].trim();
        let body = &trimmed[end + 7..]; // skip past \n---\n

        // Simple key: value parsing (not full YAML)
        let mut metadata = serde_json::Map::new();
        for line in frontmatter_str.lines() {
            if let Some((key, value)) = line.split_once(':') {
                let key = key.trim().to_string();
                let value = value.trim().to_string();
                metadata.insert(key, Value::String(value));
            }
        }

        (Value::Object(metadata), body.trim_start().to_string())
    } else {
        (json!({}), content.to_string())
    }
}
