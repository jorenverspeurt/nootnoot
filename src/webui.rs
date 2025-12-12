use axum::{
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::get,
    Json, Router,
};

use crate::WebState;

#[derive(Clone)]
pub struct AppWebState {
    pub web_state: WebState,
}

// Embed the HTML at compile time.
// Path is relative to this file: src/webui.rs -> ../static/index.html
static INDEX_HTML: &str = include_str!("../static/index.html");

pub fn build_router(web_state: WebState) -> Router {
    let app_state = AppWebState { web_state };

    Router::new()
        .route("/", get(handler_index))
        .route("/api/state", get(handler_state))
        .with_state(app_state)
}

async fn handler_index() -> impl IntoResponse {
    Html(INDEX_HTML)
}

async fn handler_state(State(state): State<AppWebState>) -> impl IntoResponse {
    let snapshot = state.web_state.snapshot();
    (StatusCode::OK, Json(snapshot))
}
