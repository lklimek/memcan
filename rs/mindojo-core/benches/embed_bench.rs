use std::time::{Duration, Instant};

use fastembed::EmbeddingModel;
use mindojo_core::embed::FastEmbedProvider;
use mindojo_core::traits::EmbeddingProvider;

const ROUNDS: usize = 10;

struct ModelSpec {
    name: &'static str,
    model: EmbeddingModel,
    dims: usize,
}

fn test_texts() -> Vec<String> {
    vec![
        "Rust borrow checker prevents data races at compile time".to_string(),
        "LanceDB stores vectors in columnar Arrow format".to_string(),
        "The fastembed crate runs ONNX models in-process without external services".to_string(),
        "Multilingual embedding models support cross-language semantic search".to_string(),
        "Claude Code plugins extend the CLI with custom skills and hooks".to_string(),
    ]
}

fn format_duration(d: Duration) -> String {
    let ms = d.as_secs_f64() * 1000.0;
    if ms >= 1000.0 {
        format!("{:.2}s", ms / 1000.0)
    } else {
        format!("{:.1}ms", ms)
    }
}

async fn bench_model(spec: &ModelSpec, texts: &[String]) -> Vec<Duration> {
    let provider =
        FastEmbedProvider::new(spec.model.clone(), spec.dims).expect("failed to create provider");

    // Warm up
    provider.embed(texts).await.expect("warm-up embed failed");

    let mut timings = Vec::with_capacity(ROUNDS);
    for _ in 0..ROUNDS {
        let start = Instant::now();
        provider.embed(texts).await.expect("embed failed");
        timings.push(start.elapsed());
    }
    timings
}

fn stats(timings: &[Duration]) -> (Duration, Duration, Duration, Duration) {
    let mut sorted: Vec<Duration> = timings.to_vec();
    sorted.sort();
    let min = sorted[0];
    let max = *sorted.last().unwrap();
    let sum: Duration = sorted.iter().sum();
    let mean = sum / sorted.len() as u32;
    let median = if sorted.len().is_multiple_of(2) {
        (sorted[sorted.len() / 2 - 1] + sorted[sorted.len() / 2]) / 2
    } else {
        sorted[sorted.len() / 2]
    };
    (min, max, mean, median)
}

#[tokio::main]
async fn main() {
    let models = vec![
        ModelSpec {
            name: "AllMiniLML6V2",
            model: EmbeddingModel::AllMiniLML6V2,
            dims: 384,
        },
        ModelSpec {
            name: "MultilingualE5Small",
            model: EmbeddingModel::MultilingualE5Small,
            dims: 384,
        },
        ModelSpec {
            name: "MultilingualE5Base",
            model: EmbeddingModel::MultilingualE5Base,
            dims: 768,
        },
        ModelSpec {
            name: "MultilingualE5Large",
            model: EmbeddingModel::MultilingualE5Large,
            dims: 1024,
        },
    ];

    let texts = test_texts();

    println!(
        "\nEmbedding Model Benchmark ({} rounds, {} texts per round)",
        ROUNDS,
        texts.len()
    );
    println!("{}", "\u{2500}".repeat(75));
    println!(
        "{:<25} {:>5}   {:>9} {:>9} {:>9} {:>9}",
        "Model", "Dims", "Min", "Max", "Mean", "Median"
    );
    println!("{}", "\u{2500}".repeat(75));

    for spec in &models {
        let timings = bench_model(spec, &texts).await;
        let (min, max, mean, median) = stats(&timings);
        println!(
            "{:<25} {:>5}   {:>9} {:>9} {:>9} {:>9}",
            spec.name,
            spec.dims,
            format_duration(min),
            format_duration(max),
            format_duration(mean),
            format_duration(median),
        );
    }

    println!("{}", "\u{2500}".repeat(75));
}
