use async_trait::async_trait;
use serde_json::Value;

use crate::error::AppError;
use crate::providers::ProviderAdapter;
use crate::providers::types::*;
use crate::services::project_scanner;

pub struct CodexAdapter;

/// Normalize a raw Codex history entry (has message.role) into NormalizedMessages.
fn normalize_codex_history_entry(raw: &Value, session_id: &str) -> Vec<NormalizedMessage> {
    let provider = SessionProvider::Codex;
    let ts = raw
        .get("timestamp")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let base_id = raw
        .get("uuid")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| generate_message_id(&MessageKind::Text));

    // User message
    if let Some(msg) = raw.get("message") {
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");

        if role == "user" {
            let content = extract_content_text(msg.get("content"));
            if content.trim().is_empty() {
                return vec![];
            }
            let mut m = NormalizedMessage::new(MessageKind::Text, provider, session_id);
            m.id = base_id;
            m.timestamp = ts;
            m.role = Some("user".to_string());
            m.content = Some(content);
            return vec![m];
        }

        if role == "assistant" {
            let content = extract_content_text(msg.get("content"));
            if content.trim().is_empty() {
                return vec![];
            }
            let mut m = NormalizedMessage::new(MessageKind::Text, provider, session_id);
            m.id = base_id;
            m.timestamp = ts;
            m.role = Some("assistant".to_string());
            m.content = Some(content);
            return vec![m];
        }
    }

    // Thinking/reasoning
    let raw_type = raw.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let is_reasoning = raw.get("isReasoning").and_then(|v| v.as_bool()).unwrap_or(false);

    if raw_type == "thinking" || is_reasoning {
        let content = raw
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let mut m = NormalizedMessage::new(MessageKind::Thinking, provider, session_id);
        m.id = base_id;
        m.timestamp = ts;
        m.content = Some(content);
        return vec![m];
    }

    // Tool use
    if raw_type == "tool_use" || raw.get("toolName").is_some() {
        let tool_name = raw
            .get("toolName")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown")
            .to_string();
        let mut m = NormalizedMessage::new(MessageKind::ToolUse, provider, session_id);
        m.id = base_id.clone();
        m.timestamp = ts;
        m.tool_name = Some(tool_name);
        m.tool_input = raw.get("toolInput").cloned();
        m.tool_id = Some(
            raw.get("toolCallId")
                .and_then(|v| v.as_str())
                .unwrap_or(&base_id)
                .to_string(),
        );
        return vec![m];
    }

    // Tool result
    if raw_type == "tool_result" {
        let mut m = NormalizedMessage::new(MessageKind::ToolResult, provider, session_id);
        m.id = base_id;
        m.timestamp = ts;
        m.tool_id = raw
            .get("toolCallId")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        m.content = raw
            .get("output")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        m.is_error = Some(raw.get("isError").and_then(|v| v.as_bool()).unwrap_or(false));
        return vec![m];
    }

    vec![]
}

fn extract_content_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
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
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

#[async_trait]
impl ProviderAdapter for CodexAdapter {
    async fn fetch_history(
        &self,
        session_id: &str,
        opts: FetchHistoryOptions,
    ) -> Result<FetchHistoryResult, AppError> {
        let (raw_messages, total, has_more) = match project_scanner::get_codex_session_messages(
            session_id,
            opts.limit,
            opts.offset,
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("CodexAdapter: Failed to load session {session_id}: {e}");
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

        let mut normalized = Vec::new();
        for raw in &raw_messages {
            let entries = normalize_codex_history_entry(raw, session_id);
            normalized.extend(entries);
        }

        tracing::debug!(session_id, total, message_count = normalized.len(), has_more, "CodexAdapter: history fetched");
        Ok(FetchHistoryResult {
            messages: normalized,
            total,
            has_more,
            offset: opts.offset,
            limit: opts.limit,
            token_usage: None,
        })
    }

    fn normalize_message(&self, raw: &Value, session_id: &str) -> Vec<NormalizedMessage> {
        let provider = SessionProvider::Codex;
        let ts = raw
            .get("timestamp")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let base_id = raw
            .get("uuid")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| generate_message_id(&MessageKind::Text));

        // History format
        if raw.get("message").and_then(|m| m.get("role")).is_some() {
            return normalize_codex_history_entry(raw, session_id);
        }

        // SDK event format
        let raw_type = raw.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let item_type = raw.get("itemType").and_then(|v| v.as_str()).unwrap_or("");

        if raw_type == "item" {
            match item_type {
                "agent_message" => {
                    let content = raw
                        .get("message")
                        .and_then(|m| m.get("content"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let mut m = NormalizedMessage::new(MessageKind::Text, provider, session_id);
                    m.id = base_id;
                    m.timestamp = ts;
                    m.role = Some("assistant".to_string());
                    m.content = Some(content);
                    return vec![m];
                }
                "reasoning" => {
                    let content = raw
                        .get("message")
                        .and_then(|m| m.get("content"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let mut m =
                        NormalizedMessage::new(MessageKind::Thinking, provider, session_id);
                    m.id = base_id;
                    m.timestamp = ts;
                    m.content = Some(content);
                    return vec![m];
                }
                "command_execution" => {
                    let mut m =
                        NormalizedMessage::new(MessageKind::ToolUse, provider, session_id);
                    m.id = base_id.clone();
                    m.timestamp = ts;
                    m.tool_name = Some("Bash".to_string());
                    m.tool_input = Some(serde_json::json!({
                        "command": raw.get("command").and_then(|v| v.as_str()).unwrap_or("")
                    }));
                    m.tool_id = Some(base_id);
                    return vec![m];
                }
                "file_change" => {
                    let mut m =
                        NormalizedMessage::new(MessageKind::ToolUse, provider, session_id);
                    m.id = base_id.clone();
                    m.timestamp = ts;
                    m.tool_name = Some("FileChanges".to_string());
                    m.tool_input = raw.get("changes").cloned();
                    m.tool_id = Some(base_id);
                    return vec![m];
                }
                "error" => {
                    let content = raw
                        .get("message")
                        .and_then(|m| m.get("content"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("Unknown error")
                        .to_string();
                    let mut m = NormalizedMessage::new(MessageKind::Error, provider, session_id);
                    m.id = base_id;
                    m.timestamp = ts;
                    m.content = Some(content);
                    return vec![m];
                }
                _ => {
                    let mut m =
                        NormalizedMessage::new(MessageKind::ToolUse, provider, session_id);
                    m.id = base_id.clone();
                    m.timestamp = ts;
                    m.tool_name = Some(item_type.to_string());
                    m.tool_input = raw.get("item").or(Some(raw)).cloned();
                    m.tool_id = Some(base_id);
                    return vec![m];
                }
            }
        }

        if raw_type == "turn_complete" {
            let mut m = NormalizedMessage::new(MessageKind::Complete, provider, session_id);
            m.id = base_id;
            m.timestamp = ts;
            return vec![m];
        }

        if raw_type == "turn_failed" {
            let content = raw
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|v| v.as_str())
                .unwrap_or("Turn failed")
                .to_string();
            let mut m = NormalizedMessage::new(MessageKind::Error, provider, session_id);
            m.id = base_id;
            m.timestamp = ts;
            m.content = Some(content);
            return vec![m];
        }

        vec![]
    }
}
