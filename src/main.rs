//! Code Sandbox API — submit code, run it in a wasmtime sandbox, get output.

mod api;
mod runtime;
mod transpile;

use std::sync::Arc;

use axum::{
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::{get, post},
    Json, Router,
};
use tower_http::trace::TraceLayer;

use api::{ErrorResponse, Language, RunRequest};
use runtime::{Limits, Sandbox};

#[derive(Clone)]
struct AppState {
    sandbox: Arc<Sandbox>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,code_sandbox=debug".into()),
        )
        .init();

    // Runtime module locations (overridable via env).
    let python_wasm = std::env::var("PYTHON_WASM").ok();
    let qjs_wasm = std::env::var("QJS_WASM").ok();

    // Default to the bundled runtimes/ dir if present and not overridden.
    let python_wasm = python_wasm.or_else(|| exists("runtimes/python.wasm"));
    let qjs_wasm = qjs_wasm.or_else(|| exists("runtimes/qjs.wasm"));

    if python_wasm.is_none() && qjs_wasm.is_none() {
        anyhow::bail!("no runtimes found: set PYTHON_WASM and/or QJS_WASM, or place them in runtimes/");
    }

    // Optional shared dir of pure-Python packages, mounted read-only for every
    // Python run. Default to runtimes/py-site-packages if present.
    let py_packages = std::env::var("PY_PACKAGES")
        .ok()
        .or_else(|| exists("runtimes/py-site-packages"))
        .map(std::path::PathBuf::from);

    tracing::info!(?python_wasm, ?qjs_wasm, ?py_packages, "loading runtimes");
    let sandbox = Sandbox::new(
        python_wasm.as_deref(),
        qjs_wasm.as_deref(),
        py_packages,
        Limits::default(),
    )?;
    tracing::info!("runtimes compiled and warm");

    let state = AppState {
        sandbox: Arc::new(sandbox),
    };

    let mut app = Router::new()
        .route("/health", get(health))
        .route("/languages", get(languages))
        .route("/run", post(run));

    // Browser playground — opt-in via CONSOLE=1 so it's never deployed by
    // accident. It exposes a UI that drives /run; keep it off in production.
    if env_flag("CONSOLE") {
        tracing::warn!("CONSOLE enabled: browser playground served at /console — do NOT enable in production");
        app = app.route("/console", get(console));
    }

    let app = app
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr = std::env::var("BIND").unwrap_or_else(|_| "127.0.0.1:8080".into());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

fn exists(path: &str) -> Option<String> {
    std::path::Path::new(path)
        .exists()
        .then(|| path.to_string())
}

/// Read a boolean-ish env flag (`1`/`true`/`yes`, case-insensitive).
fn env_flag(name: &str) -> bool {
    matches!(
        std::env::var(name).unwrap_or_default().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

async fn console() -> impl IntoResponse {
    Html(include_str!("console.html"))
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn languages(State(state): State<AppState>) -> impl IntoResponse {
    let mut langs = Vec::new();
    for (name, lang) in [
        ("python", Language::Python),
        ("typescript", Language::Typescript),
        ("javascript", Language::Javascript),
    ] {
        if state.sandbox.supports(lang) {
            langs.push(name);
        }
    }
    Json(serde_json::json!({ "languages": langs }))
}

async fn run(
    State(state): State<AppState>,
    Json(req): Json<RunRequest>,
) -> impl IntoResponse {
    if !state.sandbox.supports(req.language) {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: format!("language {:?} is not available on this server", req.language),
            }),
        )
            .into_response();
    }

    // Run the (blocking) wasm execution off the async runtime threads.
    let sandbox = state.sandbox.clone();
    let result = tokio::task::spawn_blocking(move || {
        sandbox.execute(
            req.language,
            &req.code,
            req.stdin.as_deref(),
            &req.args,
            req.timeout_ms,
        )
    })
    .await;

    match result {
        Ok(resp) => (StatusCode::OK, Json(resp)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("execution task failed: {e}"),
            }),
        )
            .into_response(),
    }
}
