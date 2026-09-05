use crate::camera::ptz::ManualController;
use crate::camera::{PreviewInfo, StreamTelemetry};
use eframe::egui;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

const PREVIEW_QUEUE_CAPACITY: usize = 2;
const SLOW_DECODE: Duration = Duration::from_millis(33);
const SLOW_TEXTURE_UPDATE: Duration = Duration::from_millis(33);
const SLOW_UI_GAP: Duration = Duration::from_millis(50);

pub fn run(
    camera: cameras::Camera,
    info: PreviewInfo,
    ptz: ManualController,
) -> eframe::Result<()> {
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
            let (stream, telemetry) =
                spawn_camera_stream(camera, creation_context.egui_ctx.clone(), &info);

            Ok(Box::new(BareEyeApp::new(stream, telemetry, info, ptz)))
        }),
    )
}

#[derive(Copy, Clone, Default)]
struct PreviewWorkerStats {
    decode_ms: f32,
    decode_peak_ms: f32,
    slow_decodes: u64,
    queue_drops: u64,
}

struct PreviewStream {
    pump: egui_cameras::Pump,
    queue: Arc<Mutex<VecDeque<Result<egui::ColorImage, String>>>>,
    worker_stats: Arc<Mutex<PreviewWorkerStats>>,
    texture: Option<egui::TextureHandle>,
    name: String,
}

impl PreviewStream {
    fn update_texture(&mut self, ctx: &egui::Context) -> Result<bool, String> {
        let next_image = self
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front();

        let Some(image) = next_image else {
            return Ok(false);
        };

        let image = image?;

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

    fn show(&self, ui: &mut egui::Ui) {
        let Some(texture) = &self.texture else {
            return;
        };

        let aspect = texture.aspect_ratio();
        let available = ui.available_size();
        let width = available.x.min(available.y * aspect);
        let height = width / aspect;

        ui.image((texture.id(), egui::vec2(width, height)));
    }

    fn queue_depth(&self) -> usize {
        self.queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    fn worker_stats(&self) -> PreviewWorkerStats {
        *self
            .worker_stats
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

fn spawn_camera_stream(
    camera: cameras::Camera,
    repaint_context: egui::Context,
    info: &PreviewInfo,
) -> (PreviewStream, Arc<Mutex<StreamTelemetry>>) {
    let queue = Arc::new(Mutex::new(VecDeque::with_capacity(PREVIEW_QUEUE_CAPACITY)));
    let pump_queue = Arc::clone(&queue);

    let worker_stats = Arc::new(Mutex::new(PreviewWorkerStats::default()));
    let pump_worker_stats = Arc::clone(&worker_stats);

    let telemetry = Arc::new(Mutex::new(StreamTelemetry::new(
        info.width,
        info.height,
        cameras::PixelFormat::Mjpeg,
    )));
    let pump_telemetry = Arc::clone(&telemetry);

    let pump = cameras::pump::spawn(camera, move |frame| {
        pump_telemetry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .observe(&frame);

        let decode_started = Instant::now();
        let decoded = egui_cameras::frame_to_color_image(&frame).map_err(|error| error.to_string());
        let decode_elapsed = decode_started.elapsed();

        {
            let mut stats = pump_worker_stats
                .lock()
                .unwrap_or_else(PoisonError::into_inner);

            stats.decode_ms = decode_elapsed.as_secs_f32() * 1000.0;
            stats.decode_peak_ms = stats.decode_peak_ms.max(stats.decode_ms);

            if decode_elapsed >= SLOW_DECODE {
                stats.slow_decodes += 1;
            }
        }

        let dropped = {
            let mut queue = pump_queue.lock().unwrap_or_else(PoisonError::into_inner);

            let dropped = queue.len() >= PREVIEW_QUEUE_CAPACITY;

            if dropped {
                queue.pop_front();
            }

            queue.push_back(decoded);

            dropped
        };

        if dropped {
            pump_worker_stats
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .queue_drops += 1;
        }

        repaint_context.request_repaint();
    });

    (
        PreviewStream {
            pump,
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
    ptz_error: Option<String>,
    last_ui_started: Instant,
    ui_gap_ms: f32,
    ui_gap_peak_ms: f32,
    slow_ui_gaps: u64,
    texture_update_ms: f32,
    texture_update_peak_ms: f32,
    slow_texture_updates: u64,
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
    ) -> Self {
        Self {
            stream: Some(stream),
            telemetry,
            info,
            ptz,
            ptz_error: None,
            last_ui_started: Instant::now(),
            ui_gap_ms: 0.0,
            ui_gap_peak_ms: 0.0,
            slow_ui_gaps: 0,
            texture_update_ms: 0.0,
            texture_update_peak_ms: 0.0,
            slow_texture_updates: 0,
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

        let buffered_frames = self
            .stream
            .as_ref()
            .map_or(0, |stream| stream.queue_depth());

        let worker_stats = self
            .stream
            .as_ref()
            .map_or_else(PreviewWorkerStats::default, |stream| stream.worker_stats());

        let not_displayed = telemetry
            .arrived_frames
            .saturating_sub(self.uploaded_frames + buffered_frames as u64);

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
            });

            ui.horizontal(|ui| {
                ui.label(format!("Arrived: {}", telemetry.arrived_frames));

                ui.separator();

                ui.label(format!("Displayed: {}", self.uploaded_frames));

                ui.separator();

                ui.label(format!("Not displayed: {not_displayed}"));

                ui.separator();

                ui.label(format!("Size anomalies: {}", telemetry.dimension_anomalies));

                ui.separator();

                ui.label(format!("Format anomalies: {}", telemetry.format_anomalies));

                ui.separator();

                ui.label(format!("Timing gaps: {}", telemetry.timing_anomalies));
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

                ui.label(format!("Queue drops: {}", worker_stats.queue_drops));

                ui.separator();

                ui.label(format!(
                    "UI gap: {:.1} ms / {:.1} ms peak",
                    self.ui_gap_ms, self.ui_gap_peak_ms
                ));

                ui.separator();

                ui.label(format!("Slow UI gaps: {}", self.slow_ui_gaps));
            });

            ui.separator();

            ui.horizontal(|ui| {
                ui.strong("PTZ");

                if ui.button("Left").clicked() {
                    let result = self.ptz.pan_by(-10.0);
                    self.record_ptz_result(result);
                }

                if ui.button("Right").clicked() {
                    let result = self.ptz.pan_by(10.0);
                    self.record_ptz_result(result);
                }

                if ui.button("Up").clicked() {
                    let result = self.ptz.tilt_by(5.0);
                    self.record_ptz_result(result);
                }

                if ui.button("Down").clicked() {
                    let result = self.ptz.tilt_by(-5.0);
                    self.record_ptz_result(result);
                }

                if ui.button("Center").clicked() {
                    let result = self.ptz.center();
                    self.record_ptz_result(result);
                }

                if ui.button("Read position").clicked() {
                    let result = self.ptz.refresh_actual();
                    self.record_ptz_result(result);
                }
            });

            ui.horizontal(|ui| {
                ui.label(format!(
                    "Target: Pan {:.0}  Tilt {:.0}",
                    self.ptz.pan_target(),
                    self.ptz.tilt_target()
                ));

                ui.separator();

                ui.label(format!(
                    "Last read: Pan {}  Tilt {}",
                    format_position(self.ptz.actual_pan()),
                    format_position(self.ptz.actual_tilt())
                ));
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

                ui.separator();

                ui.label(format!(
                    "Last read zoom: {}",
                    format_position(self.ptz.actual_zoom())
                ));
            });

            ui.horizontal(|ui| {
                ui.label(format!(
                    "PTZ worker last operation: {:.1} ms",
                    self.ptz.last_operation_ms()
                ));

                if let Some(error) = self.ptz.worker_error() {
                    ui.separator();
                    ui.colored_label(egui::Color32::RED, format!("PTZ worker error: {error}"));
                }
            });

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
                stream.show(ui);
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
            egui_cameras::stop_and_join(stream.pump);
        }
    }
}
