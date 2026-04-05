use std::collections::HashMap;
use std::path::{Path, PathBuf};

use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::AsyncBufReadExt;

// ─── Data Types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub name: String,
    pub path: String,
    pub display_name: String,
    pub full_path: String,
    pub is_custom_name: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_manually_added: Option<bool>,
    pub sessions: Vec<SessionInfo>,
    pub cursor_sessions: Vec<SessionInfo>,
    pub codex_sessions: Vec<SessionInfo>,
    pub gemini_sessions: Vec<SessionInfo>,
    pub session_meta: SessionMeta,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub taskmaster: Option<TaskMasterInfo>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub id: String,
    pub summary: String,
    pub message_count: usize,
    pub last_activity: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_grouped: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_size: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMeta {
    pub has_more: bool,
    pub total: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskMasterInfo {
    pub has_taskmaster: bool,
    pub has_essential_files: Option<bool>,
    pub metadata: Option<Value>,
    pub status: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectConfig {
    #[serde(flatten)]
    pub projects: HashMap<String, ProjectConfigEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectConfigEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manually_added: Option<bool>,
}

// ─── Project Directory Cache ─────────────────────────────────────────────────

use dashmap::DashMap;
use std::sync::LazyLock;

static PROJECT_DIR_CACHE: LazyLock<DashMap<String, String>> = LazyLock::new(DashMap::new);

pub fn clear_project_directory_cache() {
    let prev_size = PROJECT_DIR_CACHE.len();
    PROJECT_DIR_CACHE.clear();
    tracing::debug!(prev_size, "Project directory cache cleared");
}

// ─── Config Management ──────────────────────────────────────────────────────

fn config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".claude")
        .join("project-config.json")
}

pub async fn load_project_config() -> ProjectConfig {
    let path = config_path();
    match tokio::fs::read_to_string(&path).await {
        Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
        Err(_) => ProjectConfig::default(),
    }
}

pub async fn save_project_config(config: &ProjectConfig) -> std::io::Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let data = serde_json::to_string_pretty(config)?;
    tokio::fs::write(&path, data).await
}

// ─── Project Path Extraction ─────────────────────────────────────────────────

/// Extract actual project directory from Claude JSONL sessions (with caching).
pub async fn extract_project_directory(project_name: &str) -> String {
    // Check cache
    if let Some(cached) = PROJECT_DIR_CACHE.get(project_name) {
        tracing::debug!(project_name, "Project directory cache hit");
        return cached.clone();
    }
    tracing::debug!(project_name, "Project directory cache miss, resolving");

    // Check config for originalPath
    let config = load_project_config().await;
    if let Some(entry) = config.projects.get(project_name) {
        if let Some(ref original_path) = entry.original_path {
            PROJECT_DIR_CACHE.insert(project_name.to_string(), original_path.clone());
            return original_path.clone();
        }
    }

    let home = dirs::home_dir().unwrap_or_default();
    let project_dir = home.join(".claude").join("projects").join(project_name);

    let extracted = match extract_cwd_from_jsonl(&project_dir).await {
        Some(cwd) => cwd,
        None => project_name.replace('-', "/"),
    };

    PROJECT_DIR_CACHE.insert(project_name.to_string(), extracted.clone());
    extracted
}

/// Scan JSONL files in a project directory and extract the most likely cwd.
async fn extract_cwd_from_jsonl(project_dir: &Path) -> Option<String> {
    let mut read_dir = tokio::fs::read_dir(project_dir).await.ok()?;
    let mut cwd_counts: HashMap<String, usize> = HashMap::new();
    let mut latest_timestamp: i64 = 0;
    let mut latest_cwd: Option<String> = None;

    while let Ok(Some(entry)) = read_dir.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }

        if let Ok(file) = tokio::fs::File::open(&path).await {
            let reader = tokio::io::BufReader::new(file);
            let mut lines = reader.lines();

            while let Ok(Some(line)) = lines.next_line().await {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(entry) = serde_json::from_str::<Value>(trimmed) {
                    if let Some(cwd) = entry.get("cwd").and_then(|v| v.as_str()) {
                        *cwd_counts.entry(cwd.to_string()).or_insert(0) += 1;

                        if let Some(ts_str) = entry.get("timestamp").and_then(|v| v.as_str()) {
                            if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(ts_str) {
                                let millis = ts.timestamp_millis();
                                if millis > latest_timestamp {
                                    latest_timestamp = millis;
                                    latest_cwd = Some(cwd.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if cwd_counts.is_empty() {
        return None;
    }

    if cwd_counts.len() == 1 {
        return cwd_counts.into_keys().next();
    }

    // Multiple cwds - prefer most recent if it has >=25% of max count
    let max_count = cwd_counts.values().copied().max().unwrap_or(0);
    if let Some(ref recent) = latest_cwd {
        let recent_count = cwd_counts.get(recent).copied().unwrap_or(0);
        if recent_count >= max_count / 4 {
            return latest_cwd;
        }
    }

    // Otherwise return the most frequent
    cwd_counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(cwd, _)| cwd)
}

/// Generate a display name from project name / path.
pub async fn generate_display_name(project_name: &str, actual_project_dir: Option<&str>) -> String {
    let project_path = actual_project_dir
        .map(|s| s.to_string())
        .unwrap_or_else(|| project_name.replace('-', "/"));

    // Try to read package.json name
    let pkg_path = PathBuf::from(&project_path).join("package.json");
    if let Ok(data) = tokio::fs::read_to_string(&pkg_path).await {
        if let Ok(pkg) = serde_json::from_str::<Value>(&data) {
            if let Some(name) = pkg.get("name").and_then(|v| v.as_str()) {
                if !name.is_empty() {
                    return name.to_string();
                }
            }
        }
    }

    // Fall back to last path component
    if project_path.starts_with('/') {
        if let Some(last) = project_path.rsplit('/').find(|s| !s.is_empty()) {
            return last.to_string();
        }
    }

    project_path
}

// ─── Session Discovery ───────────────────────────────────────────────────────

/// Parse Claude JSONL sessions for a project.
pub async fn get_claude_sessions(
    project_name: &str,
    limit: Option<usize>,
    offset: usize,
) -> (Vec<SessionInfo>, usize, bool) {
    tracing::debug!(project_name, "Discovering Claude sessions");
    let home = dirs::home_dir().unwrap_or_default();
    let project_dir = home.join(".claude").join("projects").join(project_name);

    let mut read_dir = match tokio::fs::read_dir(&project_dir).await {
        Ok(rd) => rd,
        Err(_) => return (vec![], 0, false),
    };

    let mut session_entries: Vec<(String, String, usize, String)> = Vec::new(); // (id, summary, count, lastActivity)

    while let Ok(Some(entry)) = read_dir.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let session_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();

        if session_id.is_empty() {
            continue;
        }

        // Quick scan: count lines and get first user message + last timestamp
        let mut count = 0usize;
        let mut first_user_msg = String::new();
        let mut last_ts = String::new();

        if let Ok(file) = tokio::fs::File::open(&path).await {
            let reader = tokio::io::BufReader::new(file);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                count += 1;
                if let Ok(entry) = serde_json::from_str::<Value>(trimmed) {
                    if let Some(ts) = entry.get("timestamp").and_then(|v| v.as_str()) {
                        last_ts = ts.to_string();
                    }
                    if first_user_msg.is_empty() {
                        if let Some(role) = entry
                            .get("message")
                            .and_then(|m| m.get("role"))
                            .and_then(|r| r.as_str())
                        {
                            if role == "user" {
                                let content = entry
                                    .get("message")
                                    .and_then(|m| m.get("content"));
                                if let Some(Value::String(s)) = content {
                                    first_user_msg = s.chars().take(100).collect();
                                } else if let Some(Value::Array(arr)) = content {
                                    for part in arr {
                                        if part.get("type").and_then(|v| v.as_str()) == Some("text") {
                                            if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                                                first_user_msg = t.chars().take(100).collect();
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        session_entries.push((session_id, first_user_msg, count, last_ts));
    }

    // Sort by last activity descending
    session_entries.sort_by(|a, b| b.3.cmp(&a.3));

    let total = session_entries.len();
    let end = limit.map(|l| (offset + l).min(total)).unwrap_or(total);
    let has_more = end < total;

    let sessions: Vec<SessionInfo> = session_entries[offset..end]
        .iter()
        .map(|(id, summary, count, last_ts)| SessionInfo {
            id: id.clone(),
            summary: if summary.is_empty() {
                "New conversation".to_string()
            } else {
                summary.clone()
            },
            message_count: *count,
            last_activity: last_ts.clone(),
            cwd: None,
            provider: "claude".to_string(),
            is_grouped: None,
            group_size: None,
            name: None,
        })
        .collect();

    (sessions, total, has_more)
}

/// Get Cursor sessions for a project path.
pub async fn get_cursor_sessions(project_path: &str) -> Vec<SessionInfo> {
    let home = dirs::home_dir().unwrap_or_default();
    let cwd_id = format!("{:x}", Md5::digest(project_path.as_bytes()));
    let cursor_dir = home.join(".cursor").join("chats").join(&cwd_id);

    let mut read_dir = match tokio::fs::read_dir(&cursor_dir).await {
        Ok(rd) => rd,
        Err(_) => return vec![],
    };

    let mut sessions = Vec::new();
    while let Ok(Some(entry)) = read_dir.next_entry().await {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let store_db = path.join("store.db");
        if !store_db.exists() {
            continue;
        }
        let session_id = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();

        if session_id.is_empty() {
            continue;
        }

        // Get metadata from file modification time
        let last_activity = tokio::fs::metadata(&store_db)
            .await
            .ok()
            .and_then(|m| m.modified().ok())
            .map(|t| {
                chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339()
            })
            .unwrap_or_default();

        sessions.push(SessionInfo {
            id: session_id,
            summary: "Cursor session".to_string(),
            message_count: 0,
            last_activity,
            cwd: Some(project_path.to_string()),
            provider: "cursor".to_string(),
            is_grouped: None,
            group_size: None,
            name: None,
        });
    }

    sessions.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));
    sessions
}

/// Build an index of all Codex sessions by project path.
pub async fn build_codex_sessions_index() -> HashMap<String, Vec<(String, PathBuf)>> {
    tracing::debug!("Building Codex sessions index");
    let home = dirs::home_dir().unwrap_or_default();
    let sessions_dir = home.join(".codex").join("sessions");

    let mut index: HashMap<String, Vec<(String, PathBuf)>> = HashMap::new();
    let mut read_dir = match tokio::fs::read_dir(&sessions_dir).await {
        Ok(rd) => rd,
        Err(_) => return index,
    };

    while let Ok(Some(entry)) = read_dir.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let session_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        if session_id.is_empty() {
            continue;
        }

        // Read first line to get project path from session metadata
        if let Ok(file) = tokio::fs::File::open(&path).await {
            let reader = tokio::io::BufReader::new(file);
            let mut lines = reader.lines();
            if let Ok(Some(line)) = lines.next_line().await {
                if let Ok(entry) = serde_json::from_str::<Value>(line.trim()) {
                    if let Some(cwd) = entry.get("cwd").and_then(|v| v.as_str()) {
                        index
                            .entry(cwd.to_string())
                            .or_default()
                            .push((session_id, path.clone()));
                    }
                }
            }
        }
    }

    tracing::debug!(project_count = index.len(), "Codex sessions index built");
    index
}
pub async fn get_codex_sessions(
    project_path: &str,
    index: &HashMap<String, Vec<(String, PathBuf)>>,
) -> Vec<SessionInfo> {
    let entries = match index.get(project_path) {
        Some(e) => e,
        None => return vec![],
    };

    let mut sessions = Vec::new();
    for (session_id, path) in entries {
        let last_activity = tokio::fs::metadata(path)
            .await
            .ok()
            .and_then(|m| m.modified().ok())
            .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339())
            .unwrap_or_default();

        sessions.push(SessionInfo {
            id: session_id.clone(),
            summary: "Codex session".to_string(),
            message_count: 0,
            last_activity,
            cwd: Some(project_path.to_string()),
            provider: "codex".to_string(),
            is_grouped: None,
            group_size: None,
            name: None,
        });
    }

    sessions.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));
    sessions
}

/// Get Gemini CLI sessions for a project path.
pub async fn get_gemini_cli_sessions(project_path: &str) -> Vec<SessionInfo> {
    let home = dirs::home_dir().unwrap_or_default();
    // Gemini CLI stores sessions at ~/.gemini/tmp/<encoded_project>/chats/*.json
    let gemini_base = home.join(".gemini").join("tmp");

    // The project path is encoded similar to Claude (/ -> -)
    let encoded = project_path.replace('/', "-");
    // Also try the raw directory name
    let candidates = vec![
        gemini_base.join(&encoded).join("chats"),
        gemini_base.join(project_path.trim_start_matches('/')).join("chats"),
    ];

    let mut sessions = Vec::new();

    for chats_dir in &candidates {
        let mut read_dir = match tokio::fs::read_dir(chats_dir).await {
            Ok(rd) => rd,
            Err(_) => continue,
        };

        while let Ok(Some(entry)) = read_dir.next_entry().await {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let session_id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            if session_id.is_empty() {
                continue;
            }

            // Read session file for metadata
            let mut summary = "Gemini session".to_string();
            let mut last_activity = String::new();

            if let Ok(data) = tokio::fs::read_to_string(&path).await {
                if let Ok(session) = serde_json::from_str::<Value>(&data) {
                    if let Some(ts) = session.get("lastUpdated").and_then(|v| v.as_str()) {
                        last_activity = ts.to_string();
                    }
                    // Try to get first user message as summary
                    if let Some(messages) = session.get("messages").and_then(|v| v.as_array()) {
                        for msg in messages {
                            let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
                            if role == "user" {
                                if let Some(content) = msg.get("content").and_then(|v| v.as_str())
                                {
                                    summary = content.chars().take(100).collect();
                                    break;
                                }
                            }
                        }
                    }
                }
            }

            if last_activity.is_empty() {
                last_activity = tokio::fs::metadata(&path)
                    .await
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339())
                    .unwrap_or_default();
            }

            sessions.push(SessionInfo {
                id: session_id,
                summary,
                message_count: 0,
                last_activity,
                cwd: Some(project_path.to_string()),
                provider: "gemini".to_string(),
                is_grouped: None,
                group_size: None,
                name: None,
            });
        }
    }

    sessions.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));
    sessions
}

// ─── Main Discovery ──────────────────────────────────────────────────────────

/// Discover all projects from all provider directories.
pub async fn get_projects(
    pool: &sqlx::SqlitePool,
    progress_tx: Option<tokio::sync::mpsc::Sender<ProgressEvent>>,
) -> Vec<Project> {
    tracing::debug!("Starting project discovery scan");
    let home = dirs::home_dir().unwrap_or_default();
    let claude_projects_dir = home.join(".claude").join("projects");
    let config = load_project_config().await;
    let mut projects = Vec::new();
    let mut existing_project_names = std::collections::HashSet::new();

    // Build codex index once
    let codex_index = build_codex_sessions_index().await;

    // Phase 1: Discover Claude projects
    let mut directories: Vec<String> = Vec::new();
    if let Ok(mut read_dir) = tokio::fs::read_dir(&claude_projects_dir).await {
        while let Ok(Some(entry)) = read_dir.next_entry().await {
            if let Ok(ft) = entry.file_type().await {
                if ft.is_dir() {
                    if let Some(name) = entry.file_name().to_str() {
                        directories.push(name.to_string());
                        existing_project_names.insert(name.to_string());
                    }
                }
            }
        }
    }

    let manual_projects: Vec<(String, ProjectConfigEntry)> = config
        .projects
        .iter()
        .filter(|(name, cfg)| {
            cfg.manually_added == Some(true) && !existing_project_names.contains(name.as_str())
        })
        .map(|(n, c)| (n.clone(), c.clone()))
        .collect();

    let total = directories.len() + manual_projects.len();
    let mut processed = 0usize;

    for dir_name in &directories {
        processed += 1;
        if let Some(ref tx) = progress_tx {
            let _ = tx
                .send(ProgressEvent {
                    phase: "loading".to_string(),
                    current: processed,
                    total,
                    current_project: dir_name.clone(),
                })
                .await;
        }

        let actual_dir = extract_project_directory(dir_name).await;
        let custom_name = config
            .projects
            .get(dir_name)
            .and_then(|c| c.display_name.clone());
        let display_name = custom_name
            .clone()
            .unwrap_or(generate_display_name(dir_name, Some(&actual_dir)).await);

        let (sessions, session_total, has_more) = get_claude_sessions(dir_name, Some(5), 0).await;
        let cursor_sessions = get_cursor_sessions(&actual_dir).await;
        let codex_sessions = get_codex_sessions(&actual_dir, &codex_index).await;
        let gemini_sessions = get_gemini_cli_sessions(&actual_dir).await;

        projects.push(Project {
            name: dir_name.clone(),
            path: actual_dir.clone(),
            display_name,
            full_path: actual_dir,
            is_custom_name: custom_name.is_some(),
            is_manually_added: None,
            sessions,
            cursor_sessions,
            codex_sessions,
            gemini_sessions,
            session_meta: SessionMeta {
                has_more,
                total: session_total,
            },
            taskmaster: None,
        });
    }

    // Phase 2: Add manually configured projects
    for (project_name, project_config) in &manual_projects {
        processed += 1;
        if let Some(ref tx) = progress_tx {
            let _ = tx
                .send(ProgressEvent {
                    phase: "loading".to_string(),
                    current: processed,
                    total,
                    current_project: project_name.clone(),
                })
                .await;
        }

        let actual_dir = project_config
            .original_path
            .clone()
            .unwrap_or_else(|| project_name.replace('-', "/"));

        let display_name = project_config
            .display_name
            .clone()
            .unwrap_or(generate_display_name(project_name, Some(&actual_dir)).await);

        let cursor_sessions = get_cursor_sessions(&actual_dir).await;
        let codex_sessions = get_codex_sessions(&actual_dir, &codex_index).await;
        let gemini_sessions = get_gemini_cli_sessions(&actual_dir).await;

        projects.push(Project {
            name: project_name.clone(),
            path: actual_dir.clone(),
            display_name,
            full_path: actual_dir,
            is_custom_name: project_config.display_name.is_some(),
            is_manually_added: Some(true),
            sessions: vec![],
            cursor_sessions,
            codex_sessions,
            gemini_sessions,
            session_meta: SessionMeta {
                has_more: false,
                total: 0,
            },
            taskmaster: None,
        });
    }

    // Sort projects by most recent activity
    projects.sort_by(|a, b| {
        let a_latest = most_recent_activity(a);
        let b_latest = most_recent_activity(b);
        b_latest.cmp(&a_latest)
    });

    // Apply custom session names from database
    apply_custom_session_names(pool, &mut projects).await;

    tracing::debug!(project_count = projects.len(), "Project discovery scan complete");
    projects
}

/// Apply custom names from session_names DB table to session lists.
async fn apply_custom_session_names(pool: &sqlx::SqlitePool, projects: &mut [Project]) {
    let providers = ["claude", "cursor", "codex", "gemini"];
    for provider in providers {
        let session_ids: Vec<String> = projects
            .iter()
            .flat_map(|p| match provider {
                "claude" => p.sessions.iter(),
                "cursor" => p.cursor_sessions.iter(),
                "codex" => p.codex_sessions.iter(),
                "gemini" => p.gemini_sessions.iter(),
                _ => [].iter(),
            })
            .map(|s| s.id.clone())
            .collect();

        if session_ids.is_empty() {
            continue;
        }

        let names =
            match crate::db::session_names::get_names_batch(pool, &session_ids, provider).await {
                Ok(n) => n,
                Err(e) => {
                    tracing::warn!(%provider, "Failed to fetch custom session names: {e}");
                    continue;
                }
            };

        if names.is_empty() {
            continue;
        }

        for project in projects.iter_mut() {
            let sessions = match provider {
                "claude" => &mut project.sessions,
                "cursor" => &mut project.cursor_sessions,
                "codex" => &mut project.codex_sessions,
                "gemini" => &mut project.gemini_sessions,
                _ => continue,
            };
            for session in sessions.iter_mut() {
                if let Some(custom_name) = names.get(&session.id) {
                    session.name = Some(custom_name.clone());
                    session.summary = custom_name.clone();
                }
            }
        }
    }
}

fn most_recent_activity(project: &Project) -> String {
    let all_sessions = project
        .sessions
        .iter()
        .chain(project.cursor_sessions.iter())
        .chain(project.codex_sessions.iter())
        .chain(project.gemini_sessions.iter());

    all_sessions
        .map(|s| &s.last_activity)
        .max()
        .cloned()
        .unwrap_or_default()
}

#[derive(Debug, Clone, Serialize)]
pub struct ProgressEvent {
    pub phase: String,
    pub current: usize,
    pub total: usize,
    pub current_project: String,
}

// ─── Project Management ──────────────────────────────────────────────────────

pub async fn add_project_manually(project_path: &str) -> Result<String, crate::error::AppError> {
    let path = PathBuf::from(project_path);
    if !path.exists() {
        return Err(crate::error::AppError::BadRequest(
            "Project path does not exist".to_string(),
        ));
    }

    // Encode the path as project name (same as Claude: / -> -)
    let project_name = project_path.replace('/', "-");

    let mut config = load_project_config().await;
    config.projects.insert(
        project_name.clone(),
        ProjectConfigEntry {
            display_name: None,
            original_path: Some(project_path.to_string()),
            manually_added: Some(true),
        },
    );
    save_project_config(&config)
        .await
        .map_err(|e| crate::error::AppError::Internal(e.into()))?;

    clear_project_directory_cache();
    Ok(project_name)
}

pub async fn rename_project(
    project_name: &str,
    display_name: &str,
) -> Result<(), crate::error::AppError> {
    let mut config = load_project_config().await;
    let entry = config
        .projects
        .entry(project_name.to_string())
        .or_insert_with(|| ProjectConfigEntry {
            display_name: None,
            original_path: None,
            manually_added: None,
        });
    entry.display_name = Some(display_name.to_string());
    save_project_config(&config)
        .await
        .map_err(|e| crate::error::AppError::Internal(e.into()))?;
    Ok(())
}

pub async fn delete_project(project_name: &str) -> Result<(), crate::error::AppError> {
    let home = dirs::home_dir().unwrap_or_default();
    let project_dir = home.join(".claude").join("projects").join(project_name);

    // Remove Claude sessions directory
    if project_dir.exists() {
        tokio::fs::remove_dir_all(&project_dir)
            .await
            .map_err(|e| crate::error::AppError::Internal(e.into()))?;
    }

    // Remove from config
    let mut config = load_project_config().await;
    config.projects.remove(project_name);
    save_project_config(&config)
        .await
        .map_err(|e| crate::error::AppError::Internal(e.into()))?;

    clear_project_directory_cache();
    Ok(())
}

pub async fn delete_session(
    project_name: &str,
    session_id: &str,
    provider: &str,
) -> Result<(), crate::error::AppError> {
    let home = dirs::home_dir().unwrap_or_default();

    match provider {
        "claude" => {
            let session_file = home
                .join(".claude")
                .join("projects")
                .join(project_name)
                .join(format!("{session_id}.jsonl"));
            if session_file.exists() {
                tokio::fs::remove_file(&session_file)
                    .await
                    .map_err(|e| crate::error::AppError::Internal(e.into()))?;
            }
        }
        "codex" => {
            let session_file = home
                .join(".codex")
                .join("sessions")
                .join(format!("{session_id}.jsonl"));
            if session_file.exists() {
                tokio::fs::remove_file(&session_file)
                    .await
                    .map_err(|e| crate::error::AppError::Internal(e.into()))?;
            }
        }
        _ => {
            // cursor/gemini: more complex, skip for now
        }
    }

    Ok(())
}

/// Get messages for a Codex session (reads JSONL file).
pub async fn get_codex_session_messages(
    session_id: &str,
    limit: Option<u32>,
    offset: u32,
) -> Result<(Vec<Value>, usize, bool), crate::error::AppError> {
    let home = dirs::home_dir().unwrap_or_default();
    let session_file = home
        .join(".codex")
        .join("sessions")
        .join(format!("{session_id}.jsonl"));

    if !session_file.exists() {
        return Ok((vec![], 0, false));
    }

    let file = tokio::fs::File::open(&session_file).await?;
    let reader = tokio::io::BufReader::new(file);
    let mut lines = reader.lines();
    let mut messages: Vec<Value> = Vec::new();

    while let Ok(Some(line)) = lines.next_line().await {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
            messages.push(parsed);
        }
    }

    let total = messages.len();
    let start = offset as usize;
    let end = limit
        .map(|l| (start + l as usize).min(total))
        .unwrap_or(total);
    let has_more = end < total;

    let page = if start < total {
        messages[start..end].to_vec()
    } else {
        vec![]
    };

    Ok((page, total, has_more))
}

/// Get messages for a Gemini CLI session (reads JSON file).
pub async fn get_gemini_cli_session_messages(
    session_id: &str,
) -> Result<Vec<Value>, crate::error::AppError> {
    let home = dirs::home_dir().unwrap_or_default();
    let gemini_base = home.join(".gemini").join("tmp");

    // Search for the session file in all project directories
    let mut found_messages = Vec::new();

    if let Ok(mut read_dir) = tokio::fs::read_dir(&gemini_base).await {
        while let Ok(Some(entry)) = read_dir.next_entry().await {
            let chats_dir = entry.path().join("chats");
            let session_file = chats_dir.join(format!("{session_id}.json"));

            if session_file.exists() {
                if let Ok(data) = tokio::fs::read_to_string(&session_file).await {
                    if let Ok(session) = serde_json::from_str::<Value>(&data) {
                        if let Some(messages) = session.get("messages").and_then(|v| v.as_array()) {
                            found_messages = messages.clone();
                        }
                    }
                }
                break;
            }
        }
    }

    Ok(found_messages)
}
