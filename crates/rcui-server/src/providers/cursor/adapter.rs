use async_trait::async_trait;
use md5::{Digest, Md5};
use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::collections::HashMap;
use std::str::FromStr;

use crate::error::AppError;
use crate::providers::ProviderAdapter;
use crate::providers::types::*;

pub struct CursorAdapter;

/// Load raw JSON blobs from Cursor's SQLite store.db, parse the DAG structure,
/// and return sorted message blobs in chronological order.
async fn load_cursor_blobs(
    session_id: &str,
    project_path: &str,
) -> Result<Vec<CursorBlob>, AppError> {
    let cwd_id = format!("{:x}", Md5::digest(project_path.as_bytes()));
    let home = dirs::home_dir().unwrap_or_default();
    let store_db_path = home
        .join(".cursor")
        .join("chats")
        .join(&cwd_id)
        .join(session_id)
        .join("store.db");

    if !store_db_path.exists() {
        return Ok(vec![]);
    }

    let db_url = format!("sqlite://{}?mode=ro", store_db_path.display());
    let opts = SqliteConnectOptions::from_str(&db_url)
        .map_err(|e| AppError::Internal(e.into()))?
        .read_only(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;

    // Read all blobs
    let rows: Vec<(i64, String, Vec<u8>)> =
        sqlx::query_as("SELECT rowid, id, data FROM blobs")
            .fetch_all(&pool)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;

    pool.close().await;

    let mut blob_map: HashMap<String, (i64, Vec<u8>)> = HashMap::new();
    let mut json_blobs: Vec<(String, i64, Value)> = Vec::new();
    let mut parent_refs: HashMap<String, Vec<String>> = HashMap::new();

    for (rowid, id, data) in &rows {
        blob_map.insert(id.clone(), (*rowid, data.clone()));

        if !data.is_empty() && data[0] == 0x7B {
            // JSON blob (starts with '{')
            if let Ok(parsed) = serde_json::from_slice::<Value>(data) {
                json_blobs.push((id.clone(), *rowid, parsed));
            }
        } else if !data.is_empty() {
            // Binary blob — try to extract parent references
            let mut parents = Vec::new();
            let mut i = 0;
            while i + 34 <= data.len() {
                if data[i] == 0x0A && data[i + 1] == 0x20 {
                    let parent_hash = hex::encode(&data[i + 2..i + 34]);
                    if blob_map.contains_key(&parent_hash) {
                        parents.push(parent_hash);
                    }
                    i += 34;
                } else {
                    i += 1;
                }
            }
            if !parents.is_empty() {
                parent_refs.insert(id.clone(), parents);
            }
        }
    }

    // Topological sort (DFS)
    let mut visited = std::collections::HashSet::new();
    let mut sorted: Vec<String> = Vec::new();

    fn visit(
        node_id: &str,
        parent_refs: &HashMap<String, Vec<String>>,
        visited: &mut std::collections::HashSet<String>,
        sorted: &mut Vec<String>,
    ) {
        if visited.contains(node_id) {
            return;
        }
        visited.insert(node_id.to_string());
        if let Some(parents) = parent_refs.get(node_id) {
            for pid in parents {
                visit(pid, parent_refs, visited, sorted);
            }
        }
        sorted.push(node_id.to_string());
    }

    // Visit roots first (nodes without parents)
    for (_, id, _) in &rows {
        if !parent_refs.contains_key(id) {
            visit(id, &parent_refs, &mut visited, &mut sorted);
        }
    }
    // Then visit any remaining
    for (_, id, _) in &rows {
        visit(id, &parent_refs, &mut visited, &mut sorted);
    }

    // Order JSON blobs by DAG appearance
    let mut message_order: HashMap<String, usize> = HashMap::new();
    let mut order_idx = 0usize;

    for blob_id in &sorted {
        if let Some((_, data)) = blob_map.get(blob_id) {
            if data.is_empty() || data[0] == 0x7B {
                continue;
            }
            for (jb_id, _, _) in &json_blobs {
                if let Ok(id_bytes) = hex::decode(jb_id) {
                    if data.windows(id_bytes.len()).any(|w| w == id_bytes.as_slice())
                        && !message_order.contains_key(jb_id)
                    {
                        message_order.insert(jb_id.clone(), order_idx);
                        order_idx += 1;
                    }
                }
            }
        }
    }

    json_blobs.sort_by(|a, b| {
        let oa = message_order.get(&a.0).copied().unwrap_or(usize::MAX);
        let ob = message_order.get(&b.0).copied().unwrap_or(usize::MAX);
        if oa != ob {
            oa.cmp(&ob)
        } else {
            a.1.cmp(&b.1)
        }
    });

    // Convert to CursorBlob, skipping system messages
    let mut blobs = Vec::new();
    for (idx, (id, rowid, parsed)) in json_blobs.into_iter().enumerate() {
        let role = parsed
            .get("role")
            .or_else(|| parsed.get("message").and_then(|m| m.get("role")))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if role == "system" {
            continue;
        }

        blobs.push(CursorBlob {
            id,
            sequence: idx + 1,
            rowid,
            content: parsed,
        });
    }

    Ok(blobs)
}

struct CursorBlob {
    id: String,
    sequence: usize,
    rowid: i64,
    content: Value,
}

/// Normalize cursor blobs into NormalizedMessages.
fn normalize_cursor_blobs(blobs: &[CursorBlob], session_id: &str) -> Vec<NormalizedMessage> {
    let mut messages = Vec::new();
    let provider = SessionProvider::Cursor;
    let base_time = chrono::Utc::now().timestamp_millis();

    for blob in blobs {
        let ts = chrono::DateTime::from_timestamp_millis(base_time + (blob.sequence as i64) * 100)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_default();
        let base_id = &blob.id;
        let content = &blob.content;

        // Check for top-level role+content
        let has_role = content.get("role").and_then(|v| v.as_str()).is_some();
        let has_content = content.get("content").is_some();

        if !has_role || !has_content {
            // Try nested message format
            if let Some(msg) = content.get("message") {
                let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
                if role == "system" {
                    continue;
                }
                let normalized_role = if role == "user" { "user" } else { "assistant" };
                let text = extract_text_content(msg.get("content"));
                if !text.is_empty() {
                    let mut m = NormalizedMessage::new(MessageKind::Text, provider, session_id);
                    m.id = base_id.clone();
                    m.timestamp = ts.clone();
                    m.role = Some(normalized_role.to_string());
                    m.content = Some(text);
                    messages.push(m);
                }
            }
            continue;
        }

        let role_str = content.get("role").and_then(|v| v.as_str()).unwrap_or("");
        if role_str == "system" {
            continue;
        }

        // Tool results (role = "tool")
        if role_str == "tool" {
            if let Some(items) = content.get("content").and_then(|c| c.as_array()) {
                for item in items {
                    if item.get("type").and_then(|v| v.as_str()) != Some("tool-result") {
                        continue;
                    }
                    let tool_call_id = item
                        .get("toolCallId")
                        .or(content.get("id"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let result = item
                        .get("result")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();

                    let mut m =
                        NormalizedMessage::new(MessageKind::ToolResult, provider, session_id);
                    m.id = format!("{base_id}_tr");
                    m.timestamp = ts.clone();
                    m.tool_id = Some(tool_call_id.to_string());
                    m.content = Some(result);
                    m.is_error = Some(false);
                    messages.push(m);
                }
            }
            continue;
        }

        let normalized_role = if role_str == "user" {
            "user"
        } else {
            "assistant"
        };

        if let Some(parts) = content.get("content").and_then(|c| c.as_array()) {
            for (part_idx, part) in parts.iter().enumerate() {
                let part_type = part.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match part_type {
                    "text" => {
                        if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                            if !text.is_empty() {
                                let mut m = NormalizedMessage::new(
                                    MessageKind::Text,
                                    provider,
                                    session_id,
                                );
                                m.id = format!("{base_id}_{part_idx}");
                                m.timestamp = ts.clone();
                                m.role = Some(normalized_role.to_string());
                                m.content = Some(text.to_string());
                                messages.push(m);
                            }
                        }
                    }
                    "reasoning" => {
                        if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                            if !text.is_empty() {
                                let mut m = NormalizedMessage::new(
                                    MessageKind::Thinking,
                                    provider,
                                    session_id,
                                );
                                m.id = format!("{base_id}_{part_idx}");
                                m.timestamp = ts.clone();
                                m.content = Some(text.to_string());
                                messages.push(m);
                            }
                        }
                    }
                    "tool-call" | "tool_use" => {
                        let tool_name_raw = part
                            .get("toolName")
                            .or(part.get("name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("Unknown Tool");
                        let tool_name = if tool_name_raw == "ApplyPatch" {
                            "Edit"
                        } else {
                            tool_name_raw
                        };
                        let tool_id = part
                            .get("toolCallId")
                            .or(part.get("id"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();

                        let mut m =
                            NormalizedMessage::new(MessageKind::ToolUse, provider, session_id);
                        m.id = format!("{base_id}_{part_idx}");
                        m.timestamp = ts.clone();
                        m.tool_name = Some(tool_name.to_string());
                        m.tool_input = part.get("args").or(part.get("input")).cloned();
                        m.tool_id = Some(if tool_id.is_empty() {
                            format!("tool_{}", part_idx)
                        } else {
                            tool_id
                        });
                        messages.push(m);
                    }
                    _ => {}
                }
            }
        } else if let Some(text) = content.get("content").and_then(|c| c.as_str()) {
            if !text.trim().is_empty() {
                let mut m = NormalizedMessage::new(MessageKind::Text, provider, session_id);
                m.id = base_id.clone();
                m.timestamp = ts.clone();
                m.role = Some(normalized_role.to_string());
                m.content = Some(text.to_string());
                messages.push(m);
            }
        }
    }

    messages
}

fn extract_text_content(content: Option<&Value>) -> String {
    match content {
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|p| {
                if p.is_string() {
                    p.as_str().map(|s| s.to_string())
                } else {
                    p.get("text").and_then(|v| v.as_str()).map(|s| s.to_string())
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

#[async_trait]
impl ProviderAdapter for CursorAdapter {
    async fn fetch_history(
        &self,
        session_id: &str,
        opts: FetchHistoryOptions,
    ) -> Result<FetchHistoryResult, AppError> {
        let project_path = opts.project_path.as_deref().unwrap_or("");

        let blobs = match load_cursor_blobs(session_id, project_path).await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("CursorAdapter: Failed to load session {session_id}: {e}");
                return Ok(FetchHistoryResult {
                    messages: vec![],
                    total: 0,
                    has_more: false,
                    offset: 0,
                    limit: None,
                    token_usage: None,
                });
            }
        };

        let all_normalized = normalize_cursor_blobs(&blobs, session_id);
        let total = all_normalized.len();
        tracing::debug!(session_id, blob_count = blobs.len(), total, "CursorAdapter: history normalized");

        if let Some(limit) = opts.limit {
            let start = opts.offset as usize;
            let end = (start + limit as usize).min(total);
            let page = if start < total {
                all_normalized[start..end].to_vec()
            } else {
                vec![]
            };
            Ok(FetchHistoryResult {
                messages: page,
                total,
                has_more: end < total,
                offset: opts.offset,
                limit: Some(limit),
                token_usage: None,
            })
        } else {
            Ok(FetchHistoryResult {
                messages: all_normalized,
                total,
                has_more: false,
                offset: 0,
                limit: None,
                token_usage: None,
            })
        }
    }

    fn normalize_message(&self, raw: &Value, session_id: &str) -> Vec<NormalizedMessage> {
        // For real-time streaming normalization
        let provider = SessionProvider::Cursor;

        if raw.get("type").and_then(|v| v.as_str()) == Some("assistant") {
            if let Some(text) = raw
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array())
                .and_then(|arr| arr.first())
                .and_then(|p| p.get("text"))
                .and_then(|v| v.as_str())
            {
                let mut m =
                    NormalizedMessage::new(MessageKind::StreamDelta, provider, session_id);
                m.content = Some(text.to_string());
                return vec![m];
            }
        }

        if let Some(text) = raw.as_str() {
            if !text.trim().is_empty() {
                let mut m =
                    NormalizedMessage::new(MessageKind::StreamDelta, provider, session_id);
                m.content = Some(text.to_string());
                return vec![m];
            }
        }

        vec![]
    }
}
