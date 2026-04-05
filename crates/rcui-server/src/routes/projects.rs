use std::sync::Arc;

use axum::extract::{Multipart, Path, Query, State};
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
