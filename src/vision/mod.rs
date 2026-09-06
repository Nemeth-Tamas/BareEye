use ort::ep::ExecutionProvider;
use ort::session::Session;
use ort::value::Tensor;
use std::error::Error;
use std::io;
use std::path::Path;
use std::time::Instant;

const MODEL_WIDTH: usize = 640;
const MODEL_HEIGHT: usize = 640;

pub fn smoke_test_model(model_path: impl AsRef<Path>) -> Result<(), Box<dyn Error>> {
    let model_path = model_path.as_ref();

    if !model_path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("vision model not found: {}", model_path.display()),
        )
        .into());
    }

    println!();
    println!("BareEye vision smoke test");
    println!("=========================");
    println!("Model: {}", model_path.display());

    let mut builder = Session::builder()?;

    ort::ep::CUDA::default().register(&mut builder)?;

    println!("CUDA execution provider registered.");

    let mut session = builder.commit_from_file(model_path)?;

    println!("ONNX model loaded.");

    println!("Running CUDA warm-up inference...");

    {
        let input = Tensor::from_array((
            [1usize, 3, MODEL_HEIGHT, MODEL_WIDTH],
            vec![0.0_f32; 3 * MODEL_HEIGHT * MODEL_WIDTH],
        ))?;

        let outputs = session.run(ort::inputs![input])?;

        if outputs.len() == 0 {
            return Err(io::Error::other("YOLO model returned no outputs").into());
        }

        let (shape, data) = outputs[0].try_extract_tensor::<f32>()?;

        println!("Output shape: {shape:?}");
        println!("Output values: {}", data.len());

        if shape.as_ref() == [1, 300, 6] {
            println!("YOLO26 end-to-end output format confirmed.");
        } else {
            println!("WARNING: expected YOLO26 output shape [1, 300, 6].");
        }
    }

    const BENCHMARK_RUNS: usize = 10;

    let mut total_ms = 0.0_f64;
    let mut minimum_ms = f64::INFINITY;
    let mut maximum_ms = 0.0_f64;

    println!();
    println!("Running {BENCHMARK_RUNS} measured inferences...");

    for run in 1..=BENCHMARK_RUNS {
        let input = Tensor::from_array((
            [1usize, 3, MODEL_HEIGHT, MODEL_WIDTH],
            vec![0.0_f32; 3 * MODEL_HEIGHT * MODEL_WIDTH],
        ))?;

        let started = Instant::now();
        let outputs = session.run(ort::inputs![input])?;
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;

        if outputs.len() == 0 {
            return Err(io::Error::other("YOLO model returned no outputs").into());
        }

        total_ms += elapsed_ms;
        minimum_ms = minimum_ms.min(elapsed_ms);
        maximum_ms = maximum_ms.max(elapsed_ms);

        println!("Run {run:2}: {elapsed_ms:8.2} ms");
    }

    let average_ms = total_ms / BENCHMARK_RUNS as f64;

    println!();
    println!("Steady-state inference benchmark");
    println!("-------------------------------");
    println!("Minimum: {minimum_ms:.2} ms");
    println!("Average: {average_ms:.2} ms");
    println!("Maximum: {maximum_ms:.2} ms");
    println!("Equivalent rate: {:.1} FPS", 1000.0 / average_ms);

    Ok(())
}
