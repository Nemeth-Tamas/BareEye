use crate::camera::ptz::ManualController;
use crate::camera::{PreviewInfo, StreamTelemetry};
use crate::vision::{Detection, DetectionKind, VisionInput, VisionWorker};
use eframe::egui;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

const RAW_FRAME_QUEUE_CAPACITY: usize = 4;
const PREVIEW_QUEUE_CAPACITY: usize = 2;
const SLOW_DECODE: Duration = Duration::from_millis(33);
const SLOW_TEXTURE_UPDATE: Duration = Duration::from_millis(33);
const SLOW_DISPLAY_GAP: Duration = Duration::from_millis(45);
const SLOW_UI_GAP: Duration = Duration::from_millis(50);
const PTZ_BUTTON_COOLDOWN: Duration = Duration::from_millis(200);

pub fn run(
    camera: cameras::Camera,
    info: PreviewInfo,
    ptz: ManualController,
    debug: bool,
) -> eframe::Result<()> {
    let vision = VisionWorker::spawn("models/yolo26n.onnx", "models/yolov8n-face.onnx");
    let vision_input = vision.input();

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([640.0, 400.0]),
        ..Default::default()
    };

    eframe::run_native(
        "BareEye",
        native_options,
        Box::new(move |creation_context| {
            let (stream, telemetry) = spawn_camera_stream(
                camera,
                creation_context.egui_ctx.clone(),
                &info,
                vision_input,
            );

            Ok(Box::new(BareEyeApp::new(
                stream, telemetry, info, ptz, vision, debug,
            )))
        }),
    )
}

#[derive(Clone, Default)]
struct PreviewWorkerStats {
    decode_ms: f32,
    decode_peak_ms: f32,
    slow_decodes: u64,
    decode_repairs: u64,
    last_decode_repair: Option<String>,
    decode_errors: u64,
    last_decode_error: Option<String>,
    raw_queue_drops: u64,
    queue_drops: u64,
}

struct PreviewStream {
    pump: egui_cameras::Pump,
    decoder: std::thread::JoinHandle<()>,
    queue: Arc<Mutex<VecDeque<Arc<egui::ColorImage>>>>,
    worker_stats: Arc<Mutex<PreviewWorkerStats>>,
    texture: Option<egui::TextureHandle>,
    name: String,
}

impl PreviewStream {
    fn stop(self) {
        let PreviewStream { pump, decoder, .. } = self;

        egui_cameras::stop_and_join(pump);
        let _ = decoder.join();
    }

    fn update_texture(&mut self, ctx: &egui::Context) -> Result<bool, String> {
        let next_image = self
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front();

        let Some(image) = next_image else {
            return Ok(false);
        };

        match &mut self.texture {
            Some(texture) => {
                texture.set(image, egui::TextureOptions::LINEAR);
            }
            None => {
                self.texture =
                    Some(ctx.load_texture(&self.name, image, egui::TextureOptions::LINEAR));
            }
        }

        let has_more = !self
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty();

        if has_more {
            ctx.request_repaint();
        }

        Ok(true)
    }

    fn show(&self, ui: &mut egui::Ui, detections: &[Detection]) {
        let Some(texture) = &self.texture else {
            return;
        };

        let aspect = texture.aspect_ratio();
        let available = ui.available_size();
        let width = available.x.min(available.y * aspect);
        let height = width / aspect;

        let response = ui.image((texture.id(), egui::vec2(width, height)));
        let image_rect = response.rect;

        let texture_size = texture.size();
        let scale_x = image_rect.width() / texture_size[0] as f32;
        let scale_y = image_rect.height() / texture_size[1] as f32;

        let painter = ui.painter();

        for detection in detections {
            let color = match detection.kind {
                DetectionKind::Person => egui::Color32::from_rgb(0, 255, 0),
                DetectionKind::Face => egui::Color32::from_rgb(0, 200, 255),
            };

            let stroke = egui::Stroke::new(2.0_f32, color);
            let left = image_rect.left() + detection.x1 * scale_x;
            let top = image_rect.top() + detection.y1 * scale_y;
            let right = image_rect.left() + detection.x2 * scale_x;
            let bottom = image_rect.top() + detection.y2 * scale_y;

            let top_left = egui::pos2(left, top);
            let top_right = egui::pos2(right, top);
            let bottom_left = egui::pos2(left, bottom);
            let bottom_right = egui::pos2(right, bottom);

            painter.line_segment([top_left, top_right], stroke);
            painter.line_segment([top_right, bottom_right], stroke);
            painter.line_segment([bottom_right, bottom_left], stroke);
            painter.line_segment([bottom_left, top_left], stroke);

            painter.text(
                top_left + egui::vec2(4.0, 4.0),
                egui::Align2::LEFT_TOP,
                format!(
                    "{} {:.0}%",
                    detection.kind.label(),
                    detection.confidence * 100.0
                ),
                egui::FontId::proportional(16.0),
                color,
            );
        }
    }

    fn queue_depth(&self) -> usize {
        self.queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    fn worker_stats(&self) -> PreviewWorkerStats {
        self.worker_stats
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

fn decode_preview_frame(
    frame: &cameras::Frame,
) -> (Result<egui::ColorImage, String>, Option<(usize, usize)>) {
    match egui_cameras::frame_to_color_image(frame) {
        Ok(image) => (Ok(image), None),
        Err(first_error) => {
            if frame.pixel_format != cameras::PixelFormat::Mjpeg {
                return (Err(first_error.to_string()), None);
            }

            let data = frame.plane_primary.as_ref();

            let Some(start) = data
                .windows(2)
                .position(|bytes| bytes[0] == 0xFF && bytes[1] == 0xD8)
            else {
                return (Err(first_error.to_string()), None);
            };

            let end = data[start + 2..]
                .windows(2)
                .position(|bytes| bytes[0] == 0xFF && bytes[1] == 0xD9)
                .map(|offset| start + 2 + offset + 2)
                .unwrap_or(data.len());

            if start == 0 && end == data.len() {
                return (Err(first_error.to_string()), None);
            }

            if start >= end {
                return (Err(first_error.to_string()), None);
            }

            let mut repaired = frame.clone();
            repaired.plane_primary = frame.plane_primary.slice(start..end);

            match egui_cameras::frame_to_color_image(&repaired) {
                Ok(image) => {
                    let trailing = data.len().saturating_sub(end);

                    (Ok(image), Some((start, trailing)))
                }
                Err(repair_error) => (
                    Err(format!(
                        "{first_error}; MJPEG repair attempt also failed: {repair_error}"
                    )),
                    None,
                ),
            }
        }
    }
}

fn spawn_camera_stream(
    camera: cameras::Camera,
    repaint_context: egui::Context,
    info: &PreviewInfo,
    vision_input: VisionInput,
) -> (PreviewStream, Arc<Mutex<StreamTelemetry>>) {
    let queue = Arc::new(Mutex::new(VecDeque::with_capacity(PREVIEW_QUEUE_CAPACITY)));
    let decoder_queue = Arc::clone(&queue);

    let worker_stats = Arc::new(Mutex::new(PreviewWorkerStats::default()));
    let decoder_worker_stats = Arc::clone(&worker_stats);
    let pump_worker_stats = Arc::clone(&worker_stats);

    let telemetry = Arc::new(Mutex::new(StreamTelemetry::new(
        info.width,
        info.height,
        cameras::PixelFormat::Mjpeg,
    )));
    let pump_telemetry = Arc::clone(&telemetry);

    let (raw_sender, raw_receiver) = mpsc::sync_channel::<cameras::Frame>(RAW_FRAME_QUEUE_CAPACITY);

    let decoder = std::thread::Builder::new()
        .name("bareeye-decoder".to_owned())
        .spawn(move || {
            while let Ok(frame) = raw_receiver.recv() {
                let decode_started = Instant::now();
                let (decoded, repair) = decode_preview_frame(&frame);
                let decode_elapsed = decode_started.elapsed();

                {
                    let mut stats = decoder_worker_stats
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner);

                    stats.decode_ms = decode_elapsed.as_secs_f32() * 1000.0;
                    stats.decode_peak_ms = stats.decode_peak_ms.max(stats.decode_ms);

                    if decode_elapsed >= SLOW_DECODE {
                        stats.slow_decodes += 1;
                    }

                    if let Some((leading, trailing)) = repair {
                        stats.decode_repairs += 1;
                        stats.last_decode_repair = Some(format!(
                            "trimmed {leading} leading byte(s) and {trailing} trailing byte(s)"
                        ));
                    }

                    if let Err(error) = &decoded {
                        stats.decode_errors += 1;
                        stats.last_decode_error = Some(error.clone());
                    }
                }

                let decoded = match decoded {
                    Ok(image) => Arc::new(image),
                    Err(_) => {
                        repaint_context.request_repaint();
                        continue;
                    }
                };

                vision_input.submit(Arc::clone(&decoded));

                let dropped = {
                    let mut queue = decoder_queue.lock().unwrap_or_else(PoisonError::into_inner);

                    let dropped = queue.len() >= PREVIEW_QUEUE_CAPACITY;

                    if dropped {
                        queue.pop_front();
                    }

                    queue.push_back(decoded);

                    dropped
                };

                if dropped {
                    decoder_worker_stats
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .queue_drops += 1;
                }

                repaint_context.request_repaint();
            }
        })
        .expect("failed to start BareEye decoder worker");

    let pump = cameras::pump::spawn(camera, move |frame| {
        pump_telemetry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .observe(&frame);

        match raw_sender.try_send(frame) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(_)) => {
                pump_worker_stats
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .raw_queue_drops += 1;
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {}
        }
    });

    (
        PreviewStream {
            pump,
            decoder,
            queue,
            worker_stats,
            texture: None,
            name: "bareeye-camera".to_owned(),
        },
        telemetry,
    )
}

struct BareEyeApp {
    stream: Option<PreviewStream>,
    telemetry: Arc<Mutex<StreamTelemetry>>,
    info: PreviewInfo,
    ptz: ManualController,
    vision: VisionWorker,
    debug: bool,
    ptz_error: Option<String>,
    last_ptz_button_at: Option<Instant>,
    last_ui_started: Instant,
    ui_gap_ms: f32,
    ui_gap_peak_ms: f32,
    slow_ui_gaps: u64,
    texture_update_ms: f32,
    texture_update_peak_ms: f32,
    slow_texture_updates: u64,
    last_displayed_at: Option<Instant>,
    display_gap_ms: f32,
    display_gap_peak_ms: f32,
    slow_display_gaps: u64,
    uploaded_frames: u64,
    measured_fps: f32,
    fps_window_frames: u64,
    fps_window_started: Instant,
    last_error: Option<String>,
}

impl BareEyeApp {
    fn new(
        stream: PreviewStream,
        telemetry: Arc<Mutex<StreamTelemetry>>,
        info: PreviewInfo,
        ptz: ManualController,
        vision: VisionWorker,
        debug: bool,
    ) -> Self {
        Self {
            stream: Some(stream),
            telemetry,
            info,
            ptz,
            vision,
            debug,
            ptz_error: None,
            last_ptz_button_at: None,
            last_ui_started: Instant::now(),
            ui_gap_ms: 0.0,
            ui_gap_peak_ms: 0.0,
            slow_ui_gaps: 0,
            texture_update_ms: 0.0,
            texture_update_peak_ms: 0.0,
            slow_texture_updates: 0,
            last_displayed_at: None,
            display_gap_ms: 0.0,
            display_gap_peak_ms: 0.0,
            slow_display_gaps: 0,
            uploaded_frames: 0,
            measured_fps: 0.0,
            fps_window_frames: 0,
            fps_window_started: Instant::now(),
            last_error: None,
        }
    }

    fn record_ptz_result(&mut self, result: Result<(), String>) {
        match result {
            Ok(()) => self.ptz_error = None,
            Err(error) => self.ptz_error = Some(error),
        }
    }

    fn ptz_buttons_enabled(&self) -> bool {
        match self.last_ptz_button_at {
            Some(last) => last.elapsed() >= PTZ_BUTTON_COOLDOWN,
            None => true,
        }
    }

    fn mark_ptz_button_used(&mut self) {
        self.last_ptz_button_at = Some(Instant::now());
    }

    fn handle_keyboard_ptz(&mut self, ctx: &egui::Context) {
        let (left, right, up, down, zoom_out, zoom_in, center) = ctx.input(|input| {
            (
                input.key_pressed(egui::Key::ArrowLeft) || input.key_pressed(egui::Key::A),
                input.key_pressed(egui::Key::ArrowRight) || input.key_pressed(egui::Key::D),
                input.key_pressed(egui::Key::ArrowUp) || input.key_pressed(egui::Key::W),
                input.key_pressed(egui::Key::ArrowDown) || input.key_pressed(egui::Key::S),
                input.key_pressed(egui::Key::Q),
                input.key_pressed(egui::Key::E),
                input.key_pressed(egui::Key::C),
            )
        });

        if left {
            let result = self.ptz.pan_by(-10.0);
            self.record_ptz_result(result);
        }

        if right {
            let result = self.ptz.pan_by(10.0);
            self.record_ptz_result(result);
        }

        if up {
            let result = self.ptz.tilt_by(5.0);
            self.record_ptz_result(result);
        }

        if down {
            let result = self.ptz.tilt_by(-5.0);
            self.record_ptz_result(result);
        }

        if zoom_out {
            let result = self.ptz.zoom_by(-400.0);
            self.record_ptz_result(result);
        }

        if zoom_in {
            let result = self.ptz.zoom_by(400.0);
            self.record_ptz_result(result);
        }

        if center {
            let result = self.ptz.center();
            self.record_ptz_result(result);
        }
    }
}

fn format_position(value: Option<f32>) -> String {
    match value {
        Some(value) => format!("{value:.0}"),
        None => "n/a".to_owned(),
    }
}

impl eframe::App for BareEyeApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        self.handle_keyboard_ptz(&ctx);

        let ui_started = Instant::now();
        let ui_gap = ui_started.duration_since(self.last_ui_started);
        self.last_ui_started = ui_started;

        self.ui_gap_ms = ui_gap.as_secs_f32() * 1000.0;
        self.ui_gap_peak_ms = self.ui_gap_peak_ms.max(self.ui_gap_ms);

        if ui_gap >= SLOW_UI_GAP {
            self.slow_ui_gaps += 1;
        }

        if let Some(stream) = self.stream.as_mut() {
            let texture_started = Instant::now();
            let update_result = stream.update_texture(&ctx);
            let texture_elapsed = texture_started.elapsed();

            match update_result {
                Ok(true) => {
                    self.texture_update_ms = texture_elapsed.as_secs_f32() * 1000.0;
                    self.texture_update_peak_ms =
                        self.texture_update_peak_ms.max(self.texture_update_ms);

                    if texture_elapsed >= SLOW_TEXTURE_UPDATE {
                        self.slow_texture_updates += 1;
                    }

                    let displayed_now = Instant::now();

                    if let Some(previous) = self.last_displayed_at {
                        let display_gap = displayed_now.duration_since(previous);

                        self.display_gap_ms = display_gap.as_secs_f32() * 1000.0;
                        self.display_gap_peak_ms =
                            self.display_gap_peak_ms.max(self.display_gap_ms);

                        if display_gap >= SLOW_DISPLAY_GAP {
                            self.slow_display_gaps += 1;
                        }
                    }

                    self.last_displayed_at = Some(displayed_now);

                    self.uploaded_frames += 1;
                    self.fps_window_frames += 1;
                    self.last_error = None;

                    let elapsed = self.fps_window_started.elapsed();

                    if elapsed.as_secs_f32() >= 1.0 {
                        self.measured_fps = self.fps_window_frames as f32 / elapsed.as_secs_f32();

                        self.fps_window_frames = 0;
                        self.fps_window_started = Instant::now();
                    }
                }
                Ok(false) => {}
                Err(error) => {
                    self.last_error = Some(error);
                }
            }
        }

        let telemetry = self
            .telemetry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .snapshot();

        let vision = self.vision.snapshot();

        let person_count = vision
            .detections
            .iter()
            .filter(|detection| detection.kind == DetectionKind::Person)
            .count();

        let face_count = vision
            .detections
            .iter()
            .filter(|detection| detection.kind == DetectionKind::Face)
            .count();

        let buffered_frames = self
            .stream
            .as_ref()
            .map_or(0, |stream| stream.queue_depth());

        let worker_stats = self
            .stream
            .as_ref()
            .map_or_else(PreviewWorkerStats::default, |stream| stream.worker_stats());

        let pipeline_drops = worker_stats.raw_queue_drops + worker_stats.queue_drops;

        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.strong("BareEye");

                ui.separator();

                ui.label(format!(
                    "{}x{} @ {} FPS",
                    self.info.width, self.info.height, self.info.framerate
                ));

                ui.separator();

                ui.label(&self.info.pixel_format);

                ui.separator();

                ui.label(format!("Camera: {:.1} FPS", telemetry.arrival_fps));

                ui.separator();

                ui.label(format!("Display: {:.1} FPS", self.measured_fps));

                ui.separator();

                if vision.ready {
                    ui.label(format!(
                        "Vision: {person_count} person(s) | {face_count} face(s) | {:.1} ms prep + {:.1}/{:.1} ms CUDA",
                        vision.preprocess_ms,
                        vision.inference_ms,
                        vision.face_inference_ms
                    ));
                } else {
                    ui.label("Vision: starting...");
                }

                if self.debug {
                    ui.separator();
                    ui.strong("DEBUG");
                }
            });

            if let Some(error) = &vision.last_error {
                ui.colored_label(egui::Color32::RED, format!("Vision error: {error}"));
            }

            if self.debug {
                ui.horizontal(|ui| {
                    ui.label(format!("Arrived: {}", telemetry.arrived_frames));

                    ui.separator();

                    ui.label(format!("Displayed: {}", self.uploaded_frames));

                    ui.separator();

                    ui.label(format!("Pipeline drops: {pipeline_drops}"));

                    ui.separator();

                    ui.label(format!("Source gaps: {}", telemetry.timing_anomalies));

                    ui.separator();

                    ui.label(format!("Size anomalies: {}", telemetry.dimension_anomalies));

                    ui.separator();

                    ui.label(format!("Format anomalies: {}", telemetry.format_anomalies));

                    ui.separator();

                    ui.label(format!("Vision processed: {}", vision.processed_frames));

                    ui.separator();

                    ui.label(format!(
                        "Vision stale frames replaced: {}",
                        vision.replaced_frames
                    ));
                });

                ui.horizontal(|ui| {
                    ui.label(format!(
                        "MJPEG: {:.0} KiB avg / {:.0} KiB peak",
                        telemetry.mjpeg_average_kib, telemetry.mjpeg_peak_kib
                    ));

                    ui.separator();

                    ui.label(format!(
                        "Decode: {:.1} ms / {:.1} ms peak",
                        worker_stats.decode_ms, worker_stats.decode_peak_ms
                    ));

                    ui.separator();

                    ui.label(format!("Slow decodes: {}", worker_stats.slow_decodes));

                    ui.separator();

                    ui.label(format!(
                        "Upload: {:.1} ms / {:.1} ms peak",
                        self.texture_update_ms, self.texture_update_peak_ms
                    ));

                    ui.separator();

                    ui.label(format!("Slow uploads: {}", self.slow_texture_updates));
                });

                ui.horizontal(|ui| {
                    ui.label(format!(
                        "Buffered: {}/{}",
                        buffered_frames, PREVIEW_QUEUE_CAPACITY
                    ));

                    ui.separator();

                    ui.label(format!("Raw queue drops: {}", worker_stats.raw_queue_drops));

                    ui.separator();

                    ui.label(format!("Decoded queue drops: {}", worker_stats.queue_drops));

                    ui.separator();

                    ui.label(format!("JPEG repairs: {}", worker_stats.decode_repairs));

                    ui.separator();

                    ui.label(format!("Decode errors: {}", worker_stats.decode_errors));
                });

                ui.horizontal(|ui| {
                    ui.label(format!(
                        "Display gap: {:.1} ms / {:.1} ms peak",
                        self.display_gap_ms, self.display_gap_peak_ms
                    ));

                    ui.separator();

                    ui.label(format!("Slow display gaps: {}", self.slow_display_gaps));

                    ui.separator();

                    ui.label(format!(
                        "UI gap: {:.1} ms / {:.1} ms peak",
                        self.ui_gap_ms, self.ui_gap_peak_ms
                    ));

                    ui.separator();

                    ui.label(format!("Slow UI gaps: {}", self.slow_ui_gaps));
                });

                if let Some(repair) = &worker_stats.last_decode_repair {
                    ui.label(format!("Last JPEG repair: {repair}"));
                }

                if let Some(error) = &worker_stats.last_decode_error {
                    ui.colored_label(
                        egui::Color32::RED,
                        format!("Last unrecoverable JPEG error: {error}"),
                    );
                }
            }

            ui.separator();

            ui.horizontal(|ui| {
                ui.strong("PTZ");

                let buttons_enabled = self.ptz_buttons_enabled();

                if ui
                    .add_enabled(buttons_enabled, egui::Button::new("Left"))
                    .clicked()
                {
                    self.mark_ptz_button_used();
                    let result = self.ptz.pan_by(-10.0);
                    self.record_ptz_result(result);
                }

                if ui
                    .add_enabled(buttons_enabled, egui::Button::new("Right"))
                    .clicked()
                {
                    self.mark_ptz_button_used();
                    let result = self.ptz.pan_by(10.0);
                    self.record_ptz_result(result);
                }

                if ui
                    .add_enabled(buttons_enabled, egui::Button::new("Up"))
                    .clicked()
                {
                    self.mark_ptz_button_used();
                    let result = self.ptz.tilt_by(5.0);
                    self.record_ptz_result(result);
                }

                if ui
                    .add_enabled(buttons_enabled, egui::Button::new("Down"))
                    .clicked()
                {
                    self.mark_ptz_button_used();
                    let result = self.ptz.tilt_by(-5.0);
                    self.record_ptz_result(result);
                }

                if ui
                    .add_enabled(buttons_enabled, egui::Button::new("Center"))
                    .clicked()
                {
                    self.mark_ptz_button_used();
                    let result = self.ptz.center();
                    self.record_ptz_result(result);
                }

                if self.debug && ui.button("Read position").clicked() {
                    let result = self.ptz.refresh_actual();
                    self.record_ptz_result(result);
                }
            });

            ui.horizontal(|ui| {
                ui.label("Keys: WASD/Arrows = PTZ  |  Q/E = Zoom  |  C = Center");

                ui.separator();

                ui.label(format!(
                    "Target: Pan {:.0}  Tilt {:.0}",
                    self.ptz.pan_target(),
                    self.ptz.tilt_target()
                ));

                if self.debug {
                    ui.separator();

                    ui.label(format!(
                        "Last read: Pan {}  Tilt {}",
                        format_position(self.ptz.actual_pan()),
                        format_position(self.ptz.actual_tilt())
                    ));
                }
            });

            ui.horizontal(|ui| {
                if ui.button("Zoom -").clicked() {
                    let result = self.ptz.zoom_by(-400.0);
                    self.record_ptz_result(result);
                }

                if ui.button("Zoom +").clicked() {
                    let result = self.ptz.zoom_by(400.0);
                    self.record_ptz_result(result);
                }

                if ui.button("Wide").clicked() {
                    let result = self.ptz.wide();
                    self.record_ptz_result(result);
                }

                if let Some(range) = self.ptz.zoom_range() {
                    let mut zoom = self.ptz.zoom_target();

                    if ui
                        .add(
                            egui::Slider::new(&mut zoom, range.min..=range.max).text("Zoom target"),
                        )
                        .changed()
                    {
                        let result = self.ptz.set_zoom(zoom);
                        self.record_ptz_result(result);
                    }
                }

                if self.debug {
                    ui.separator();

                    ui.label(format!(
                        "Last read zoom: {}",
                        format_position(self.ptz.actual_zoom())
                    ));
                }
            });

            if self.debug {
                ui.label(format!(
                    "PTZ worker last operation: {:.1} ms",
                    self.ptz.last_operation_ms()
                ));
            }

            if let Some(error) = self.ptz.worker_error() {
                ui.colored_label(egui::Color32::RED, format!("PTZ worker error: {error}"));
            }

            if let Some(error) = &self.ptz_error {
                ui.colored_label(egui::Color32::RED, format!("PTZ queue error: {error}"));
            }

            if let Some(error) = &self.last_error {
                ui.colored_label(egui::Color32::RED, format!("Camera frame error: {error}"));
            }

            ui.separator();

            let Some(stream) = self.stream.as_ref() else {
                ui.centered_and_justified(|ui| {
                    ui.label("Camera stream has stopped.");
                });
                return;
            };

            if stream.texture.is_some() {
                stream.show(ui, &vision.detections);
            } else {
                ui.centered_and_justified(|ui| {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Waiting for EagleEye video...");
                    });
                });
            }
        });
    }
}

impl Drop for BareEyeApp {
    fn drop(&mut self) {
        if let Some(stream) = self.stream.take() {
            stream.stop();
        }
    }
}
