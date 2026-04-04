use serde::{Deserialize, Serialize};
use serde_json::Value;

// ─── Session Provider ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionProvider {
    Claude,
    Cursor,
    Codex,
    Gemini,
}

impl std::fmt::Display for SessionProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionProvider::Claude => write!(f, "claude"),
            SessionProvider::Cursor => write!(f, "cursor"),
            SessionProvider::Codex => write!(f, "codex"),
            SessionProvider::Gemini => write!(f, "gemini"),
        }
    }
}

impl std::str::FromStr for SessionProvider {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "claude" => Ok(SessionProvider::Claude),
            "cursor" => Ok(SessionProvider::Cursor),
            "codex" => Ok(SessionProvider::Codex),
            "gemini" => Ok(SessionProvider::Gemini),
            _ => Err(format!("Unknown provider: {s}")),
        }
    }
}

// ─── Message Kind ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    Text,
    ToolUse,
    ToolResult,
    Thinking,
    StreamDelta,
    StreamEnd,
    Error,
    Complete,
    Status,
    PermissionRequest,
    PermissionCancelled,
    SessionCreated,
    InteractivePrompt,
    TaskNotification,
}

// ─── NormalizedMessage ───────────────────────────────────────────────────────

/// Normalized message format shared by all providers.
/// Uses flat struct with optional fields to match the JavaScript frontend exactly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizedMessage {
    pub id: String,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub timestamp: String,
    pub provider: SessionProvider,
    pub kind: MessageKind,

    // text / tool_result / thinking / stream_delta / error / interactive_prompt
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,

    // tool_use
    #[serde(rename = "toolName", skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(rename = "toolInput", skip_serializing_if = "Option::is_none")]
    pub tool_input: Option<Value>,
    #[serde(rename = "toolId", skip_serializing_if = "Option::is_none")]
    pub tool_id: Option<String>,

    // tool_result
    #[serde(rename = "isError", skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,

    // permission_request / permission_cancelled
    #[serde(rename = "requestId", skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,

    // session_created
    #[serde(rename = "newSessionId", skip_serializing_if = "Option::is_none")]
    pub new_session_id: Option<String>,

    // status
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    #[serde(rename = "canInterrupt", skip_serializing_if = "Option::is_none")]
    pub can_interrupt: Option<bool>,
    #[serde(rename = "tokenBudget", skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<u64>,

    // complete
    #[serde(rename = "exitCode", skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(rename = "resultText", skip_serializing_if = "Option::is_none")]
    pub result_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aborted: Option<bool>,
    #[serde(rename = "actualSessionId", skip_serializing_if = "Option::is_none")]
    pub actual_session_id: Option<String>,

    // images (for text messages with attachments)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<ImageData>>,

    // task_notification
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageData {
    pub name: String,
    pub data: String,
    #[serde(rename = "mimeType")]
    pub mime_type: String,
}

impl NormalizedMessage {
    /// Create a new NormalizedMessage with common fields pre-filled.
    pub fn new(kind: MessageKind, provider: SessionProvider, session_id: &str) -> Self {
        Self {
            id: generate_message_id(&kind),
            session_id: session_id.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            provider,
            kind,
            role: None,
            content: None,
            tool_name: None,
            tool_input: None,
            tool_id: None,
            is_error: None,
            request_id: None,
            input: None,
            context: None,
            new_session_id: None,
            text: None,
            tokens: None,
            can_interrupt: None,
            token_budget: None,
            exit_code: None,
            result_text: None,
            aborted: None,
            actual_session_id: None,
            images: None,
            status: None,
            summary: None,
        }
    }
}

/// Generate a unique message ID with a kind-based prefix.
pub fn generate_message_id(kind: &MessageKind) -> String {
    let prefix = match kind {
        MessageKind::Text => "text",
        MessageKind::ToolUse => "tool_use",
        MessageKind::ToolResult => "tool_result",
        MessageKind::Thinking => "thinking",
        MessageKind::StreamDelta => "delta",
        MessageKind::StreamEnd => "end",
        MessageKind::Error => "error",
        MessageKind::Complete => "complete",
        MessageKind::Status => "status",
        MessageKind::PermissionRequest => "perm_req",
        MessageKind::PermissionCancelled => "perm_cancel",
        MessageKind::SessionCreated => "session",
        MessageKind::InteractivePrompt => "prompt",
        MessageKind::TaskNotification => "task",
    };
    format!("{}_{}", prefix, uuid::Uuid::new_v4())
}

// ─── Fetch History ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct FetchHistoryOptions {
    pub project_name: Option<String>,
    pub project_path: Option<String>,
    pub limit: Option<u32>,
    pub offset: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct FetchHistoryResult {
    pub messages: Vec<NormalizedMessage>,
    pub total: usize,
    #[serde(rename = "hasMore")]
    pub has_more: bool,
    pub offset: u32,
    pub limit: Option<u32>,
    #[serde(rename = "tokenUsage", skip_serializing_if = "Option::is_none")]
    pub token_usage: Option<Value>,
}
