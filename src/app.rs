use crate::camera::PreviewInfo;
use eframe::egui;

pub fn run(camera: cameras::Camera, info: PreviewInfo) -> eframe::Result<()> {
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
            let stream = spawn_camera_stream(camera, creation_context.egui_ctx.clone());

            Ok(Box::new(BareEyeApp::new(stream, info)))
        }),
    )
}

fn spawn_camera_stream(
    camera: cameras::Camera,
    repaint_context: egui::Context,
) -> egui_cameras::Stream {
    let sink = egui_cameras::Sink::default();
    let pump_sink = sink.clone();

    let pump = cameras::pump::spawn(camera, move |frame| {
        egui_cameras::publish_frame(&pump_sink, frame);
        repaint_context.request_repaint();
    });

    egui_cameras::Stream {
        pump,
        sink,
        texture: None,
        name: "bareeye-camera".to_owned(),
    }
}

struct BareEyeApp {
    stream: Option<egui_cameras::Stream>,
    info: PreviewInfo,
    uploaded_frames: u64,
    last_error: Option<String>,
}

impl BareEyeApp {
    fn new(stream: egui_cameras::Stream, info: PreviewInfo) -> Self {
        Self {
            stream: Some(stream),
            info,
            uploaded_frames: 0,
            last_error: None,
        }
    }
}

impl eframe::App for BareEyeApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        if let Some(stream) = self.stream.as_mut() {
            match egui_cameras::update_texture(stream, &ctx) {
                Ok(true) => {
                    self.uploaded_frames += 1;
                    self.last_error = None;
                }
                Ok(false) => {}
                Err(error) => {
                    self.last_error = Some(error.to_string());
                }
            }
        }

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

                ui.label(format!("Frames: {}", self.uploaded_frames));
            });

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
