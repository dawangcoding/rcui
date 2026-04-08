use std::path::PathBuf;

use axum::extract::Query;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::process::Command;

use crate::auth::middleware::AuthUser;
use crate::error::AppError;
use crate::services::project_scanner;

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Resolve a project name to an absolute path, with path traversal protection.
async fn resolve_project_path(project: &str) -> Result<PathBuf, AppError> {
    // If already an absolute path, use it directly
    let path = if project.starts_with('/') {
        PathBuf::from(project)
    } else {
        let actual = project_scanner::extract_project_directory(project).await;
        PathBuf::from(actual)
    };

    // Block root directory operations
    if path == PathBuf::from("/") {
        return Err(AppError::BadRequest("Cannot operate on root directory".into()));
    }

    // Validate path exists
    if !path.exists() {
        return Err(AppError::NotFound(format!("Project path not found: {}", path.display())));
    }

    Ok(path)
}

/// Validate a branch name (no shell metacharacters).
fn validate_ref(name: &str) -> Result<(), AppError> {
    if name.is_empty() || name.contains('\0') || name.contains("..") {
        return Err(AppError::BadRequest("Invalid ref name".into()));
    }
    // Block obvious injection attempts
    if name.contains(';') || name.contains('|') || name.contains('&') || name.contains('`') {
        return Err(AppError::BadRequest("Invalid characters in ref name".into()));
    }
    Ok(())
}

/// Validate a file path (no null bytes, no traversal).
fn validate_file_path(file: &str, project_path: &PathBuf) -> Result<PathBuf, AppError> {
    if file.contains('\0') {
        return Err(AppError::BadRequest("Invalid file path".into()));
    }
    let full = project_path.join(file);
    let canonical_project = project_path.canonicalize().unwrap_or(project_path.clone());
    let canonical_file = full.canonicalize().unwrap_or(full.clone());

    if !canonical_file.starts_with(&canonical_project) {
        return Err(AppError::BadRequest("Path traversal detected".into()));
    }
    Ok(full)
}

/// Run a git command and return its output.
async fn run_git(project_path: &PathBuf, args: &[&str]) -> Result<String, AppError> {
    let output = Command::new("git")
        .args(args)
        .current_dir(project_path)
        .output()
        .await
        .map_err(|e| AppError::Internal(e.into()))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        Err(AppError::Internal(anyhow::anyhow!("git error: {stderr}")))
    }
}

/// Run a git command, returning stdout even on non-zero exit (for diff, etc.).
async fn run_git_lossy(project_path: &PathBuf, args: &[&str]) -> Result<String, AppError> {
    let output = Command::new("git")
        .args(args)
        .current_dir(project_path)
        .output()
        .await
        .map_err(|e| AppError::Internal(e.into()))?;

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

// ─── Query Types ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ProjectQuery {
    pub project: String,
}

#[derive(Deserialize)]
pub struct FileQuery {
    pub project: String,
    pub file: String,
}

#[derive(Deserialize)]
pub struct CommitsQuery {
    pub project: String,
    pub limit: Option<u32>,
}

#[derive(Deserialize)]
pub struct CommitDiffQuery {
    pub project: String,
    pub commit: String,
}

// ─── Request Types ───────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitRequest {
    pub project: String,
    pub message: String,
    #[serde(default)]
    pub files: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitialCommitRequest {
    pub project: String,
    #[serde(default = "default_initial_commit_message")]
    pub message: String,
    #[serde(default)]
    pub files: Vec<String>,
}

fn default_initial_commit_message() -> String {
    "Initial commit".to_string()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchRequest {
    pub project: String,
    pub branch: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscardRequest {
    pub project: String,
    pub file: String,
}

// ─── Endpoints ───────────────────────────────────────────────────────────────

/// GET /api/git/status
pub async fn status(
    _auth: AuthUser,
    Query(q): Query<ProjectQuery>,
) -> Result<Json<Value>, AppError> {
    let path = resolve_project_path(&q.project).await?;

    // Get branch name
    let branch = run_git(&path, &["rev-parse", "--abbrev-ref", "HEAD"])
        .await
        .unwrap_or_else(|_| "main".to_string())
        .trim()
        .to_string();

    // Check if there are any commits
    let has_commits = run_git(&path, &["rev-parse", "HEAD"]).await.is_ok();

    // Get porcelain status
    let status_output = run_git(&path, &["status", "--porcelain=v1"]).await.unwrap_or_default();

    let mut modified = Vec::new();
    let mut added = Vec::new();
    let mut deleted = Vec::new();
    let mut untracked = Vec::new();

    for line in status_output.lines() {
        if line.len() < 4 {
            continue;
        }
        let xy = &line[..2];
        let file = line[3..].trim().to_string();

        match xy.trim() {
            "M" | "MM" | "AM" => modified.push(file),
            "A" => added.push(file),
            "D" => deleted.push(file),
            "??" => untracked.push(file),
            "R" | "RM" => modified.push(file),
            "C" => added.push(file),
            _ => {
                if xy.contains('M') {
                    modified.push(file);
                } else if xy.contains('D') {
                    deleted.push(file);
                } else if xy.contains('A') {
                    added.push(file);
                }
            }
        }
    }

    Ok(Json(json!({
        "branch": branch,
        "hasCommits": has_commits,
        "modified": modified,
        "added": added,
        "deleted": deleted,
        "untracked": untracked
    })))
}

/// GET /api/git/diff
pub async fn diff(
    _auth: AuthUser,
    Query(q): Query<FileQuery>,
) -> Result<Json<Value>, AppError> {
    let path = resolve_project_path(&q.project).await?;
    validate_file_path(&q.file, &path)?;

    let mut output = run_git_lossy(&path, &["diff", "--", &q.file]).await?;
    // Also include staged diff
    let staged = run_git_lossy(&path, &["diff", "--cached", "--", &q.file]).await?;
    if !staged.is_empty() {
        output.push_str(&staged);
    }

    // Truncate large diffs
    let max_len = 500_000;
    let is_truncated = output.len() > max_len;
    if is_truncated {
        output.truncate(max_len);
    }

    Ok(Json(json!({
        "diff": output,
        "isTruncated": is_truncated
    })))
}

/// GET /api/git/file-with-diff
pub async fn file_with_diff(
    _auth: AuthUser,
    Query(q): Query<FileQuery>,
) -> Result<Json<Value>, AppError> {
    let path = resolve_project_path(&q.project).await?;
    let file_path = validate_file_path(&q.file, &path)?;

    let is_untracked = !run_git(&path, &["ls-files", "--error-unmatch", &q.file])
        .await
        .is_ok();

    let current_content = tokio::fs::read_to_string(&file_path)
        .await
        .unwrap_or_default();

    let old_content = if is_untracked {
        String::new()
    } else {
        run_git_lossy(&path, &["show", &format!("HEAD:{}", q.file)])
            .await
            .unwrap_or_default()
    };

    let is_deleted = !file_path.exists();

    Ok(Json(json!({
        "currentContent": current_content,
        "oldContent": old_content,
        "isDeleted": is_deleted,
        "isUntracked": is_untracked
    })))
}

/// POST /api/git/initial-commit
pub async fn initial_commit(
    _auth: AuthUser,
    Json(body): Json<InitialCommitRequest>,
) -> Result<Json<Value>, AppError> {
    let path = resolve_project_path(&body.project).await?;

    if body.files.is_empty() {
        run_git(&path, &["add", "-A"]).await?;
    } else {
        for file in &body.files {
            validate_file_path(file, &path)?;
            run_git(&path, &["add", file]).await?;
        }
    }

    let message = if body.message.trim().is_empty() {
        "Initial commit".to_string()
    } else {
        body.message
    };

    let output = run_git(&path, &["commit", "-m", &message]).await?;
    Ok(Json(json!({ "success": true, "output": output })))
}

/// POST /api/git/commit
pub async fn commit(
    _auth: AuthUser,
    Json(body): Json<CommitRequest>,
) -> Result<Json<Value>, AppError> {
    let path = resolve_project_path(&body.project).await?;

    if body.files.is_empty() {
        run_git(&path, &["add", "-A"]).await?;
    } else {
        for file in &body.files {
            validate_file_path(file, &path)?;
            run_git(&path, &["add", file]).await?;
        }
    }

    let output = run_git(&path, &["commit", "-m", &body.message]).await?;
    Ok(Json(json!({ "success": true, "output": output })))
}

/// POST /api/git/revert-local-commit
pub async fn revert_local_commit(
    _auth: AuthUser,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<Value>, AppError> {
    let project = body["project"].as_str().unwrap_or("");
    let path = resolve_project_path(project).await?;

    let output = run_git(&path, &["reset", "--soft", "HEAD~1"]).await?;
    Ok(Json(json!({ "success": true, "output": output })))
}

/// GET /api/git/branches
pub async fn branches(
    _auth: AuthUser,
    Query(q): Query<ProjectQuery>,
) -> Result<Json<Value>, AppError> {
    let path = resolve_project_path(&q.project).await?;

    let local = run_git(&path, &["branch", "--format=%(refname:short)"]).await.unwrap_or_default();
    let remote = run_git(&path, &["branch", "-r", "--format=%(refname:short)"]).await.unwrap_or_default();

    let local_branches: Vec<&str> = local.lines().filter(|l| !l.is_empty()).collect();
    let remote_branches: Vec<&str> = remote.lines().filter(|l| !l.is_empty()).collect();

    let mut all: Vec<&str> = local_branches.clone();
    all.extend(remote_branches.iter());

    Ok(Json(json!({
        "branches": all,
        "localBranches": local_branches,
        "remoteBranches": remote_branches
    })))
}

/// POST /api/git/checkout
pub async fn checkout(
    _auth: AuthUser,
    Json(body): Json<BranchRequest>,
) -> Result<Json<Value>, AppError> {
    let path = resolve_project_path(&body.project).await?;
    validate_ref(&body.branch)?;

    let output = run_git(&path, &["checkout", &body.branch]).await?;
    Ok(Json(json!({ "success": true, "output": output })))
}

/// POST /api/git/create-branch
pub async fn create_branch(
    _auth: AuthUser,
    Json(body): Json<BranchRequest>,
) -> Result<Json<Value>, AppError> {
    let path = resolve_project_path(&body.project).await?;
    validate_ref(&body.branch)?;

    let output = run_git(&path, &["checkout", "-b", &body.branch]).await?;
    Ok(Json(json!({ "success": true, "output": output })))
}

/// POST /api/git/delete-branch
pub async fn delete_branch(
    _auth: AuthUser,
    Json(body): Json<BranchRequest>,
) -> Result<Json<Value>, AppError> {
    let path = resolve_project_path(&body.project).await?;
    validate_ref(&body.branch)?;

    let output = run_git(&path, &["branch", "-d", &body.branch]).await?;
    Ok(Json(json!({ "success": true, "output": output })))
}

/// GET /api/git/commits
pub async fn commits(
    _auth: AuthUser,
    Query(q): Query<CommitsQuery>,
) -> Result<Json<Value>, AppError> {
    let path = resolve_project_path(&q.project).await?;
    let limit = q.limit.unwrap_or(50).min(100);
    let limit_str = format!("-{limit}");

    let output = run_git(
        &path,
        &[
            "log",
            &limit_str,
            "--format=%H|%an|%ae|%aI|%s",
            "--shortstat",
        ],
    )
    .await
    .unwrap_or_default();

    let mut result = Vec::new();
    let mut current_commit: Option<Value> = None;

    for line in output.lines() {
        if line.is_empty() {
            continue;
        }

        let parts: Vec<&str> = line.splitn(5, '|').collect();
        if parts.len() == 5 {
            // Push previous commit if exists
            if let Some(c) = current_commit.take() {
                result.push(c);
            }
            current_commit = Some(json!({
                "hash": parts[0],
                "author": parts[1],
                "email": parts[2],
                "date": parts[3],
                "message": parts[4],
                "stats": null
            }));
        } else if let Some(ref mut c) = current_commit {
            // This is a stats line (e.g., "1 file changed, 5 insertions(+)")
            c["stats"] = Value::String(line.trim().to_string());
        }
    }
    if let Some(c) = current_commit {
        result.push(c);
    }

    Ok(Json(json!({ "commits": result })))
}

/// GET /api/git/commit-diff
pub async fn commit_diff(
    _auth: AuthUser,
    Query(q): Query<CommitDiffQuery>,
) -> Result<Json<Value>, AppError> {
    let path = resolve_project_path(&q.project).await?;
    validate_ref(&q.commit)?;

    let mut output = run_git_lossy(&path, &["show", &q.commit]).await?;

    let max_len = 500_000;
    let is_truncated = output.len() > max_len;
    if is_truncated {
        output.truncate(max_len);
    }

    Ok(Json(json!({
        "diff": output,
        "isTruncated": is_truncated
    })))
}

/// GET /api/git/remote-status
pub async fn remote_status(
    _auth: AuthUser,
    Query(q): Query<ProjectQuery>,
) -> Result<Json<Value>, AppError> {
    let path = resolve_project_path(&q.project).await?;

    let branch = run_git(&path, &["rev-parse", "--abbrev-ref", "HEAD"])
        .await
        .unwrap_or_else(|_| "main".to_string())
        .trim()
        .to_string();

    // Check if remote exists
    let has_remote = run_git(&path, &["remote"]).await.map(|o| !o.trim().is_empty()).unwrap_or(false);

    // Check upstream
    let upstream_result = run_git(
        &path,
        &["rev-parse", "--abbrev-ref", &format!("{branch}@{{upstream}}")],
    )
    .await;
    let has_upstream = upstream_result.is_ok();

    let (ahead, behind) = if has_upstream {
        let count = run_git(
            &path,
            &["rev-list", "--left-right", "--count", &format!("{branch}...{branch}@{{upstream}}")],
        )
        .await
        .unwrap_or_default();

        let parts: Vec<&str> = count.trim().split('\t').collect();
        let a: u32 = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
        let b: u32 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        (a, b)
    } else {
        (0, 0)
    };

    Ok(Json(json!({
        "hasRemote": has_remote,
        "hasUpstream": has_upstream,
        "branch": branch,
        "ahead": ahead,
        "behind": behind,
        "isUpToDate": ahead == 0 && behind == 0
    })))
}

/// POST /api/git/fetch
pub async fn fetch(
    _auth: AuthUser,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<Value>, AppError> {
    let project = body["project"].as_str().unwrap_or("");
    let path = resolve_project_path(project).await?;

    let output = run_git(&path, &["fetch", "--all"]).await?;
    Ok(Json(json!({ "success": true, "output": output })))
}

/// POST /api/git/pull
pub async fn pull(
    _auth: AuthUser,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<Value>, AppError> {
    let project = body["project"].as_str().unwrap_or("");
    let path = resolve_project_path(project).await?;

    let output = run_git(&path, &["pull"]).await?;
    Ok(Json(json!({ "success": true, "output": output })))
}

/// POST /api/git/push
pub async fn push(
    _auth: AuthUser,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<Value>, AppError> {
    let project = body["project"].as_str().unwrap_or("");
    let path = resolve_project_path(project).await?;

    let output = run_git(&path, &["push"]).await?;
    Ok(Json(json!({ "success": true, "output": output })))
}

/// POST /api/git/publish
pub async fn publish(
    _auth: AuthUser,
    Json(body): Json<BranchRequest>,
) -> Result<Json<Value>, AppError> {
    let path = resolve_project_path(&body.project).await?;
    validate_ref(&body.branch)?;

    let output = run_git(&path, &["push", "-u", "origin", &body.branch]).await?;
    Ok(Json(json!({ "success": true, "output": output })))
}

/// POST /api/git/discard
pub async fn discard(
    _auth: AuthUser,
    Json(body): Json<DiscardRequest>,
) -> Result<Json<Value>, AppError> {
    let path = resolve_project_path(&body.project).await?;
    validate_file_path(&body.file, &path)?;

    // Unstage first
    let _ = run_git(&path, &["reset", "HEAD", "--", &body.file]).await;
    // Then checkout
    run_git(&path, &["checkout", "--", &body.file]).await?;

    Ok(Json(json!({ "success": true, "message": "Changes discarded" })))
}

/// POST /api/git/delete-untracked
pub async fn delete_untracked(
    _auth: AuthUser,
    Json(body): Json<DiscardRequest>,
) -> Result<Json<Value>, AppError> {
    let path = resolve_project_path(&body.project).await?;
    let file_path = validate_file_path(&body.file, &path)?;

    if file_path.is_dir() {
        tokio::fs::remove_dir_all(&file_path)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
    } else {
        tokio::fs::remove_file(&file_path)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
    }

    Ok(Json(json!({ "success": true, "message": "File deleted" })))
}
