mod models;
use anyhow::{Context, Result};
use ort::{
    session::{Session, builder::GraphOptimizationLevel},
    value::Tensor,
};
use std::{
    io::{self, BufRead, Read, Write},
    path::Path,
    time::Instant,
};
use tokenizers::{Tokenizer, TruncationParams};

struct Engine {
    tokenizer: Tokenizer,
    model: Session,
}
impl Engine {
    fn load(dir: &Path) -> Result<Self> {
        let mut tokenizer =
            Tokenizer::from_file(dir.join("tokenizer.json")).map_err(|e| anyhow::anyhow!("{e}"))?;
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: 512,
                ..Default::default()
            }))
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let model = Session::builder()?
            .with_optimization_level(GraphOptimizationLevel::Level3)?
            .with_intra_threads(1)?
            .with_inter_threads(1)?
            .with_parallel_execution(false)?
            .with_intra_op_spinning(false)?
            .with_inter_op_spinning(false)?
            .commit_from_file(dir.join("model.onnx"))?;
        Ok(Self { tokenizer, model })
    }
    fn embed(&mut self, text: &str, query: bool) -> Result<Vec<f32>> {
        let text = format!("{}: {text}", if query { "query" } else { "passage" });
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let n = encoding.len();
        let ids = Tensor::from_array((
            [1, n],
            encoding
                .get_ids()
                .iter()
                .map(|&v| i64::from(v))
                .collect::<Vec<_>>(),
        ))?;
        let mask = Tensor::from_array(([1, n], vec![1i64; n]))?;
        let types = Tensor::from_array(([1, n], vec![0i64; n]))?;
        let output = self
            .model
            .run(ort::inputs!["input_ids"=>ids,"attention_mask"=>mask,"token_type_ids"=>types])?;
        let (shape, data) = output[0].try_extract_tensor::<f32>()?;
        let dim = usize::try_from(*shape.last().context("empty output")?)?;
        anyhow::ensure!(
            shape.as_ref() == [1, n as i64, 384],
            "Некорректная форма вектора E5"
        );
        let mut vector = vec![0f32; dim];
        for row in data.chunks_exact(dim) {
            for (a, b) in vector.iter_mut().zip(row) {
                *a += b;
            }
        }
        let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
        anyhow::ensure!(norm.is_finite() && norm > 0., "invalid embedding");
        for v in &mut vector {
            *v /= norm;
        }
        Ok(vector)
    }
}
fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("download") => return models::download(),
        Some("check-model") => return models::check(),
        Some("serve") => {}
        Some("bench") => return benchmark(),
        _ => {
            println!("notetaker-embed: download | check-model | serve [model-dir] | bench");
            return Ok(());
        }
    }
    lower_priority();
    let dir = args
        .next()
        .map(std::path::PathBuf::from)
        .map(Ok)
        .unwrap_or_else(notetaker_core::embedding::model_dir)?;
    let mut engine = Engine::load(&dir)?;
    let mut input = io::stdin().lock();
    loop {
        let mut line = String::new();
        let n = std::io::Read::by_ref(&mut input)
            .take(1_048_577)
            .read_line(&mut line)?;
        if n == 0 {
            break;
        }
        anyhow::ensure!(n <= 1_048_576, "Слишком большой запрос");
        let result = (|| -> Result<Vec<f32>> {
            let request: notetaker_core::embedding::EmbedRequest = serde_json::from_str(&line)?;
            engine.embed(&request.text, request.query)
        })();
        let response = match result {
            Ok(vector) => notetaker_core::embedding::EmbedResponse {
                vector,
                error: None,
            },
            Err(_) => notetaker_core::embedding::EmbedResponse {
                vector: vec![],
                error: Some("Ошибка локальной модели: проверьте модель в настройках".into()),
            },
        };
        let mut output = io::stdout().lock();
        serde_json::to_writer(&mut output, &response)?;
        output.write_all(b"\n")?;
        output.flush()?;
    }
    Ok(())
}
fn lower_priority() {
    #[cfg(unix)]
    #[allow(unsafe_code)]
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 10);
    }
}
fn benchmark() -> Result<()> {
    let start = Instant::now();
    let mut engine = Engine::load(&notetaker_core::embedding::model_dir()?)?;
    eprintln!("load_s={:.3}", start.elapsed().as_secs_f64());
    let texts = [
        "обсуждали бюджет на следующий квартал",
        "говорили про деньги на Q1",
        "сегодня солнечная погода и кошка спит на окне",
    ];
    let mut vectors = Vec::new();
    for text in texts {
        let start = Instant::now();
        let vector = engine.embed(text, false)?;
        eprintln!(
            "embed_ms={:.2} dim={}",
            start.elapsed().as_secs_f64() * 1000.,
            vector.len()
        );
        vectors.push(vector);
    }
    let dot = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
    eprintln!(
        "paraphrase={:.6} unrelated={:.6}",
        dot(&vectors[0], &vectors[1]),
        dot(&vectors[0], &vectors[2])
    );
    for _ in 0..3 {
        let start = Instant::now();
        engine.embed(
            &"Обсудили расходы команды на следующий квартал и план работы. ".repeat(20),
            false,
        )?;
        eprintln!("chunk_ms={:.2}", start.elapsed().as_secs_f64() * 1000.);
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "downloads required: notetaker-embed download; NOTETAKER_MODELS_DIR"]
    fn real_russian_embeddings() -> Result<()> {
        let mut engine = Engine::load(&notetaker_core::embedding::model_dir()?)?;
        let query = engine.embed("обсуждали бюджет на следующий квартал", true)?;
        let related = engine.embed("говорили про деньги на Q1", false)?;
        let unrelated = engine.embed("кот спит на окне, на улице солнечная погода", false)?;
        let dot = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        let good = dot(&query, &related);
        let bad = dot(&query, &unrelated);
        eprintln!("RUSSIAN paraphrase={good:.6} unrelated={bad:.6}");
        assert_eq!(query.len(), 384);
        assert!(good > bad);
        Ok(())
    }
}
