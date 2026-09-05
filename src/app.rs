use crate::camera::ptz::ManualController;
use crate::camera::{PreviewInfo, StreamTelemetry};
use eframe::egui;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

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

fn spawn_camera_stream(
    camera: cameras::Camera,
    repaint_context: egui::Context,
    info: &PreviewInfo,
) -> (egui_cameras::Stream, Arc<Mutex<StreamTelemetry>>) {
    let sink = egui_cameras::Sink::default();
    let pump_sink = sink.clone();

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

        egui_cameras::publish_frame(&pump_sink, frame);
        repaint_context.request_repaint();
    });

    (
        egui_cameras::Stream {
            pump,
            sink,
            texture: None,
            name: "bareeye-camera".to_owned(),
        },
        telemetry,
    )
}

struct BareEyeApp {
    stream: Option<egui_cameras::Stream>,
    telemetry: Arc<Mutex<StreamTelemetry>>,
    info: PreviewInfo,
    ptz: ManualController,
    ptz_error: Option<String>,
    last_ptz_poll: Instant,
    uploaded_frames: u64,
    measured_fps: f32,
    fps_window_frames: u64,
    fps_window_started: Instant,
    last_error: Option<String>,
}

impl BareEyeApp {
    fn new(
        stream: egui_cameras::Stream,
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
            last_ptz_poll: Instant::now(),
            uploaded_frames: 0,
            measured_fps: 0.0,
            fps_window_frames: 0,
            fps_window_started: Instant::now(),
            last_error: None,
        }
    }

    fn record_ptz_result(&mut self, result: Result<(), cameras::Error>) {
        match result {
            Ok(()) => self.ptz_error = None,
            Err(error) => self.ptz_error = Some(error.to_string()),
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

        if let Some(stream) = self.stream.as_mut() {
            match egui_cameras::update_texture(stream, &ctx) {
                Ok(true) => {
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
                    self.last_error = Some(error.to_string());
                }
            }
        }

        if self.last_ptz_poll.elapsed() >= Duration::from_millis(250) {
            let result = self.ptz.refresh_actual();
            self.record_ptz_result(result);
            self.last_ptz_poll = Instant::now();
        }

        let telemetry = self
            .telemetry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .snapshot();

        let not_displayed = telemetry
            .arrived_frames
            .saturating_sub(self.uploaded_frames);

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
            });

            ui.horizontal(|ui| {
                ui.label(format!(
                    "Target: Pan {:.0}  Tilt {:.0}",
                    self.ptz.pan_target(),
                    self.ptz.tilt_target()
                ));

                ui.separator();

                ui.label(format!(
                    "Actual: Pan {}  Tilt {}",
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
                    "Actual zoom: {}",
                    format_position(self.ptz.actual_zoom())
                ));
            });

            if let Some(error) = &self.ptz_error {
                ui.colored_label(egui::Color32::RED, format!("PTZ error: {error}"));
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
                egui_cameras::show(stream, ui);
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
