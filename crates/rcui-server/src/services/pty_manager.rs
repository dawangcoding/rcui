use std::collections::VecDeque;
use std::io::{Read, Write};

use base64::Engine;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use regex::Regex;
use std::sync::LazyLock;
use tracing::{debug, info};

use crate::error::AppError;
use crate::state::PTY_BUFFER_CAP;

// ─── Session Key ─────────────────────────────────────────────────────────────

/// Compute a deterministic key for PTY session reuse / reconnection.
///
/// Format: `{project_path}_{session_id|default}[_cmd_{base64_prefix}]`
pub fn compute_session_key(
    project_path: &str,
    session_id: Option<&str>,
    initial_command: Option<&str>,
    is_plain_shell: bool,
) -> String {
    let sid = session_id.unwrap_or("default");
    let mut key = format!("{project_path}_{sid}");

    if is_plain_shell && let Some(cmd) = initial_command {
        let encoded = base64::engine::general_purpose::STANDARD.encode(cmd);
        let prefix = &encoded[..encoded.len().min(16)];
        key.push_str("_cmd_");
        key.push_str(prefix);
    }

    key
}

// ─── Command Construction ────────────────────────────────────────────────────

/// Build the shell command for a given provider and session configuration.
///
/// Returns `(shell_binary, shell_args)` suitable for `CommandBuilder`.
pub fn build_shell_command(
    provider: &str,
    has_session: bool,
    session_id: Option<&str>,
    initial_command: Option<&str>,
    is_plain_shell: bool,
) -> (String, Vec<String>) {
    let shell = "bash".to_string();

    let command = if is_plain_shell {
        // Plain shell: run the initial command directly, or just open bash
        match initial_command {
            Some(cmd) if !cmd.is_empty() => cmd.to_string(),
            _ => return (shell, Vec::new()), // interactive bash
        }
    } else {
        match provider {
            "claude" => {
                if has_session {
                    if let Some(sid) = session_id {
                        if let Some(cmd) = initial_command.filter(|c| !c.is_empty()) {
                            format!("{cmd} --resume \"{sid}\" || {cmd}")
                        } else {
                            format!("claude --resume \"{sid}\" || claude")
                        }
                    } else {
                        initial_command
                            .filter(|c| !c.is_empty())
                            .unwrap_or("claude")
                            .to_string()
                    }
                } else {
                    initial_command
                        .filter(|c| !c.is_empty())
                        .unwrap_or("claude")
                        .to_string()
                }
            }
            "cursor" => {
                if has_session {
                    if let Some(sid) = session_id {
                        format!("cursor-agent --resume=\"{sid}\"")
                    } else {
                        "cursor-agent".to_string()
                    }
                } else {
                    "cursor-agent".to_string()
                }
            }
            "codex" => {
                if has_session {
                    if let Some(sid) = session_id {
                        format!("codex resume \"{sid}\" || codex")
                    } else {
                        "codex".to_string()
                    }
                } else {
                    "codex".to_string()
                }
            }
            "gemini" => {
                if has_session {
                    if let Some(sid) = session_id {
                        let cmd = initial_command
                            .filter(|c| !c.is_empty())
                            .unwrap_or("gemini");
                        format!("{cmd} --resume \"{sid}\"")
                    } else {
                        initial_command
                            .filter(|c| !c.is_empty())
                            .unwrap_or("gemini")
                            .to_string()
                    }
                } else {
                    initial_command
                        .filter(|c| !c.is_empty())
                        .unwrap_or("gemini")
                        .to_string()
                }
            }
            _ => {
                // Unknown provider: fall back to claude
                if has_session {
                    if let Some(sid) = session_id {
                        format!("claude --resume \"{sid}\" || claude")
                    } else {
                        "claude".to_string()
                    }
                } else {
                    "claude".to_string()
                }
            }
        }
    };

    (shell, vec!["-c".to_string(), command])
}

// ─── PTY Spawn ───────────────────────────────────────────────────────────────

/// Result of spawning a PTY process.
pub struct SpawnedPty {
    pub master: Box<dyn MasterPty + Send>,
    pub writer: Box<dyn Write + Send>,
    pub reader: Box<dyn Read + Send>,
    pub child: Box<dyn Child + Send + Sync>,
}

/// Spawn a new PTY process with the given parameters.
pub fn spawn_pty(
    shell: &str,
    args: &[String],
    cwd: &str,
    cols: u16,
    rows: u16,
) -> Result<SpawnedPty, AppError> {
    let pty_system = native_pty_system();

    let size = PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    };

    let pair = pty_system
        .openpty(size)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Failed to open PTY: {e}")))?;

    let mut cmd = CommandBuilder::new(shell);
    for arg in args {
        cmd.arg(arg);
    }
    cmd.cwd(cwd);

    // Set terminal environment variables
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("FORCE_COLOR", "3");

    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Failed to spawn PTY process: {e}")))?;

    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Failed to clone PTY reader: {e}")))?;

    let writer = pair
        .master
        .take_writer()
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Failed to take PTY writer: {e}")))?;

    info!(%cwd, %shell, "PTY process spawned");

    Ok(SpawnedPty {
        master: pair.master,
        writer,
        reader,
        child,
    })
}

// ─── Security Validation ─────────────────────────────────────────────────────

static SAFE_SESSION_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9_.:\-]+$").unwrap());

/// Validate a session ID string for safety.
pub fn validate_session_id(session_id: &str) -> Result<(), AppError> {
    if !SAFE_SESSION_ID.is_match(session_id) {
        return Err(AppError::BadRequest(
            "Invalid session ID: contains unsafe characters".to_string(),
        ));
    }
    Ok(())
}

/// Validate a project path for safety (no traversal, must be a directory).
pub fn validate_project_path(path: &str) -> Result<std::path::PathBuf, AppError> {
    if path.contains('\0') {
        return Err(AppError::BadRequest(
            "Invalid project path: contains null byte".to_string(),
        ));
    }
    if path.contains("..") {
        return Err(AppError::BadRequest(
            "Invalid project path: path traversal detected".to_string(),
        ));
    }

    let resolved = std::path::Path::new(path).canonicalize().map_err(|e| {
        AppError::BadRequest(format!("Invalid project path: {e}"))
    })?;

    if !resolved.is_dir() {
        return Err(AppError::BadRequest(
            "Invalid project path: not a directory".to_string(),
        ));
    }

    Ok(resolved)
}

/// Check if a command looks like a login/auth command that should not reuse sessions.
pub fn is_login_command(initial_command: Option<&str>) -> bool {
    match initial_command {
        Some(cmd) => {
            cmd.contains("setup-token")
                || cmd.contains("cursor-agent login")
                || cmd.contains("auth login")
        }
        None => false,
    }
}

// ─── Buffer Management ───────────────────────────────────────────────────────

/// Push data into a circular buffer, evicting old entries when full.
pub fn push_buffer(buffer: &mut VecDeque<String>, data: String) {
    if buffer.len() >= PTY_BUFFER_CAP {
        buffer.pop_front();
    }
    buffer.push_back(data);
}

// ─── Auth URL Detection ──────────────────────────────────────────────────────

static ANSI_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?:",
        r"\x1b\[[0-?]*[ -/]*[@-~]",      // CSI sequences
        r"|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)", // OSC sequences
        r"|\x1b[@-Z\\^_]",                // Fe escape sequences
        r"|\x1b\[[0-9;]*[mGKHJ]",         // Common SGR/cursor
        r")",
    ))
    .unwrap()
});

static URL_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"https?://[^\s<>"'\x1b\x07\x00-\x1f]+"#).unwrap()
});

/// Strip ANSI escape sequences from text.
pub fn strip_ansi(text: &str) -> String {
    ANSI_REGEX.replace_all(text, "").to_string()
}

/// Extract URLs from plain text (after ANSI stripping).
pub fn extract_urls(text: &str) -> Vec<String> {
    URL_REGEX
        .find_iter(text)
        .map(|m| {
            // Trim trailing punctuation that may have been captured
            let url = m.as_str();
            let url = url.trim_end_matches(['.', ',', ')', ']', ';']);
            url.to_string()
        })
        .filter(|url| url::Url::parse(url).is_ok())
        .collect()
}

/// Check if PTY output suggests an auth URL should be auto-opened.
pub fn should_auto_open_url(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("browser didn't open")
        || lower.contains("open this url")
        || lower.contains("open the following url")
        || lower.contains("copy and paste")
        || lower.contains("open_url:")
        || lower.contains("device code")
        || lower.contains("login required")
        || lower.contains("authenticate")
}

/// Maintain a rolling URL detection buffer and return newly detected auth URLs.
pub fn detect_auth_urls(
    url_buffer: &mut String,
    new_chunk: &str,
    announced_urls: &mut std::collections::HashSet<String>,
) -> Vec<(String, bool)> {
    let clean = strip_ansi(new_chunk);
    url_buffer.push_str(&clean);

    // Cap buffer at 32KB
    const URL_BUFFER_LIMIT: usize = 32768;
    if url_buffer.len() > URL_BUFFER_LIMIT {
        let excess = url_buffer.len() - URL_BUFFER_LIMIT;
        url_buffer.drain(..excess);
    }

    let should_open = should_auto_open_url(&clean);
    let urls = extract_urls(url_buffer);

    let mut results = Vec::new();
    for url in urls {
        if announced_urls.insert(url.clone()) {
            debug!(%url, auto_open = should_open, "Auth URL detected");
            results.push((url, should_open));
        }
    }

    results
}
