//! Real built-in AI: needs `notetaker-asr download local-llm` in NOTETAKER_MODELS_DIR.
use notetaker_core::Mode;
use notetaker_llm::{Cancellation, CompletionRequest, Config, create_provider};

#[test]
#[ignore = "downloads nothing; needs an installed local-llm and several minutes on CPU"]
fn builtin_server_starts_on_demand_and_answers_on_loopback() {
    let config = Config::parse(
        "[defaults]\nlocal='builtin'\nhybrid='builtin'\n[[providers]]\nid='builtin'\nkind='builtin'\nmodel='qwen3.5-4b'\ncontext_tokens=16384",
    )
    .unwrap();
    let provider = create_provider(
        config.providers[0].clone(),
        Mode::Local,
        Cancellation::default(),
    )
    .unwrap();
    assert!(provider.config().base_url.starts_with("http://127.0.0.1:"));
    let answer = provider
        .complete(
            &CompletionRequest {
                system: "Отвечай одним словом, без пояснений.".into(),
                user: "Столица Франции?".into(),
                max_output_tokens: 16,
            },
            &mut |_| {},
        )
        .unwrap();
    assert!(answer.contains("Париж"), "{answer}");
    notetaker_runtime::process::terminate_all();
}
