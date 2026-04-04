use async_trait::async_trait;
use serde_json::Value;

use crate::error::AppError;
use crate::providers::ProviderAdapter;
use crate::providers::types::*;
use crate::services::project_scanner;

pub struct GeminiAdapter;

#[async_trait]
impl ProviderAdapter for GeminiAdapter {
    async fn fetch_history(
        &self,
        session_id: &str,
        _opts: FetchHistoryOptions,
    ) -> Result<FetchHistoryResult, AppError> {
        tracing::debug!(session_id, "GeminiAdapter: fetching history");
        // Read from Gemini CLI sessions on disk
        let raw_messages = match project_scanner::get_gemini_cli_session_messages(session_id).await
        {
            Ok(msgs) => msgs,
            Err(e) => {
                tracing::warn!("GeminiAdapter: Failed to load session {session_id}: {e}");
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

        let provider = SessionProvider::Gemini;
        let mut normalized = Vec::new();

        for raw in &raw_messages {
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

            // Determine role from either sessionManager format or CLI format
            let role = raw
                .get("message")
                .and_then(|m| m.get("role"))
                .or(raw.get("role"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let content = raw
                .get("message")
                .and_then(|m| m.get("content"))
                .or(raw.get("content"));

            if role.is_empty() || content.is_none() {
                continue;
            }

            let normalized_role = if role == "user" { "user" } else { "assistant" };

            match content {
                Some(Value::Array(parts)) => {
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
                                        normalized.push(m);
                                    }
                                }
                            }
                            "tool_use" => {
                                let mut m = NormalizedMessage::new(
                                    MessageKind::ToolUse,
                                    provider,
                                    session_id,
                                );
                                m.id = format!("{base_id}_{part_idx}");
                                m.timestamp = ts.clone();
                                m.tool_name = part
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string());
                                m.tool_input = part.get("input").cloned();
                                m.tool_id = Some(
                                    part.get("id")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string(),
                                );
                                normalized.push(m);
                            }
                            "tool_result" => {
                                let mut m = NormalizedMessage::new(
                                    MessageKind::ToolResult,
                                    provider,
                                    session_id,
                                );
                                m.id = format!("{base_id}_{part_idx}");
                                m.timestamp = ts.clone();
                                m.tool_id = part
                                    .get("tool_use_id")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string());
                                m.content = part.get("content").map(|v| {
                                    if let Some(s) = v.as_str() {
                                        s.to_string()
                                    } else {
                                        v.to_string()
                                    }
                                });
                                m.is_error = Some(
                                    part.get("is_error")
                                        .and_then(|v| v.as_bool())
                                        .unwrap_or(false),
                                );
                                normalized.push(m);
                            }
                            _ => {}
                        }
                    }
                }
                Some(Value::String(text)) => {
                    if !text.trim().is_empty() {
                        let mut m =
                            NormalizedMessage::new(MessageKind::Text, provider, session_id);
                        m.id = base_id.clone();
                        m.timestamp = ts.clone();
                        m.role = Some(normalized_role.to_string());
                        m.content = Some(text.clone());
                        normalized.push(m);
                    }
                }
                _ => {}
            }
        }

        let total = normalized.len();
        tracing::debug!(session_id, total, "GeminiAdapter: history fetched");
        Ok(FetchHistoryResult {
            messages: normalized,
            total,
            has_more: false,
            offset: 0,
            limit: None,
            token_usage: None,
        })
    }

    fn normalize_message(&self, raw: &Value, session_id: &str) -> Vec<NormalizedMessage> {
        let provider = SessionProvider::Gemini;
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

        let raw_type = raw.get("type").and_then(|v| v.as_str()).unwrap_or("");

        match raw_type {
            "message" => {
                let role = raw.get("role").and_then(|v| v.as_str()).unwrap_or("");
                if role != "assistant" {
                    return vec![];
                }
                let content = raw
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let mut msgs = Vec::new();
                if !content.is_empty() {
                    let mut m =
                        NormalizedMessage::new(MessageKind::StreamDelta, provider, session_id);
                    m.id = base_id;
                    m.timestamp = ts.clone();
                    m.content = Some(content);
                    msgs.push(m);
                }
                let is_delta = raw.get("delta").and_then(|v| v.as_bool()).unwrap_or(false);
                if !is_delta {
                    let mut m =
                        NormalizedMessage::new(MessageKind::StreamEnd, provider, session_id);
                    m.timestamp = ts;
                    msgs.push(m);
                }
                msgs
            }
            "tool_use" => {
                let mut m = NormalizedMessage::new(MessageKind::ToolUse, provider, session_id);
                m.id = base_id.clone();
                m.timestamp = ts;
                m.tool_name = raw
                    .get("tool_name")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                m.tool_input = raw.get("parameters").cloned().or(Some(serde_json::json!({})));
                m.tool_id = Some(
                    raw.get("tool_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or(&base_id)
                        .to_string(),
                );
                vec![m]
            }
            "tool_result" => {
                let mut m =
                    NormalizedMessage::new(MessageKind::ToolResult, provider, session_id);
                m.id = base_id;
                m.timestamp = ts;
                m.tool_id = raw
                    .get("tool_id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                m.content = raw.get("output").map(|v| {
                    if let Some(s) = v.as_str() {
                        s.to_string()
                    } else {
                        v.to_string()
                    }
                });
                m.is_error = Some(
                    raw.get("status").and_then(|v| v.as_str()) == Some("error"),
                );
                vec![m]
            }
            "result" => {
                let mut msgs = vec![NormalizedMessage::new(
                    MessageKind::StreamEnd,
                    provider,
                    session_id,
                )];
                msgs[0].timestamp = ts.clone();
                if let Some(total_tokens) = raw
                    .get("stats")
                    .and_then(|s| s.get("total_tokens"))
                    .and_then(|v| v.as_u64())
                {
                    let mut m =
                        NormalizedMessage::new(MessageKind::Status, provider, session_id);
                    m.timestamp = ts;
                    m.text = Some("Complete".to_string());
                    m.tokens = Some(total_tokens);
                    m.can_interrupt = Some(false);
                    msgs.push(m);
                }
                msgs
            }
            "error" => {
                let content = raw
                    .get("error")
                    .or(raw.get("message"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("Unknown Gemini streaming error")
                    .to_string();
                let mut m = NormalizedMessage::new(MessageKind::Error, provider, session_id);
                m.id = base_id;
                m.timestamp = ts;
                m.content = Some(content);
                vec![m]
            }
            _ => vec![],
        }
    }
}
