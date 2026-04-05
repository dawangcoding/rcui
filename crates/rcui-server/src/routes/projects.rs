use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Multipart, Path, Query, State};
use axum::response::IntoResponse;
use axum::Json;
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::{debug, info, warn};

use crate::auth::middleware::AuthUser;
use crate::error::AppError;
use crate::services::project_scanner;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct ProjectsQuery {
    pub refresh: Option<bool>,
}

/// GET /api/projects — List all discovered projects.
pub async fn list_projects(
    _auth: AuthUser,
    State(state): State<Arc<AppState>>,
    Query(query): Query<ProjectsQuery>,
) -> Result<Json<Value>, AppError> {
    let refresh = query.refresh.unwrap_or(false);

    // Check cache first
    if !refresh {
        let cache = state.project_cache.read().await;
        if let Some(ref cached) = *cache {
            debug!(count = cached.len(), "Projects cache hit");
            return Ok(Json(Value::Array(cached.clone())));
        }
    }

    debug!(refresh, "Scanning projects");

    // Discover projects
    let projects = project_scanner::get_projects(&state.db, None).await;
    let projects_json: Vec<Value> = projects
        .iter()
        .map(|p| serde_json::to_value(p).unwrap_or_default())
        .collect();

    debug!(count = projects_json.len(), "Projects discovered");

    // Update cache
    {
        let mut cache = state.project_cache.write().await;
        *cache = Some(projects_json.clone());
    }

    Ok(Json(Value::Array(projects_json)))
}

/// POST /api/projects/add — Add a project manually.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddProjectRequest {
    pub project_path: String,
}

pub async fn add_project(
    _auth: AuthUser,
    State(state): State<Arc<AppState>>,
    Json(body): Json<AddProjectRequest>,
) -> Result<Json<Value>, AppError> {
    info!(path = %body.project_path, "Adding project manually");

    let project_name = project_scanner::add_project_manually(&body.project_path).await?;

    // Invalidate cache
    {
        let mut cache = state.project_cache.write().await;
        *cache = None;
    }

    info!(%project_name, "Project added");

    Ok(Json(json!({
        "success": true,
        "projectName": project_name
    })))
}

/// POST /api/projects/create-workspace — Create a workspace (new or existing) and add as project.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateWorkspaceRequest {
    pub workspace_type: String,
    pub path: String,
}

pub async fn create_workspace(
    _auth: AuthUser,
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateWorkspaceRequest>,
) -> Result<Json<Value>, AppError> {
    let workspace_path = body.path.trim().to_string();
    if workspace_path.is_empty() {
        return Err(AppError::BadRequest("Path is required".to_string()));
    }

    // Expand ~ to home directory
    let expanded_path = if workspace_path.starts_with("~/") {
        let home = dirs::home_dir().unwrap_or_default();
        home.join(&workspace_path[2..]).to_string_lossy().to_string()
    } else {
        workspace_path.clone()
    };

    let path = std::path::PathBuf::from(&expanded_path);

    // If workspace type is "new", create the directory
    if body.workspace_type == "new" {
        if !path.exists() {
            tokio::fs::create_dir_all(&path).await.map_err(|e| {
                AppError::BadRequest(format!("Failed to create directory: {e}"))
            })?;
            info!(path = %expanded_path, "Created new workspace directory");
        }
    } else if !path.exists() {
        return Err(AppError::BadRequest(format!(
            "Directory does not exist: {expanded_path}"
        )));
    }

    let project_name = project_scanner::add_project_manually(&expanded_path).await?;

    // Invalidate cache
    {
        let mut cache = state.project_cache.write().await;
        *cache = None;
    }

    info!(%project_name, path = %expanded_path, "Workspace created and project added");

    Ok(Json(json!({
        "success": true,
        "project": {
            "name": project_name,
            "path": expanded_path
        }
    })))
}

/// PUT /api/projects/:projectName/rename — Rename a project.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameProjectRequest {
    pub display_name: String,
}

pub async fn rename_project(
    _auth: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(project_name): Path<String>,
    Json(body): Json<RenameProjectRequest>,
) -> Result<Json<Value>, AppError> {
    info!(%project_name, display_name = %body.display_name, "Renaming project");

    project_scanner::rename_project(&project_name, &body.display_name).await?;

    // Invalidate cache
    {
        let mut cache = state.project_cache.write().await;
        *cache = None;
    }

    Ok(Json(json!({ "success": true })))
}

/// DELETE /api/projects/:projectName — Delete a project.
pub async fn delete_project(
    _auth: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(project_name): Path<String>,
) -> Result<Json<Value>, AppError> {
    info!(%project_name, "Deleting project");

    project_scanner::delete_project(&project_name).await?;

    // Invalidate cache
    {
        let mut cache = state.project_cache.write().await;
        *cache = None;
    }

    Ok(Json(json!({ "success": true })))
}

/// POST /api/projects/:projectName/upload-images — Upload images for chat attachments.
///
/// Accepts multipart form data with field "images" containing image files.
/// Returns base64-encoded image data for passing to AI providers.
const MAX_IMAGE_SIZE: usize = 5 * 1024 * 1024; // 5MB
const MAX_IMAGE_COUNT: usize = 5;

pub async fn upload_images(
    _auth: AuthUser,
    Path(_project_name): Path<String>,
    mut multipart: Multipart,
) -> Result<Json<Value>, AppError> {
    let mut images = Vec::new();

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(format!("Invalid multipart data: {e}")))?
    {
        let field_name = field.name().unwrap_or("").to_string();
        if field_name != "images" {
            continue;
        }

        if images.len() >= MAX_IMAGE_COUNT {
            warn!("Too many images, max {MAX_IMAGE_COUNT}");
            break;
        }

        let file_name = field
            .file_name()
            .unwrap_or("image.png")
            .to_string();

        let content_type = field
            .content_type()
            .unwrap_or("application/octet-stream")
            .to_string();

        // Validate MIME type
        if !content_type.starts_with("image/") {
            return Err(AppError::BadRequest(format!(
                "Invalid file type: {content_type}. Only images are allowed."
            )));
        }

        let data = field
            .bytes()
            .await
            .map_err(|e| AppError::BadRequest(format!("Failed to read image data: {e}")))?;

        if data.len() > MAX_IMAGE_SIZE {
            return Err(AppError::BadRequest(format!(
                "Image '{}' exceeds maximum size of 5MB",
                file_name
            )));
        }

        // Determine MIME type: prefer content-type header, fallback to extension guess
        let mime_type = if content_type != "application/octet-stream" {
            content_type
        } else {
            mime_guess::from_path(&file_name)
                .first_or_octet_stream()
                .to_string()
        };

        let encoded = base64::engine::general_purpose::STANDARD.encode(&data);
        let data_uri = format!("data:{mime_type};base64,{encoded}");

        images.push(json!({
            "name": file_name,
            "data": data_uri,
            "size": data.len(),
            "mimeType": mime_type,
        }));

        debug!(name = %file_name, size = data.len(), "Image uploaded");
    }

    if images.is_empty() {
        return Err(AppError::BadRequest(
            "No images provided".to_string(),
        ));
    }

    info!(count = images.len(), "Images uploaded successfully");

    Ok(Json(json!({ "images": images })))
}

// ─── File Reading ────────────────────────────────────────────────────────────

/// Resolve project name to its root directory path.
async fn resolve_project_root(project_name: &str) -> Result<PathBuf, AppError> {
    let dir = project_scanner::extract_project_directory(project_name).await;
    let path = PathBuf::from(&dir);
    if !path.exists() {
        return Err(AppError::NotFound(format!(
            "Project directory not found: {dir}"
        )));
    }
    Ok(path)
}

/// Validate that a file path is within the project root (prevent path traversal).
fn validate_within_project(file_path: &str, project_root: &PathBuf) -> Result<PathBuf, AppError> {
    if file_path.contains('\0') {
        return Err(AppError::BadRequest("Invalid file path".into()));
    }

    let resolved = if std::path::Path::new(file_path).is_absolute() {
        PathBuf::from(file_path)
    } else {
        project_root.join(file_path)
    };

    let canonical_root = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.clone());
    let canonical_file = resolved
        .canonicalize()
        .map_err(|_| AppError::NotFound("File not found".into()))?;

    if !canonical_file.starts_with(&canonical_root) {
        return Err(AppError::BadRequest("Path traversal detected".into()));
    }

    Ok(canonical_file)
}

/// GET /api/projects/:projectName/file?filePath=... — Read a text file (JSON response).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadFileQuery {
    pub file_path: String,
}

pub async fn read_file(
    _auth: AuthUser,
    Path(project_name): Path<String>,
    Query(query): Query<ReadFileQuery>,
) -> Result<Json<Value>, AppError> {
    let project_root = resolve_project_root(&project_name).await?;
    let file_path = validate_within_project(&query.file_path, &project_root)?;

    let content = tokio::fs::read_to_string(&file_path).await.map_err(|e| {
        match e.kind() {
            std::io::ErrorKind::NotFound => {
                AppError::NotFound(format!("File not found: {}", file_path.display()))
            }
            std::io::ErrorKind::PermissionDenied => {
                AppError::BadRequest("Permission denied".into())
            }
            std::io::ErrorKind::InvalidData => {
                AppError::BadRequest("File is binary and cannot be read as text".into())
            }
            _ => AppError::BadRequest(format!("Cannot read file as text: {e}")),
        }
    })?;

    Ok(Json(json!({
        "content": content,
        "path": file_path.display().to_string(),
    })))
}

/// GET /api/projects/:projectName/files/content?path=... — Read a file as binary (raw response).
#[derive(Deserialize)]
pub struct FileContentQuery {
    pub path: String,
}

pub async fn read_file_content(
    _auth: AuthUser,
    Path(project_name): Path<String>,
    Query(query): Query<FileContentQuery>,
) -> Result<impl IntoResponse, AppError> {
    let project_root = resolve_project_root(&project_name).await?;
    let file_path = validate_within_project(&query.path, &project_root)?;

    let data = tokio::fs::read(&file_path).await.map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => {
            AppError::NotFound(format!("File not found: {}", file_path.display()))
        }
        std::io::ErrorKind::PermissionDenied => {
            AppError::BadRequest("Permission denied".into())
        }
        _ => AppError::Internal(e.into()),
    })?;

    let mime = mime_guess::from_path(&file_path)
        .first_or_octet_stream()
        .to_string();

    Ok(([(axum::http::header::CONTENT_TYPE, mime)], data))
}

/// GET /api/files/raw?path=... — Read a file by absolute path as binary (authenticated).
/// Used for serving image previews from session conversations where the file path
/// may be outside the project directory (e.g. .tmp/images/).
#[derive(Deserialize)]
pub struct RawFileQuery {
    pub path: String,
}

pub async fn read_raw_file(
    _auth: AuthUser,
    Query(query): Query<RawFileQuery>,
) -> Result<impl IntoResponse, AppError> {
    if query.path.contains('\0') {
        return Err(AppError::BadRequest("Invalid file path".into()));
    }

    let file_path = std::path::Path::new(&query.path);
    if !file_path.is_absolute() {
        return Err(AppError::BadRequest("Path must be absolute".into()));
    }

    let canonical = file_path
        .canonicalize()
        .map_err(|_| AppError::NotFound("File not found".into()))?;

    let data = tokio::fs::read(&canonical).await.map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => {
            AppError::NotFound(format!("File not found: {}", canonical.display()))
        }
        std::io::ErrorKind::PermissionDenied => {
            AppError::BadRequest("Permission denied".into())
        }
        _ => AppError::Internal(e.into()),
    })?;

    let mime = mime_guess::from_path(&canonical)
        .first_or_octet_stream()
        .to_string();

    Ok(([(axum::http::header::CONTENT_TYPE, mime)], data))
}
