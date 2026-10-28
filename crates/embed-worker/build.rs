// This worker uses the SDK's monolithic CPU ORT in its own process. No sherpa
// Rust dependency and no runtime DLL; the existing Windows SDK is static MT.
use std::{env, path::PathBuf};
fn main() {
    println!("cargo:rerun-if-env-changed=SHERPA_ONNX_LIB_DIR");
    let dir=PathBuf::from(env::var_os("SHERPA_ONNX_LIB_DIR").expect("Run scripts/fetch-sherpa-libs.sh and set SHERPA_ONNX_LIB_DIR (also for the embedding worker)"));
    println!("cargo:rustc-link-search=native={}", dir.display());
    println!("cargo:rustc-link-lib=static=onnxruntime");
    match env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("linux") => {
            for lib in ["stdc++", "m", "pthread", "dl"] {
                println!("cargo:rustc-link-lib=dylib={lib}");
            }
        }
        Ok("macos") => {
            println!("cargo:rustc-link-lib=dylib=c++");
            println!("cargo:rustc-link-lib=framework=Foundation");
        }
        _ => {}
    }
}
