use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::{debug, info};

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
