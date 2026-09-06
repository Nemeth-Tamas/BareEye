use eframe::egui::ColorImage;
use ort::ep::ExecutionProvider;
use ort::session::Session;
use ort::value::Tensor;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread;
use std::time::Instant;

const MODEL_WIDTH: usize = 640;
const MODEL_HEIGHT: usize = 640;
const DETECTOR_CLASS_ID: i32 = 0;
const PERSON_CONFIDENCE_THRESHOLD: f32 = 0.25;
const FACE_CONFIDENCE_THRESHOLD: f32 = 0.25;
const LETTERBOX_VALUE: f32 = 114.0 / 255.0;

enum VisionSignal {
    Frame,
    Shutdown,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DetectionKind {
    Person,
    Face,
}

impl DetectionKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Person => "person",
            Self::Face => "face",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Detection {
    pub kind: DetectionKind,
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
    pub confidence: f32,
}

#[derive(Clone, Debug, Default)]
pub struct VisionSnapshot {
    pub ready: bool,
    pub detections: Vec<Detection>,
    pub preprocess_ms: f32,
    pub inference_ms: f32,
    pub face_inference_ms: f32,
    pub processed_frames: u64,
    pub replaced_frames: u64,
    pub last_error: Option<String>,
}

struct Shared {
    latest_frame: Mutex<Option<Arc<ColorImage>>>,
    snapshot: Mutex<VisionSnapshot>,
}

#[derive(Clone)]
pub struct VisionInput {
    sender: mpsc::SyncSender<VisionSignal>,
    shared: Arc<Shared>,
}

impl VisionInput {
    pub fn submit(&self, frame: Arc<ColorImage>) {
        let replaced = {
            let mut latest = self
                .shared
                .latest_frame
                .lock()
                .unwrap_or_else(PoisonError::into_inner);

            latest.replace(frame).is_some()
        };

        if replaced {
            self.shared
                .snapshot
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .replaced_frames += 1;
        }

        let _ = self.sender.try_send(VisionSignal::Frame);
    }
}

pub struct VisionWorker {
    sender: mpsc::SyncSender<VisionSignal>,
    shared: Arc<Shared>,
    worker: Option<thread::JoinHandle<()>>,
}

impl VisionWorker {
    pub fn spawn(
        person_model_path: impl Into<PathBuf>,
        face_model_path: impl Into<PathBuf>,
    ) -> Self {
        let person_model_path = person_model_path.into();
        let face_model_path = face_model_path.into();

        let shared = Arc::new(Shared {
            latest_frame: Mutex::new(None),
            snapshot: Mutex::new(VisionSnapshot::default()),
        });

        let worker_shared = Arc::clone(&shared);
        let (sender, receiver) = mpsc::sync_channel(1);
        let worker_sender = sender.clone();

        let worker = thread::Builder::new()
            .name("bareeye-vision".to_owned())
            .spawn(move || {
                run_worker(person_model_path, face_model_path, worker_shared, receiver);
            })
            .expect("failed to start BareEye vision worker");

        Self {
            sender: worker_sender,
            shared,
            worker: Some(worker),
        }
    }

    pub fn input(&self) -> VisionInput {
        VisionInput {
            sender: self.sender.clone(),
            shared: Arc::clone(&self.shared),
        }
    }

    pub fn snapshot(&self) -> VisionSnapshot {
        self.shared
            .snapshot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Drop for VisionWorker {
    fn drop(&mut self) {
        let _ = self.sender.send(VisionSignal::Shutdown);

        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct Letterbox {
    scale: f32,
    pad_x: f32,
    pad_y: f32,
    source_width: usize,
    source_height: usize,
}

fn create_cuda_session(model_path: &PathBuf, label: &str) -> Result<Session, String> {
    let mut builder =
        Session::builder().map_err(|error| format!("{label} session builder failed: {error}"))?;

    ort::ep::CUDA::default()
        .register(&mut builder)
        .map_err(|error| format!("{label} CUDA provider failed: {error}"))?;

    builder.commit_from_file(model_path).map_err(|error| {
        format!(
            "Could not load {label} model {}: {error}",
            model_path.display()
        )
    })
}

fn warmup_session(session: &mut Session, label: &str) -> Result<(), String> {
    let input = Tensor::from_array((
        [1usize, 3, MODEL_HEIGHT, MODEL_WIDTH],
        vec![0.0_f32; 3 * MODEL_HEIGHT * MODEL_WIDTH],
    ))
    .map_err(|error| format!("Could not build {label} warm-up tensor: {error}"))?;

    session
        .run(ort::inputs![input])
        .map_err(|error| format!("{label} warm-up failed: {error}"))?;

    Ok(())
}

fn run_worker(
    person_model_path: PathBuf,
    face_model_path: PathBuf,
    shared: Arc<Shared>,
    receiver: mpsc::Receiver<VisionSignal>,
) {
    let mut person_session = match create_cuda_session(&person_model_path, "person") {
        Ok(session) => session,
        Err(error) => {
            set_error(&shared, error);
            return;
        }
    };

    let mut face_session = match create_cuda_session(&face_model_path, "face") {
        Ok(session) => session,
        Err(error) => {
            set_error(&shared, error);
            return;
        }
    };

    if let Err(error) = warmup_session(&mut person_session, "person") {
        set_error(&shared, error);
        return;
    }

    if let Err(error) = warmup_session(&mut face_session, "face") {
        set_error(&shared, error);
        return;
    }

    {
        let mut snapshot = shared
            .snapshot
            .lock()
            .unwrap_or_else(PoisonError::into_inner);

        snapshot.ready = true;
        snapshot.last_error = None;
    }

    while let Ok(signal) = receiver.recv() {
        match signal {
            VisionSignal::Frame => {
                let frame = shared
                    .latest_frame
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take();

                let Some(frame) = frame else {
                    continue;
                };

                process_frame(&mut person_session, &mut face_session, &shared, &frame);
            }
            VisionSignal::Shutdown => break,
        }
    }
}

fn process_frame(
    person_session: &mut Session,
    face_session: &mut Session,
    shared: &Arc<Shared>,
    frame: &ColorImage,
) {
    let preprocess_started = Instant::now();

    let (input_data, letterbox) = match preprocess(frame) {
        Ok(result) => result,
        Err(error) => {
            set_error(shared, error);
            return;
        }
    };

    let preprocess_ms = preprocess_started.elapsed().as_secs_f32() * 1000.0;

    let face_input_data = input_data.clone();

    let (mut detections, inference_ms) = match run_detector(
        person_session,
        input_data,
        &letterbox,
        DetectionKind::Person,
        PERSON_CONFIDENCE_THRESHOLD,
    ) {
        Ok(result) => result,
        Err(error) => {
            set_error(shared, error);
            return;
        }
    };

    let (face_detections, face_inference_ms) = match run_detector(
        face_session,
        face_input_data,
        &letterbox,
        DetectionKind::Face,
        FACE_CONFIDENCE_THRESHOLD,
    ) {
        Ok(result) => result,
        Err(error) => {
            set_error(shared, error);
            return;
        }
    };

    detections.extend(face_detections);

    let mut snapshot = shared
        .snapshot
        .lock()
        .unwrap_or_else(PoisonError::into_inner);

    snapshot.ready = true;
    snapshot.detections = detections;
    snapshot.preprocess_ms = preprocess_ms;
    snapshot.inference_ms = inference_ms;
    snapshot.face_inference_ms = face_inference_ms;
    snapshot.processed_frames += 1;
    snapshot.last_error = None;
}

fn run_detector(
    session: &mut Session,
    input_data: Vec<f32>,
    letterbox: &Letterbox,
    kind: DetectionKind,
    confidence_threshold: f32,
) -> Result<(Vec<Detection>, f32), String> {
    let input = Tensor::from_array(([1usize, 3, MODEL_HEIGHT, MODEL_WIDTH], input_data))
        .map_err(|error| format!("Could not build {} tensor: {error}", kind.label()))?;

    let inference_started = Instant::now();

    let outputs = session
        .run(ort::inputs![input])
        .map_err(|error| format!("{} inference failed: {error}", kind.label()))?;

    let inference_ms = inference_started.elapsed().as_secs_f32() * 1000.0;

    if outputs.len() == 0 {
        return Err(format!("{} detector returned no outputs", kind.label()));
    }

    let (shape, data) = outputs[0]
        .try_extract_tensor::<f32>()
        .map_err(|error| format!("Could not read {} output: {error}", kind.label()))?;

    if shape.as_ref() != [1, 300, 6] {
        return Err(format!(
            "Unexpected {} output shape: {shape:?}",
            kind.label()
        ));
    }

    Ok((
        decode_detections(data, letterbox, kind, confidence_threshold),
        inference_ms,
    ))
}

fn preprocess(image: &ColorImage) -> Result<(Vec<f32>, Letterbox), String> {
    let source_width = image.width();
    let source_height = image.height();

    if source_width == 0 || source_height == 0 {
        return Err("Vision received an empty image".to_owned());
    }

    let scale =
        (MODEL_WIDTH as f32 / source_width as f32).min(MODEL_HEIGHT as f32 / source_height as f32);

    let resized_width = ((source_width as f32 * scale).round() as usize).clamp(1, MODEL_WIDTH);

    let resized_height = ((source_height as f32 * scale).round() as usize).clamp(1, MODEL_HEIGHT);

    let pad_x = (MODEL_WIDTH - resized_width) / 2;
    let pad_y = (MODEL_HEIGHT - resized_height) / 2;

    let plane_size = MODEL_WIDTH * MODEL_HEIGHT;

    let mut input = vec![LETTERBOX_VALUE; 3 * plane_size];

    let rgba = image.as_raw();

    if rgba.len() < source_width * source_height * 4 {
        return Err("Vision image buffer is smaller than expected".to_owned());
    }

    for target_y in 0..resized_height {
        let source_y = target_y * source_height / resized_height;
        let output_y = pad_y + target_y;

        for target_x in 0..resized_width {
            let source_x = target_x * source_width / resized_width;
            let output_x = pad_x + target_x;

            let source_index = (source_y * source_width + source_x) * 4;
            let target_index = output_y * MODEL_WIDTH + output_x;

            input[target_index] = rgba[source_index] as f32 / 255.0;
            input[plane_size + target_index] = rgba[source_index + 1] as f32 / 255.0;
            input[2 * plane_size + target_index] = rgba[source_index + 2] as f32 / 255.0;
        }
    }

    Ok((
        input,
        Letterbox {
            scale,
            pad_x: pad_x as f32,
            pad_y: pad_y as f32,
            source_width,
            source_height,
        },
    ))
}

fn decode_detections(
    data: &[f32],
    letterbox: &Letterbox,
    kind: DetectionKind,
    confidence_threshold: f32,
) -> Vec<Detection> {
    let mut detections = Vec::new();

    for detection in data.chunks_exact(6) {
        let confidence = detection[4];
        let class_id = detection[5].round() as i32;

        if class_id != DETECTOR_CLASS_ID || confidence < confidence_threshold {
            continue;
        }

        let x1 = ((detection[0] - letterbox.pad_x) / letterbox.scale)
            .clamp(0.0, letterbox.source_width as f32);

        let y1 = ((detection[1] - letterbox.pad_y) / letterbox.scale)
            .clamp(0.0, letterbox.source_height as f32);

        let x2 = ((detection[2] - letterbox.pad_x) / letterbox.scale)
            .clamp(0.0, letterbox.source_width as f32);

        let y2 = ((detection[3] - letterbox.pad_y) / letterbox.scale)
            .clamp(0.0, letterbox.source_height as f32);

        if x2 <= x1 || y2 <= y1 {
            continue;
        }

        detections.push(Detection {
            kind,
            x1,
            y1,
            x2,
            y2,
            confidence,
        });
    }

    detections.sort_by(|left, right| right.confidence.total_cmp(&left.confidence));

    detections
}

fn set_error(shared: &Arc<Shared>, error: String) {
    let mut snapshot = shared
        .snapshot
        .lock()
        .unwrap_or_else(PoisonError::into_inner);

    snapshot.last_error = Some(error);
}
