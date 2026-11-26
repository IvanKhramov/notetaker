//! Pinned names of the built-in local AI, shared by the installer and the app.

/// Directory name of the weights under the models directory.
pub const MODEL_ID: &str = "qwen3.5-4b";
pub const MODEL_FILE: &str = "Qwen3.5-4B-Q4_K_M.gguf";
/// Context the server allocates; longer meetings go through map-reduce.
pub const CONTEXT_TOKENS: usize = 16_384;
/// Directory name of the unpacked llama.cpp release under the models directory.
pub const SERVER_ID: &str = "llama-server";
/// The server inside its unpacked release archive.
#[cfg(windows)]
pub const SERVER: &str = "llama-server.exe";
#[cfg(not(windows))]
pub const SERVER: &str = "llama-b12292/llama-server";
