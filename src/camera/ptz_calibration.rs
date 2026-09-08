use super::relative_ptz::RelativePtzController;
use cameras::{ControlRange, Controls, Device};
use std::error::Error;
use std::io;
use std::thread;
use std::time::{Duration, Instant};

const CALIBRATION_REPEATS: usize = 3;
const CALIBRATION_MOVE_TIME: Duration = Duration::from_millis(200);
const CALIBRATION_SETTLE_TIME: Duration = Duration::from_millis(400);
const CALIBRATION_HOME_SETTLE_TIME: Duration = Duration::from_millis(250);
const CALIBRATION_HOME_POLL: Duration = Duration::from_millis(50);
const CALIBRATION_HOME_TIMEOUT: Duration = Duration::from_secs(8);

const PAN_SPEEDS: &[i32] = &[1, 2, 4, 8, 12, 16, 20, 24];
const TILT_SPEEDS: &[i32] = &[1, 2, 4, 8, 12, 16, 20];

#[derive(Copy, Clone)]
enum CalibrationAxis {
    Pan,
    Tilt,
}

impl CalibrationAxis {
    fn label(self) -> &'static str {
        match self {
            Self::Pan => "PAN",
            Self::Tilt => "TILT",
        }
    }

    fn command(self, speed: i32) -> (i32, i32) {
        match self {
            Self::Pan => (speed, 0),
            Self::Tilt => (0, speed),
        }
    }
}

struct CalibrationConfig {
    home_pan: f32,
    home_tilt: f32,
    pan_tolerance: f32,
    tilt_tolerance: f32,
}

struct CalibrationSummary {
    axis: &'static str,
    direction: i32,
    speed: i32,
    average_delta_deg: f32,
    average_rate_deg_s: f32,
    minimum_rate_deg_s: f32,
    maximum_rate_deg_s: f32,
}

pub fn run(device: &Device) -> Result<(), Box<dyn Error>> {
    println!();
    println!("BareEye automatic relative PTZ calibration");
    println!("==========================================");
    println!("Device: {}", device.name);

    let capabilities = cameras::control_capabilities(device)?;

    let pan_range = capabilities
        .pan
        .ok_or_else(|| io::Error::other("Camera does not expose an absolute pan range"))?;

    let tilt_range = capabilities
        .tilt
        .ok_or_else(|| io::Error::other("Camera does not expose an absolute tilt range"))?;

    let original = cameras::read_controls(device)?;

    let original_pan = original.pan.unwrap_or(pan_range.default);
    let original_tilt = original.tilt.unwrap_or(tilt_range.default);

    let config = CalibrationConfig {
        home_pan: calibration_midpoint(&pan_range),
        home_tilt: calibration_midpoint(&tilt_range),
        pan_tolerance: pan_range.step.abs().max(1.0),
        tilt_tolerance: tilt_range.step.abs().max(1.0),
    };

    println!("Original position: pan={original_pan:.1}, tilt={original_tilt:.1}");
    println!(
        "Calibration home: pan={:.1}, tilt={:.1}",
        config.home_pan, config.home_tilt
    );
    println!(
        "Motion time: {} ms | repeats: {}",
        CALIBRATION_MOVE_TIME.as_millis(),
        CALIBRATION_REPEATS
    );

    let controller = RelativePtzController::open(device).map_err(io::Error::other)?;

    controller.stop().map_err(io::Error::other)?;

    let test_result = (|| -> Result<Vec<CalibrationSummary>, String> {
        let mut summaries = Vec::new();

        for &speed in PAN_SPEEDS {
            for direction in [1, -1] {
                summaries.push(run_setting(
                    device,
                    &controller,
                    CalibrationAxis::Pan,
                    speed,
                    direction,
                    &config,
                )?);
            }
        }

        for &speed in TILT_SPEEDS {
            for direction in [1, -1] {
                summaries.push(run_setting(
                    device,
                    &controller,
                    CalibrationAxis::Tilt,
                    speed,
                    direction,
                    &config,
                )?);
            }
        }

        Ok(summaries)
    })();

    let stop_result = controller.stop();

    println!();
    println!("Restoring original camera position...");

    let restore_result = move_absolute_and_wait(
        device,
        original_pan,
        original_tilt,
        config.pan_tolerance,
        config.tilt_tolerance,
    );

    let summaries = test_result.map_err(io::Error::other)?;
    stop_result.map_err(io::Error::other)?;
    restore_result.map_err(io::Error::other)?;

    println!();
    println!("Calibration summary");
    println!("===================");
    println!(
        "axis,direction,command_speed,avg_delta_deg,avg_abs_deg_s,min_abs_deg_s,max_abs_deg_s"
    );

    for summary in summaries {
        let direction = if summary.direction > 0 { "+" } else { "-" };

        println!(
            "{},{},{},{:.3},{:.3},{:.3},{:.3}",
            summary.axis,
            direction,
            summary.speed,
            summary.average_delta_deg,
            summary.average_rate_deg_s,
            summary.minimum_rate_deg_s,
            summary.maximum_rate_deg_s
        );
    }

    println!();
    println!("Calibration complete.");
    println!("Camera restored to pan={original_pan:.1}, tilt={original_tilt:.1}");

    Ok(())
}

fn run_setting(
    device: &Device,
    controller: &RelativePtzController,
    axis: CalibrationAxis,
    speed: i32,
    direction: i32,
    config: &CalibrationConfig,
) -> Result<CalibrationSummary, String> {
    let signed_speed = speed * direction;
    let direction_label = if direction > 0 { "+" } else { "-" };

    println!();
    println!("{} {} speed {}", axis.label(), direction_label, speed);
    println!("----------------");

    let mut delta_sum = 0.0_f32;
    let mut rate_sum = 0.0_f32;
    let mut minimum_rate = f32::INFINITY;
    let mut maximum_rate = f32::NEG_INFINITY;

    for repeat in 1..=CALIBRATION_REPEATS {
        move_absolute_and_wait(
            device,
            config.home_pan,
            config.home_tilt,
            config.pan_tolerance,
            config.tilt_tolerance,
        )?;

        thread::sleep(CALIBRATION_HOME_SETTLE_TIME);

        let before = read_axis_position(device, axis)?;

        let (pan_speed, tilt_speed) = axis.command(signed_speed);

        let started = Instant::now();

        controller.set_speed(pan_speed, tilt_speed)?;

        thread::sleep(CALIBRATION_MOVE_TIME);

        controller.stop()?;

        let elapsed = started.elapsed();

        thread::sleep(CALIBRATION_SETTLE_TIME);

        let after = read_axis_position(device, axis)?;

        let delta = after - before;
        let elapsed_seconds = elapsed.as_secs_f32().max(0.001);
        let rate = delta.abs() / elapsed_seconds;

        delta_sum += delta;
        rate_sum += rate;
        minimum_rate = minimum_rate.min(rate);
        maximum_rate = maximum_rate.max(rate);

        println!(
            "  trial {repeat}: start={before:.1} end={after:.1} delta={delta:+.1} | {:.0} ms | {:.2} deg/s",
            elapsed.as_secs_f32() * 1000.0,
            rate
        );
    }

    let repeats = CALIBRATION_REPEATS as f32;

    let average_delta = delta_sum / repeats;
    let average_rate = rate_sum / repeats;

    println!(
        "  AVG: delta={average_delta:+.2} deg | rate={average_rate:.2} deg/s | range={minimum_rate:.2}..{maximum_rate:.2}"
    );

    Ok(CalibrationSummary {
        axis: axis.label(),
        direction,
        speed,
        average_delta_deg: average_delta,
        average_rate_deg_s: average_rate,
        minimum_rate_deg_s: minimum_rate,
        maximum_rate_deg_s: maximum_rate,
    })
}

fn read_axis_position(device: &Device, axis: CalibrationAxis) -> Result<f32, String> {
    let controls = cameras::read_controls(device).map_err(|error| error.to_string())?;

    let value = match axis {
        CalibrationAxis::Pan => controls.pan,
        CalibrationAxis::Tilt => controls.tilt,
    };

    value.ok_or_else(|| format!("{} absolute position is unavailable", axis.label()))
}

fn move_absolute_and_wait(
    device: &Device,
    pan: f32,
    tilt: f32,
    pan_tolerance: f32,
    tilt_tolerance: f32,
) -> Result<(), String> {
    let controls = Controls {
        pan: Some(pan),
        tilt: Some(tilt),
        ..Default::default()
    };

    cameras::apply_controls(device, &controls).map_err(|error| error.to_string())?;

    let started = Instant::now();

    loop {
        let current = cameras::read_controls(device).map_err(|error| error.to_string())?;

        let pan_done = current
            .pan
            .is_some_and(|value| (value - pan).abs() <= pan_tolerance);

        let tilt_done = current
            .tilt
            .is_some_and(|value| (value - tilt).abs() <= tilt_tolerance);

        if pan_done && tilt_done {
            return Ok(());
        }

        if started.elapsed() >= CALIBRATION_HOME_TIMEOUT {
            return Err(format!(
                "Timed out moving to calibration home: wanted pan={pan:.1} tilt={tilt:.1}, actual pan={:?} tilt={:?}",
                current.pan, current.tilt
            ));
        }

        thread::sleep(CALIBRATION_HOME_POLL);
    }
}

fn calibration_midpoint(range: &ControlRange) -> f32 {
    let midpoint = range.min + (range.max - range.min) * 0.5;
    let step = range.step.abs();

    if step <= f32::EPSILON {
        return midpoint.clamp(range.min, range.max);
    }

    let steps_from_min = ((midpoint - range.min) / step).round();

    (range.min + steps_from_min * step).clamp(range.min, range.max)
}
