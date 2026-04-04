use axum::Json;
use chrono::Utc;
use serde_json::{Value, json};

/// GET /health
pub async fn health_check() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "timestamp": Utc::now().to_rfc3339(),
    }))
}
