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

    let input = Tensor::from_array((
        [1usize, 3, MODEL_HEIGHT, MODEL_WIDTH],
        vec![0.0_f32; 3 * MODEL_HEIGHT * MODEL_WIDTH],
    ))?;

    let started = Instant::now();

    let outputs = session.run(ort::inputs![input])?;

    let inference_elapsed = started.elapsed();

    if outputs.is_empty() {
        return Err(io::Error::other("YOLO model returned no outputs").into());
    }

    let (shape, data) = outputs[0].try_extract_tensor::<f32>()?;

    println!("Inference completed.");
    println!("Output shape: {shape:?}");
    println!("Output values: {}", data.len());
    println!(
        "Inference time: {:.2} ms",
        inference_elapsed.as_secs_f64() * 1000.0
    );

    if shape.as_ref() == [1, 300, 6] {
        println!("YOLO26 end-to-end output format confirmed.");
    } else {
        println!("WARNING: expected YOLO26 output shape [1, 300, 6].");
    }

    Ok(())
}
