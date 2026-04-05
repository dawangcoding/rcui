use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::io::AsyncBufReadExt;

use crate::error::AppError;
use crate::providers::ProviderAdapter;
use crate::providers::types::*;
use crate::providers::utils::is_internal_content;

pub struct ClaudeAdapter;

/// Extract token usage from raw JSONL messages.
/// Scans from the end to find the latest assistant message with usage data.
/// Returns `{ used, total, breakdown }` matching the original Node.js format.
pub fn extract_token_usage(raw_messages: &[Value]) -> Option<Value> {
    let context_window: u64 = std::env::var("CONTEXT_WINDOW")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(160000);

    for raw in raw_messages.iter().rev() {
        let msg_type = raw.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if msg_type != "assistant" {
            continue;
        }
        let usage = match raw.get("message").and_then(|m| m.get("usage")) {
            Some(u) => u,
            None => continue,
        };

        let input_tokens = usage
            .get("input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let cache_creation = usage
            .get("cache_creation_input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let cache_read = usage
            .get("cache_read_input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        let total_used = input_tokens + cache_creation + cache_read;

        return Some(json!({
            "used": total_used,
            "total": context_window,
            "breakdown": {
                "input": input_tokens,
                "cacheCreation": cache_creation,
                "cacheRead": cache_read
            }
        }));
    }

    None
}

#[async_trait]
impl ProviderAdapter for ClaudeAdapter {
    async fn fetch_history(
        &self,
        session_id: &str,
        opts: FetchHistoryOptions,
    ) -> Result<FetchHistoryResult, AppError> {
        let project_name = match &opts.project_name {
            Some(name) => name.clone(),
            None => {
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

        // Find the JSONL file for this session
        let home = dirs::home_dir().unwrap_or_default();
        let projects_dir = home.join(".claude").join("projects");
        let session_file = projects_dir.join(&project_name).join(format!("{session_id}.jsonl"));

        if !session_file.exists() {
            return Ok(FetchHistoryResult {
                messages: vec![],
                total: 0,
                has_more: false,
                offset: 0,
                limit: None,
                token_usage: None,
            });
        }

        // Read and parse the JSONL file
        let file = tokio::fs::File::open(&session_file).await?;
        let reader = tokio::io::BufReader::new(file);
        let mut lines = reader.lines();
        let mut raw_messages: Vec<Value> = Vec::new();

        while let Some(line) = lines.next_line().await? {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
                raw_messages.push(parsed);
            }
        }

        // Extract token usage from the latest assistant message (scan from end).
        // Claude's usage.input_tokens represents the full conversation context for
        // that turn, so the last assistant message gives us current context usage.
        let token_usage = extract_token_usage(&raw_messages);

        // Normalize ALL messages first, then paginate the normalized result.
        // Raw JSONL entries include non-message items (queue-operation, last-prompt,
        // redacted_thinking) that produce no output. Paginating raw entries causes
        // pages with far fewer visible messages than requested and inconsistent
        // offset tracking between frontend and backend.
        let mut all_normalized = Vec::new();
        for raw in &raw_messages {
            let entries = self.normalize_message(raw, session_id);
            all_normalized.extend(entries);
        }

        let total = all_normalized.len();

        // Paginate from the END so the initial load (offset=0) returns the most
        // recent messages — the natural expectation for a chat UI.
        // offset=0  → newest messages
        // offset=20 → next 20 older messages, etc.
        let (normalized, has_more) = if let Some(limit) = opts.limit {
            let limit = limit as usize;
            let offset = opts.offset as usize;
            if offset >= total {
                (vec![], false)
            } else {
                let available = total - offset;
                let take = available.min(limit);
                let start = total - offset - take;
                let end = total - offset;
                (all_normalized[start..end].to_vec(), (offset + take) < total)
            }
        } else {
            (all_normalized, false)
        };

        tracing::debug!(session_id, total, message_count = normalized.len(), has_more, "ClaudeAdapter: history fetched");
        Ok(FetchHistoryResult {
            messages: normalized,
            total,
            has_more,
            offset: opts.offset,
            limit: opts.limit,
            token_usage,
        })
    }

    fn normalize_message(&self, raw: &Value, session_id: &str) -> Vec<NormalizedMessage> {
        let mut messages = Vec::new();
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

        let provider = SessionProvider::Claude;

        // User message
        if let Some(msg) = raw.get("message") {
            let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
            let content = msg.get("content");

            if role == "user" {
                if let Some(content_arr) = content.and_then(|c| c.as_array()) {
                    for part in content_arr {
                        let part_type = part.get("type").and_then(|v| v.as_str()).unwrap_or("");
                        match part_type {
                            "tool_result" => {
                                let tool_id = part
                                    .get("tool_use_id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                let content_str = match part.get("content") {
                                    Some(Value::String(s)) => s.clone(),
                                    Some(v) => v.to_string(),
                                    None => String::new(),
                                };
                                let is_error = part
                                    .get("is_error")
                                    .and_then(|v| v.as_bool())
                                    .unwrap_or(false);
                                let mut m = NormalizedMessage::new(
                                    MessageKind::ToolResult,
                                    provider,
                                    session_id,
                                );
                                m.id = format!("{base_id}_tr_{tool_id}");
                                m.timestamp = ts.clone();
                                m.tool_id = Some(tool_id.to_string());
                                m.content = Some(content_str);
                                m.is_error = Some(is_error);
                                messages.push(m);
                            }
                            "text" => {
                                let text = part
                                    .get("text")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                if !text.is_empty() && !is_internal_content(text) {
                                    let mut m = NormalizedMessage::new(
                                        MessageKind::Text,
                                        provider,
                                        session_id,
                                    );
                                    m.id = format!("{base_id}_text");
                                    m.timestamp = ts.clone();
                                    m.role = Some("user".to_string());
                                    m.content = Some(text.to_string());
                                    messages.push(m);
                                }
                            }
                            _ => {}
                        }
                    }
                } else if let Some(text) = content.and_then(|c| c.as_str()) {
                    if !text.is_empty() && !is_internal_content(text) {
                        let mut m =
                            NormalizedMessage::new(MessageKind::Text, provider, session_id);
                        m.id = base_id.clone();
                        m.timestamp = ts.clone();
                        m.role = Some("user".to_string());
                        m.content = Some(text.to_string());
                        messages.push(m);
                    }
                }
                return messages;
            }

            // Assistant message
            if role == "assistant" {
                if let Some(content_arr) = content.and_then(|c| c.as_array()) {
                    for (i, part) in content_arr.iter().enumerate() {
                        let part_type = part.get("type").and_then(|v| v.as_str()).unwrap_or("");
                        match part_type {
                            "text" => {
                                let text = part
                                    .get("text")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                if !text.is_empty() {
                                    let mut m = NormalizedMessage::new(
                                        MessageKind::Text,
                                        provider,
                                        session_id,
                                    );
                                    m.id = format!("{base_id}_{i}");
                                    m.timestamp = ts.clone();
                                    m.role = Some("assistant".to_string());
                                    m.content = Some(text.to_string());
                                    messages.push(m);
                                }
                            }
                            "tool_use" => {
                                let mut m = NormalizedMessage::new(
                                    MessageKind::ToolUse,
                                    provider,
                                    session_id,
                                );
                                m.id = format!("{base_id}_{i}");
                                m.timestamp = ts.clone();
                                m.tool_name = part
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string());
                                m.tool_input = part.get("input").cloned();
                                m.tool_id = part
                                    .get("id")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string());
                                messages.push(m);
                            }
                            "thinking" => {
                                let thinking = part
                                    .get("thinking")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                if !thinking.is_empty() {
                                    let mut m = NormalizedMessage::new(
                                        MessageKind::Thinking,
                                        provider,
                                        session_id,
                                    );
                                    m.id = format!("{base_id}_{i}");
                                    m.timestamp = ts.clone();
                                    m.content = Some(thinking.to_string());
                                    messages.push(m);
                                }
                            }
                            _ => {}
                        }
                    }
                } else if let Some(text) = content.and_then(|c| c.as_str()) {
                    let mut m =
                        NormalizedMessage::new(MessageKind::Text, provider, session_id);
                    m.id = base_id.clone();
                    m.timestamp = ts.clone();
                    m.role = Some("assistant".to_string());
                    m.content = Some(text.to_string());
                    messages.push(m);
                }
                return messages;
            }
        }

        messages
    }
}
