use std::time::{Duration, Instant};

use fastembed::EmbeddingModel;
use mindojo_core::embed::FastEmbedProvider;
use mindojo_core::traits::EmbeddingProvider;

struct ModelSpec {
    name: &'static str,
    model: EmbeddingModel,
    dims: usize,
    max_median_ms: u64,
}

fn models_to_test() -> Vec<ModelSpec> {
    vec![
        ModelSpec {
            name: "MultilingualE5Large",
            model: EmbeddingModel::MultilingualE5Large,
            dims: 1024,
            max_median_ms: 500,
        },
        // Add more models here to extend the benchmark
    ]
}

fn test_texts() -> Vec<String> {
    vec![
        "Rust borrow checker prevents data races at compile time".into(),
        "LanceDB stores vectors in columnar Arrow format".into(),
        "The fastembed crate runs ONNX models in-process without external services".into(),
        "Multilingual embedding models support cross-language semantic search".into(),
        "Claude Code plugins extend the CLI with custom skills and hooks".into(),
    ]
}

#[tokio::test]
#[ignore]
async fn test_embed_performance() {
    let texts = test_texts();
    let rounds = 10;

    println!();
    println!(
        "{:<25} {:>8} {:>8} {:>8} {:>8} {:>10}",
        "Model", "Min(ms)", "Max(ms)", "Mean(ms)", "Med(ms)", "Threshold"
    );
    println!("{}", "-".repeat(77));

    for spec in models_to_test() {
        let provider = FastEmbedProvider::new(spec.model, spec.dims)
            .unwrap_or_else(|e| panic!("Failed to load model {}: {e}", spec.name));

        // Warm up
        provider
            .embed(&texts)
            .await
            .unwrap_or_else(|e| panic!("Warm-up failed for {}: {e}", spec.name));

        let mut durations: Vec<Duration> = Vec::with_capacity(rounds);
        for _ in 0..rounds {
            let start = Instant::now();
            provider
                .embed(&texts)
                .await
                .unwrap_or_else(|e| panic!("Embed failed for {}: {e}", spec.name));
            durations.push(start.elapsed());
        }

        durations.sort();

        let min = durations[0];
        let max = durations[rounds - 1];
        let sum: Duration = durations.iter().sum();
        let mean = sum / rounds as u32;
        let median = if rounds % 2 == 0 {
            (durations[rounds / 2 - 1] + durations[rounds / 2]) / 2
        } else {
            durations[rounds / 2]
        };

        println!(
            "{:<25} {:>8.1} {:>8.1} {:>8.1} {:>8.1} {:>10}",
            spec.name,
            min.as_secs_f64() * 1000.0,
            max.as_secs_f64() * 1000.0,
            mean.as_secs_f64() * 1000.0,
            median.as_secs_f64() * 1000.0,
            format!("{}ms", spec.max_median_ms),
        );

        assert!(
            median.as_millis() < spec.max_median_ms as u128,
            "{}: median {:.1}ms exceeds threshold {}ms",
            spec.name,
            median.as_secs_f64() * 1000.0,
            spec.max_median_ms
        );
    }
}
