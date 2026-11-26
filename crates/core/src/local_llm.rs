//! Paths of the built-in local AI, installed by `notetaker-asr download local-llm`.
use crate::Result;
use notetaker_runtime::local_llm::{MODEL_FILE, MODEL_ID, SERVER, SERVER_ID};
use std::path::PathBuf;

pub fn model_path() -> Result<PathBuf> {
    Ok(crate::embedding::models_dir()?
        .join(MODEL_ID)
        .join(MODEL_FILE))
}

pub fn server_path() -> Result<PathBuf> {
    Ok(crate::embedding::models_dir()?.join(SERVER_ID).join(SERVER))
}
