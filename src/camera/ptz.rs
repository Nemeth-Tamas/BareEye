use cameras::{ControlCapabilities, ControlRange, Controls, Device};
use std::error::Error;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const MOVE_POLL_INTERVAL: Duration = Duration::from_millis(50);
const MOVE_TIMEOUT: Duration = Duration::from_secs(5);

const TRACKING_PWM_TICK: Duration = Duration::from_millis(20);
const TRACKING_PWM_PERIOD: Duration = Duration::from_millis(300);
const TRACKING_PWM_ON_TIME: Duration = Duration::from_millis(200);
const TRACKING_PWM_WATCHDOG: Duration = Duration::from_millis(500);

const TRACKING_PAN_LIMIT_MARGIN_DEG: f32 = 8.0;
const TRACKING_TILT_LIMIT_MARGIN_DEG: f32 = 8.0;

struct TrackingPwm {
    pan: i32,
    tilt: i32,
    cycle_started: Instant,
    last_refresh: Instant,
    output_pan: i32,
    output_tilt: i32,
}

impl TrackingPwm {
    fn new() -> Self {
        let now = Instant::now();

        Self {
            pan: 0,
            tilt: 0,
            cycle_started: now,
            last_refresh: now,
            output_pan: 0,
            output_tilt: 0,
        }
    }

    fn set_command(&mut self, pan: i32, tilt: i32) {
        let pan = pan.signum();
        let tilt = tilt.signum();
        let now = Instant::now();

        if self.pan != pan || self.tilt != tilt {
            self.cycle_started = now;
        }

        self.pan = pan;
        self.tilt = tilt;
        self.last_refresh = now;
    }

    fn clear(&mut self) {
        let now = Instant::now();

        self.pan = 0;
        self.tilt = 0;
        self.cycle_started = now;
        self.last_refresh = now;
    }

    fn next_output(&mut self, now: Instant) -> Option<(i32, i32)> {
        if now.duration_since(self.last_refresh) >= TRACKING_PWM_WATCHDOG {
            self.pan = 0;
            self.tilt = 0;
        }

        let output = if self.pan == 0 && self.tilt == 0 {
            (0, 0)
        } else {
            let mut elapsed = now.duration_since(self.cycle_started);

            if elapsed >= TRACKING_PWM_PERIOD {
                self.cycle_started = now;
                elapsed = Duration::ZERO;
            }

            if elapsed < TRACKING_PWM_ON_TIME {
                (self.pan, self.tilt)
            } else {
                (0, 0)
            }
        };

        if output == (self.output_pan, self.output_tilt) {
            return None;
        }

        self.output_pan = output.0;
        self.output_tilt = output.1;

        Some(output)
    }
}

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
    TrackVelocity { pan: i32, tilt: i32 },
    TrackAbsolute { pan_delta: f32, tilt_delta: f32 },
    StopTracking,
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

fn tracking_safe_bounds(range: ControlRange, margin: f32) -> (f32, f32) {
    let safe_min = range.min + margin;
    let safe_max = range.max - margin;

    if safe_min <= safe_max {
        (safe_min, safe_max)
    } else {
        (range.min, range.max)
    }
}

fn tracking_safe_target(value: f32, range: ControlRange, margin: f32) -> f32 {
    let (safe_min, safe_max) = tracking_safe_bounds(range, margin);

    snap_to_step(value.clamp(safe_min, safe_max), range)
}

fn limit_tracking_output(
    device: &Device,
    capabilities: &ControlCapabilities,
    pan: i32,
    tilt: i32,
) -> Result<(i32, i32), String> {
    let controls = cameras::read_controls(device).map_err(|error| error.to_string())?;

    let mut safe_pan = pan;
    let mut safe_tilt = tilt;

    if let (Some(actual), Some(range)) = (controls.pan, capabilities.pan) {
        let (safe_min, safe_max) = tracking_safe_bounds(range, TRACKING_PAN_LIMIT_MARGIN_DEG);

        if (safe_pan < 0 && actual <= safe_min) || (safe_pan > 0 && actual >= safe_max) {
            safe_pan = 0;
        }
    }

    if let (Some(actual), Some(range)) = (controls.tilt, capabilities.tilt) {
        let (safe_min, safe_max) = tracking_safe_bounds(range, TRACKING_TILT_LIMIT_MARGIN_DEG);

        if (safe_tilt < 0 && actual <= safe_min) || (safe_tilt > 0 && actual >= safe_max) {
            safe_tilt = 0;
        }
    }

    Ok((safe_pan, safe_tilt))
}

fn apply_tracking_output(
    relative_ptz: &mut Option<Result<crate::camera::relative_ptz::RelativePtzController, String>>,
    device: &Device,
    pan: i32,
    tilt: i32,
) -> Result<(), String> {
    if pan == 0 && tilt == 0 {
        return match relative_ptz.as_ref() {
            Some(Ok(controller)) => controller.stop(),
            Some(Err(error)) => Err(error.clone()),
            None => Ok(()),
        };
    }

    let controller = relative_ptz
        .get_or_insert_with(|| crate::camera::relative_ptz::RelativePtzController::open(device));

    match controller {
        Ok(controller) => controller.set_speed(pan, tilt),
        Err(error) => Err(error.clone()),
    }
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
    tracking_pending: std::sync::Arc<AtomicBool>,
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

        let tracking_pending = std::sync::Arc::new(AtomicBool::new(false));
        let worker_tracking_pending = std::sync::Arc::clone(&tracking_pending);

        let (sender, receiver) = std::sync::mpsc::channel();

        let worker = thread::Builder::new()
            .name("bareeye-ptz".to_owned())
            .spawn(move || {
                let mut relative_ptz = None;
                let mut tracking_pwm = TrackingPwm::new();

                loop {
                    let command = match receiver.recv_timeout(TRACKING_PWM_TICK) {
                        Ok(command) => Some(command),
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    };

                    if let Some(command) = command {
                        match command {
                            WorkerCommand::Apply(controls) => {
                                tracking_pwm.clear();
                                let _ = apply_tracking_output(&mut relative_ptz, &device, 0, 0);

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
                                tracking_pwm.clear();
                                let _ = apply_tracking_output(&mut relative_ptz, &device, 0, 0);

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

                                    let Some(range) = range_for_axis(&worker_capabilities, axis)
                                    else {
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
                            WorkerCommand::TrackVelocity { pan, tilt } => {
                                tracking_pwm.set_command(pan, tilt);

                                worker_tracking_pending.store(false, Ordering::Release);
                            }
                            WorkerCommand::TrackAbsolute {
                                pan_delta,
                                tilt_delta,
                            } => {
                                tracking_pwm.clear();

                                let _ = apply_tracking_output(&mut relative_ptz, &device, 0, 0);

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

                                    let pan_target = if pan_delta.abs() > f32::EPSILON {
                                        let Some(range) = worker_capabilities.pan else {
                                            return Err(
                                                "Pan is not supported by this camera.".to_owned()
                                            );
                                        };

                                        let actual = current.pan.unwrap_or(range.default);

                                        Some(tracking_safe_target(
                                            actual + pan_delta,
                                            range,
                                            TRACKING_PAN_LIMIT_MARGIN_DEG,
                                        ))
                                    } else {
                                        None
                                    };

                                    let tilt_target = if tilt_delta.abs() > f32::EPSILON {
                                        let Some(range) = worker_capabilities.tilt else {
                                            return Err(
                                                "Tilt is not supported by this camera.".to_owned()
                                            );
                                        };

                                        let actual = current.tilt.unwrap_or(range.default);

                                        Some(tracking_safe_target(
                                            actual + tilt_delta,
                                            range,
                                            TRACKING_TILT_LIMIT_MARGIN_DEG,
                                        ))
                                    } else {
                                        None
                                    };

                                    if pan_target.is_none() && tilt_target.is_none() {
                                        return Ok(());
                                    }

                                    let controls = Controls {
                                        pan: pan_target,
                                        tilt: tilt_target,
                                        ..Default::default()
                                    };

                                    cameras::apply_controls(&device, &controls)
                                        .map_err(|error| error.to_string())?;

                                    {
                                        let mut state = worker_state
                                            .lock()
                                            .unwrap_or_else(std::sync::PoisonError::into_inner);

                                        if let Some(target) = pan_target {
                                            state.target_pan = target;
                                        }

                                        if let Some(target) = tilt_target {
                                            state.target_tilt = target;
                                        }
                                    }

                                    wait_for_worker_targets(
                                        &device,
                                        &worker_capabilities,
                                        &worker_state,
                                        pan_target,
                                        tilt_target,
                                        None,
                                    )
                                })();

                                let mut state = worker_state
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);

                                state.last_operation_ms = started.elapsed().as_secs_f32() * 1000.0;
                                state.last_error = result.err();

                                worker_tracking_pending.store(false, Ordering::Release);
                            }
                            WorkerCommand::StopTracking => {
                                tracking_pwm.clear();

                                let started = Instant::now();
                                let result =
                                    apply_tracking_output(&mut relative_ptz, &device, 0, 0);

                                let mut state = worker_state
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);

                                state.last_operation_ms = started.elapsed().as_secs_f32() * 1000.0;
                                state.last_error = result.err();

                                worker_tracking_pending.store(false, Ordering::Release);
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
                            WorkerCommand::Shutdown => {
                                tracking_pwm.clear();
                                let _ = apply_tracking_output(&mut relative_ptz, &device, 0, 0);

                                break;
                            }
                        }
                    }

                    if let Some((pan, tilt)) = tracking_pwm.next_output(Instant::now()) {
                        let started = Instant::now();

                        let result = if pan == 0 && tilt == 0 {
                            apply_tracking_output(&mut relative_ptz, &device, 0, 0)
                        } else {
                            match limit_tracking_output(&device, &worker_capabilities, pan, tilt) {
                                Ok((safe_pan, safe_tilt)) => apply_tracking_output(
                                    &mut relative_ptz,
                                    &device,
                                    safe_pan,
                                    safe_tilt,
                                ),
                                Err(error) => {
                                    let _ = apply_tracking_output(&mut relative_ptz, &device, 0, 0);

                                    Err(error)
                                }
                            }
                        };

                        let mut state = worker_state
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);

                        state.last_operation_ms = started.elapsed().as_secs_f32() * 1000.0;
                        state.last_error = result.err();
                    }
                }
            })
            .expect("failed to start BareEye PTZ worker");

        Ok(Self {
            capabilities,
            sender,
            worker: Some(worker),
            state,
            tracking_pending,
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

    pub fn tracking_busy(&self) -> bool {
        self.tracking_pending.load(Ordering::Acquire)
    }

    pub fn track_velocity(&self, pan: i32, tilt: i32) -> Result<bool, String> {
        if pan == 0 && tilt == 0 {
            self.stop_tracking()?;
            return Ok(true);
        }

        if self.tracking_pending.swap(true, Ordering::AcqRel) {
            return Ok(false);
        }

        if let Err(error) = self.send(WorkerCommand::TrackVelocity { pan, tilt }) {
            self.tracking_pending.store(false, Ordering::Release);
            return Err(error);
        }

        Ok(true)
    }

    pub fn track_absolute_offset(&self, pan_delta: f32, tilt_delta: f32) -> Result<bool, String> {
        if pan_delta.abs() <= f32::EPSILON && tilt_delta.abs() <= f32::EPSILON {
            return Ok(false);
        }

        if self.tracking_pending.swap(true, Ordering::AcqRel) {
            return Ok(false);
        }

        if let Err(error) = self.send(WorkerCommand::TrackAbsolute {
            pan_delta,
            tilt_delta,
        }) {
            self.tracking_pending.store(false, Ordering::Release);
            return Err(error);
        }

        Ok(true)
    }

    pub fn stop_tracking(&self) -> Result<(), String> {
        self.tracking_pending.store(true, Ordering::Release);

        if let Err(error) = self.send(WorkerCommand::StopTracking) {
            self.tracking_pending.store(false, Ordering::Release);
            return Err(error);
        }

        Ok(())
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
