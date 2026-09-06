use cameras::{ControlCapabilities, ControlRange, Controls, Device};
use std::error::Error;
use std::io::{self, Write};
use std::thread;
use std::time::{Duration, Instant};

const MOVE_POLL_INTERVAL: Duration = Duration::from_millis(50);
const MOVE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Copy, Clone)]
enum Axis {
    Pan,
    Tilt,
    Zoom,
}

impl Axis {
    fn label(self) -> &'static str {
        match self {
            Self::Pan => "Pan",
            Self::Tilt => "Tilt",
            Self::Zoom => "Zoom",
        }
    }
}

enum WorkerCommand {
    Apply(Controls),
    Relative(Axis, f32),
    ReadPosition,
    Shutdown,
}

struct WorkerState {
    actual_pan: Option<f32>,
    actual_tilt: Option<f32>,
    actual_zoom: Option<f32>,
    target_pan: f32,
    target_tilt: f32,
    target_zoom: f32,
    last_error: Option<String>,
    last_operation_ms: f32,
}

fn wait_for_worker_targets(
    device: &Device,
    capabilities: &ControlCapabilities,
    state: &std::sync::Arc<std::sync::Mutex<WorkerState>>,
    pan_target: Option<f32>,
    tilt_target: Option<f32>,
    zoom_target: Option<f32>,
) -> Result<(), String> {
    let started = Instant::now();

    loop {
        let controls = cameras::read_controls(device).map_err(|error| error.to_string())?;

        {
            let mut state = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);

            state.actual_pan = controls.pan;
            state.actual_tilt = controls.tilt;
            state.actual_zoom = controls.zoom;
        }

        let pan_done = match (pan_target, controls.pan, capabilities.pan) {
            (Some(target), Some(actual), Some(range)) => {
                (actual - target).abs() <= range.step.max(1.0)
            }
            (Some(_), _, _) => false,
            (None, _, _) => true,
        };

        let tilt_done = match (tilt_target, controls.tilt, capabilities.tilt) {
            (Some(target), Some(actual), Some(range)) => {
                (actual - target).abs() <= range.step.max(1.0)
            }
            (Some(_), _, _) => false,
            (None, _, _) => true,
        };

        let zoom_done = match (zoom_target, controls.zoom, capabilities.zoom) {
            (Some(target), Some(actual), Some(range)) => {
                (actual - target).abs() <= range.step.max(1.0)
            }
            (Some(_), _, _) => false,
            (None, _, _) => true,
        };

        if pan_done && tilt_done && zoom_done {
            return Ok(());
        }

        if started.elapsed() >= MOVE_TIMEOUT {
            return Err(format!(
                "PTZ movement timed out: pan={:?}, tilt={:?}, zoom={:?}",
                controls.pan, controls.tilt, controls.zoom
            ));
        }

        thread::sleep(MOVE_POLL_INTERVAL);
    }
}

pub struct ManualController {
    capabilities: ControlCapabilities,
    sender: std::sync::mpsc::Sender<WorkerCommand>,
    worker: Option<thread::JoinHandle<()>>,
    state: std::sync::Arc<std::sync::Mutex<WorkerState>>,
}

impl ManualController {
    pub fn new(device: Device) -> Result<Self, cameras::Error> {
        let capabilities = cameras::control_capabilities(&device)?;
        let controls = cameras::read_controls(&device)?;

        let pan_target = controls
            .pan
            .or_else(|| capabilities.pan.map(|range| range.default))
            .unwrap_or(0.0);

        let tilt_target = controls
            .tilt
            .or_else(|| capabilities.tilt.map(|range| range.default))
            .unwrap_or(0.0);

        let zoom_target = controls
            .zoom
            .or_else(|| capabilities.zoom.map(|range| range.default))
            .unwrap_or(0.0);

        let state = std::sync::Arc::new(std::sync::Mutex::new(WorkerState {
            actual_pan: controls.pan,
            actual_tilt: controls.tilt,
            actual_zoom: controls.zoom,
            target_pan: pan_target,
            target_tilt: tilt_target,
            target_zoom: zoom_target,
            last_error: None,
            last_operation_ms: 0.0,
        }));

        let worker_state = std::sync::Arc::clone(&state);
        let worker_capabilities = capabilities.clone();
        let (sender, receiver) = std::sync::mpsc::channel();

        let worker = thread::Builder::new()
            .name("bareeye-ptz".to_owned())
            .spawn(move || {
                while let Ok(command) = receiver.recv() {
                    match command {
                        WorkerCommand::Apply(controls) => {
                            let pan_target = controls.pan;
                            let tilt_target = controls.tilt;
                            let zoom_target = controls.zoom;

                            let started = Instant::now();
                            let apply_result = cameras::apply_controls(&device, &controls);

                            if apply_result.is_ok() {
                                let mut state = worker_state
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);

                                if let Some(value) = pan_target {
                                    state.target_pan = value;
                                }

                                if let Some(value) = tilt_target {
                                    state.target_tilt = value;
                                }

                                if let Some(value) = zoom_target {
                                    state.target_zoom = value;
                                }
                            }

                            let result = match apply_result {
                                Ok(()) => wait_for_worker_targets(
                                    &device,
                                    &worker_capabilities,
                                    &worker_state,
                                    pan_target,
                                    tilt_target,
                                    zoom_target,
                                ),
                                Err(error) => Err(error.to_string()),
                            };

                            let mut state = worker_state
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);

                            state.last_operation_ms = started.elapsed().as_secs_f32() * 1000.0;

                            state.last_error = result.err();
                        }
                        WorkerCommand::Relative(axis, amount) => {
                            let started = Instant::now();

                            let result = (|| -> Result<(), String> {
                                let current = cameras::read_controls(&device)
                                    .map_err(|error| error.to_string())?;

                                {
                                    let mut state = worker_state
                                        .lock()
                                        .unwrap_or_else(std::sync::PoisonError::into_inner);

                                    state.actual_pan = current.pan;
                                    state.actual_tilt = current.tilt;
                                    state.actual_zoom = current.zoom;
                                }

                                let Some(range) = range_for_axis(&worker_capabilities, axis) else {
                                    return Err(format!(
                                        "{} is not supported by this camera.",
                                        axis.label()
                                    ));
                                };

                                let current_value = match axis {
                                    Axis::Pan => current.pan,
                                    Axis::Tilt => current.tilt,
                                    Axis::Zoom => current.zoom,
                                }
                                .unwrap_or(range.default);

                                let target = snap_to_step(
                                    (current_value + amount).clamp(range.min, range.max),
                                    range,
                                );

                                let controls = match axis {
                                    Axis::Pan => Controls {
                                        pan: Some(target),
                                        ..Default::default()
                                    },
                                    Axis::Tilt => Controls {
                                        tilt: Some(target),
                                        ..Default::default()
                                    },
                                    Axis::Zoom => Controls {
                                        zoom: Some(target),
                                        ..Default::default()
                                    },
                                };

                                cameras::apply_controls(&device, &controls)
                                    .map_err(|error| error.to_string())?;

                                {
                                    let mut state = worker_state
                                        .lock()
                                        .unwrap_or_else(std::sync::PoisonError::into_inner);

                                    match axis {
                                        Axis::Pan => state.target_pan = target,
                                        Axis::Tilt => state.target_tilt = target,
                                        Axis::Zoom => state.target_zoom = target,
                                    }
                                }

                                let (pan_target, tilt_target, zoom_target) = match axis {
                                    Axis::Pan => (Some(target), None, None),
                                    Axis::Tilt => (None, Some(target), None),
                                    Axis::Zoom => (None, None, Some(target)),
                                };

                                wait_for_worker_targets(
                                    &device,
                                    &worker_capabilities,
                                    &worker_state,
                                    pan_target,
                                    tilt_target,
                                    zoom_target,
                                )
                            })();

                            let mut state = worker_state
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);

                            state.last_operation_ms = started.elapsed().as_secs_f32() * 1000.0;

                            state.last_error = result.err();
                        }
                        WorkerCommand::ReadPosition => {
                            let started = Instant::now();
                            let result = cameras::read_controls(&device);
                            let elapsed_ms = started.elapsed().as_secs_f32() * 1000.0;

                            let mut state = worker_state
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);

                            state.last_operation_ms = elapsed_ms;

                            match result {
                                Ok(controls) => {
                                    state.actual_pan = controls.pan;
                                    state.actual_tilt = controls.tilt;
                                    state.actual_zoom = controls.zoom;
                                    state.last_error = None;
                                }
                                Err(error) => {
                                    state.last_error = Some(error.to_string());
                                }
                            }
                        }
                        WorkerCommand::Shutdown => break,
                    }
                }
            })
            .expect("failed to start BareEye PTZ worker");

        Ok(Self {
            capabilities,
            sender,
            worker: Some(worker),
            state,
        })
    }

    pub fn pan_target(&self) -> f32 {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .target_pan
    }

    pub fn tilt_target(&self) -> f32 {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .target_tilt
    }

    pub fn zoom_target(&self) -> f32 {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .target_zoom
    }

    pub fn actual_pan(&self) -> Option<f32> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .actual_pan
    }

    pub fn actual_tilt(&self) -> Option<f32> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .actual_tilt
    }

    pub fn actual_zoom(&self) -> Option<f32> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .actual_zoom
    }

    pub fn worker_error(&self) -> Option<String> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .last_error
            .clone()
    }

    pub fn last_operation_ms(&self) -> f32 {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .last_operation_ms
    }

    pub fn refresh_actual(&self) -> Result<(), String> {
        self.send(WorkerCommand::ReadPosition)
    }

    pub fn zoom_range(&self) -> Option<ControlRange> {
        self.capabilities.zoom
    }

    pub fn pan_by(&self, amount: f32) -> Result<(), String> {
        self.send(WorkerCommand::Relative(Axis::Pan, amount))
    }

    pub fn tilt_by(&self, amount: f32) -> Result<(), String> {
        self.send(WorkerCommand::Relative(Axis::Tilt, amount))
    }

    pub fn zoom_by(&self, amount: f32) -> Result<(), String> {
        self.send(WorkerCommand::Relative(Axis::Zoom, amount))
    }

    pub fn set_zoom(&self, value: f32) -> Result<(), String> {
        self.set_axis_target(Axis::Zoom, value)
    }

    pub fn center(&self) -> Result<(), String> {
        let controls = Controls {
            pan: self.capabilities.pan.map(|range| range.default),
            tilt: self.capabilities.tilt.map(|range| range.default),
            ..Default::default()
        };

        self.send(WorkerCommand::Apply(controls))
    }

    pub fn wide(&self) -> Result<(), String> {
        let Some(range) = self.capabilities.zoom else {
            return Ok(());
        };

        self.set_axis_target(Axis::Zoom, range.default)
    }

    fn set_axis_target(&self, axis: Axis, value: f32) -> Result<(), String> {
        let Some(range) = range_for_axis(&self.capabilities, axis) else {
            return Ok(());
        };

        let value = snap_to_step(value.clamp(range.min, range.max), range);

        let controls = match axis {
            Axis::Pan => Controls {
                pan: Some(value),
                ..Default::default()
            },
            Axis::Tilt => Controls {
                tilt: Some(value),
                ..Default::default()
            },
            Axis::Zoom => Controls {
                zoom: Some(value),
                ..Default::default()
            },
        };

        self.send(WorkerCommand::Apply(controls))
    }

    fn send(&self, command: WorkerCommand) -> Result<(), String> {
        self.sender
            .send(command)
            .map_err(|_| "PTZ worker has stopped".to_owned())
    }
}

impl Drop for ManualController {
    fn drop(&mut self) {
        let _ = self.sender.send(WorkerCommand::Shutdown);

        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub fn run_console(device: &Device) -> Result<(), Box<dyn Error>> {
    println!();
    println!("BareEye PTZ test console");
    println!("========================");
    println!();
    print_help();
    print_state(device)?;

    loop {
        println!();
        print!("bareeye-ptz> ");
        io::stdout().flush()?;

        let mut line = String::new();

        if io::stdin().read_line(&mut line)? == 0 {
            break;
        }

        let parts: Vec<&str> = line.split_whitespace().collect();

        match parts.as_slice() {
            [] => {}
            ["help"] => print_help(),
            ["state"] => print_state(device)?,
            ["pan", value] => set_from_text(device, Axis::Pan, value)?,
            ["tilt", value] => set_from_text(device, Axis::Tilt, value)?,
            ["zoom", value] => set_from_text(device, Axis::Zoom, value)?,
            ["center"] => center_camera(device)?,
            ["wide"] => set_zoom_default(device)?,
            ["quit"] | ["exit"] => break,
            _ => {
                println!("Unknown command.");
                println!("Type 'help' to show the available commands.");
            }
        }
    }

    println!("PTZ test console closed.");

    Ok(())
}

fn print_help() {
    println!("Commands:");
    println!("    state          Read current pan, tilt, and zoom");
    println!("    pan <value>    Set absolute pan position");
    println!("    tilt <value>   Set absolute tilt position");
    println!("    zoom <value>   Set absolute zoom position");
    println!("    center         Return pan and tilt to their defaults");
    println!("    wide           Return zoom to its default");
    println!("    help            Show this help");
    println!("    quit            Exit the PTZ console");
}

fn print_state(device: &Device) -> Result<(), cameras::Error> {
    let controls = cameras::read_controls(device)?;

    println!(
        "PTZ state: pan={:?}, tilt={:?}, zoom={:?}",
        controls.pan, controls.tilt, controls.zoom
    );

    Ok(())
}

fn set_from_text(device: &Device, axis: Axis, text: &str) -> Result<(), cameras::Error> {
    let value = match text.parse::<f32>() {
        Ok(value) => value,
        Err(_) => {
            println!("'{text}' is not a valid number.");
            return Ok(());
        }
    };

    set_axis(device, axis, value)
}

fn set_axis(device: &Device, axis: Axis, value: f32) -> Result<(), cameras::Error> {
    let capabilities = cameras::control_capabilities(device)?;

    let Some(range) = range_for_axis(&capabilities, axis) else {
        println!("{} is not supported by this camera.", axis.label());
        return Ok(());
    };

    if value < range.min || value > range.max {
        println!(
            "{} value {} is outside the supported range {}..={}.",
            axis.label(),
            value,
            range.min,
            range.max
        );
        return Ok(());
    }

    let value = snap_to_step(value, range);

    let controls = match axis {
        Axis::Pan => Controls {
            pan: Some(value),
            ..Default::default()
        },
        Axis::Tilt => Controls {
            tilt: Some(value),
            ..Default::default()
        },
        Axis::Zoom => Controls {
            zoom: Some(value),
            ..Default::default()
        },
    };

    cameras::apply_controls(device, &controls)?;

    println!("{} commanded to {}.", axis.label(), value);
    wait_for_axis(device, axis, value, range.step.max(1.0))?;
    print_state(device)?;

    Ok(())
}

fn center_camera(device: &Device) -> Result<(), cameras::Error> {
    let capabilities = cameras::control_capabilities(device)?;
    let pan_range = capabilities.pan;
    let tilt_range = capabilities.tilt;

    let controls = Controls {
        pan: pan_range.map(|range| range.default),
        tilt: tilt_range.map(|range| range.default),
        ..Default::default()
    };

    cameras::apply_controls(device, &controls)?;

    println!("Pan and tilt commanded to their default positions.");

    if let Some(range) = pan_range {
        wait_for_axis(device, Axis::Pan, range.default, range.step.max(1.0))?;
    }

    if let Some(range) = tilt_range {
        wait_for_axis(device, Axis::Tilt, range.default, range.step.max(1.0))?;
    }

    print_state(device)?;

    Ok(())
}

fn set_zoom_default(device: &Device) -> Result<(), cameras::Error> {
    let capabilities = cameras::control_capabilities(device)?;

    let Some(range) = capabilities.zoom else {
        println!("Zoom is not supported by this camera.");
        return Ok(());
    };

    set_axis(device, Axis::Zoom, range.default)
}

fn wait_for_axis(
    device: &Device,
    axis: Axis,
    target: f32,
    tolerance: f32,
) -> Result<(), cameras::Error> {
    let started = Instant::now();
    let mut last_position = None;

    loop {
        let controls = cameras::read_controls(device)?;

        let position = match axis {
            Axis::Pan => controls.pan,
            Axis::Tilt => controls.tilt,
            Axis::Zoom => controls.zoom,
        };

        let Some(position) = position else {
            println!("{} position is no longer readable.", axis.label());
            return Ok(());
        };

        if last_position != Some(position) {
            println!("    {} position: {}", axis.label(), position);
            last_position = Some(position);
        }

        if (position - target).abs() <= tolerance {
            println!("{} reached {}.", axis.label(), position);
            return Ok(());
        }

        if started.elapsed() >= MOVE_TIMEOUT {
            println!(
                "{} movement timed out at {} while targeting {}.",
                axis.label(),
                position,
                target
            );
            return Ok(());
        }

        thread::sleep(MOVE_POLL_INTERVAL);
    }
}

fn range_for_axis(capabilities: &ControlCapabilities, axis: Axis) -> Option<ControlRange> {
    match axis {
        Axis::Pan => capabilities.pan,
        Axis::Tilt => capabilities.tilt,
        Axis::Zoom => capabilities.zoom,
    }
}

fn snap_to_step(value: f32, range: ControlRange) -> f32 {
    if range.step <= 0.0 {
        return value.clamp(range.min, range.max);
    }

    let steps = ((value - range.min) / range.step).round();
    let snapped = range.min + steps * range.step;

    snapped.clamp(range.min, range.max)
}
