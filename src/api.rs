//! HTTP request/response types for the sandbox API.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Python,
    #[serde(alias = "ts")]
    Typescript,
    #[serde(alias = "js")]
    Javascript,
}

/// A code execution request.
#[derive(Debug, Deserialize)]
pub struct RunRequest {
    pub language: Language,
    /// Source code to execute.
    pub code: String,
    /// Optional data piped to the program's stdin.
    #[serde(default)]
    pub stdin: Option<String>,
    /// Extra CLI-style args exposed to the program.
    #[serde(default)]
    pub args: Vec<String>,
    /// Wall-clock timeout in milliseconds. Clamped to a server maximum.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// The result of executing a program.
#[derive(Debug, Serialize)]
pub struct RunResponse {
    pub stdout: String,
    pub stderr: String,
    /// Process exit code. `None` when the program trapped or was killed.
    pub exit_code: Option<i32>,
    pub duration_ms: u128,
    /// True when the run exceeded its wall-clock deadline.
    pub timed_out: bool,
    /// True when the guest exceeded its memory limit or otherwise trapped.
    pub trapped: bool,
    /// Populated with the transpiler / setup error when a run never started.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
}
