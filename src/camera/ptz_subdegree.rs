use cameras::{Controls, Device, Frame};
use eframe::egui;
use std::error::Error;
use std::io;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

const BASE_PAN: f32 = 0.0;
const BASE_TILT: f32 = 30.0;

const TEST_OFFSETS: &[f32] = &[0.25, 0.50, 0.75, 1.00, -0.25, -0.50, -0.75, -1.00];

const FRACTIONAL_SETTLE_TIME: Duration = Duration::from_millis(1500);
const BASE_SETTLE_TIME: Duration = Duration::from_millis(400);
const BASE_POLL_INTERVAL: Duration = Duration::from_millis(50);
const BASE_MOVE_TIMEOUT: Duration = Duration::from_secs(8);
const FRAME_TIMEOUT: Duration = Duration::from_secs(2);

const MAX_IMAGE_SHIFT_PIXELS: i32 = 80;

#[derive(Copy, Clone)]
enum TestAxis {
    Pan,
    Tilt,
}

impl TestAxis {
    fn label(self) -> &'static str {
        match self {
            Self::Pan => "PAN",
            Self::Tilt => "TILT",
        }
    }
}

struct ImageProfiles {
    x: Vec<f32>,
    y: Vec<f32>,
}

pub fn run(device: &Device) -> Result<(), Box<dyn Error>> {
    println!();
    println!("BareEye sub-degree absolute PTZ test");
    println!("====================================");
    println!("Device: {}", device.name);

    let capabilities = cameras::control_capabilities(device)?;

    let pan_range = capabilities
        .pan
        .ok_or_else(|| io::Error::other("Camera does not expose absolute pan control"))?;

    let tilt_range = capabilities
        .tilt
        .ok_or_else(|| io::Error::other("Camera does not expose absolute tilt control"))?;

    println!(
        "Advertised pan step: {:.3} deg | tilt step: {:.3} deg",
        pan_range.step, tilt_range.step
    );

    let original = cameras::read_controls(device)?;

    let original_pan = original.pan.unwrap_or(pan_range.default);
    let original_tilt = original.tilt.unwrap_or(tilt_range.default);

    println!("Original position: pan={original_pan:.3}, tilt={original_tilt:.3}");
    println!("Test base: pan={BASE_PAN:.3}, tilt={BASE_TILT:.3}");
    println!(
        "Fractional movement settle: {} ms",
        FRACTIONAL_SETTLE_TIME.as_millis()
    );

    let (camera, _) = super::open_preview(device)?;

    let latest_frame = Arc::new(Mutex::new(None::<Frame>));
    let pump_latest_frame = Arc::clone(&latest_frame);

    let pump = cameras::pump::spawn(camera, move |frame| {
        *pump_latest_frame
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(frame);
    });

    thread::sleep(Duration::from_millis(500));

    let test_result = run_tests(device, &latest_frame);

    println!();
    println!("Restoring original camera position...");

    let restore_result = move_to_base(device, original_pan, original_tilt);

    egui_cameras::stop_and_join(pump);

    test_result.map_err(io::Error::other)?;
    restore_result.map_err(io::Error::other)?;

    println!();
    println!("Camera restored to pan={original_pan:.3}, tilt={original_tilt:.3}");

    Ok(())
}

fn run_tests(device: &Device, latest_frame: &Arc<Mutex<Option<Frame>>>) -> Result<(), String> {
    println!();
    println!("Stationary image-shift reference");
    println!("--------------------------------");

    move_to_base(device, BASE_PAN, BASE_TILT)?;

    let stationary_before = capture_profiles(latest_frame)?;

    thread::sleep(Duration::from_millis(250));

    let stationary_after = capture_profiles(latest_frame)?;

    let (stationary_x, _) = estimate_shift(
        &stationary_before.x,
        &stationary_after.x,
        MAX_IMAGE_SHIFT_PIXELS,
    );

    let (stationary_y, _) = estimate_shift(
        &stationary_before.y,
        &stationary_after.y,
        MAX_IMAGE_SHIFT_PIXELS,
    );

    println!("No PTZ command: x_shift={stationary_x:+} px, y_shift={stationary_y:+} px");

    println!();
    println!("Results");
    println!("=======");
    println!(
        "axis,requested_deg,reported_before,reported_after,reported_delta,image_shift_px,profile_error"
    );

    run_axis(device, latest_frame, TestAxis::Pan)?;

    run_axis(device, latest_frame, TestAxis::Tilt)?;

    println!();
    println!("Interpretation:");
    println!("  reported_delta < 1 deg + non-zero image shift = possible hidden fractional PTZ");
    println!(
        "  integer readback + proportional pixel shifts = likely fractional physical movement"
    );
    println!(
        "  0 / 1 deg jumps with matching image shifts = camera probably quantizes to 1 degree"
    );

    Ok(())
}

fn run_axis(
    device: &Device,
    latest_frame: &Arc<Mutex<Option<Frame>>>,
    axis: TestAxis,
) -> Result<(), String> {
    for &offset in TEST_OFFSETS {
        move_to_base(device, BASE_PAN, BASE_TILT)?;

        let before_controls = cameras::read_controls(device).map_err(|error| error.to_string())?;

        let before_position = axis_position(&before_controls, axis)?;

        let before_image = capture_profiles(latest_frame)?;

        let (target_pan, target_tilt) = match axis {
            TestAxis::Pan => (BASE_PAN + offset, BASE_TILT),
            TestAxis::Tilt => (BASE_PAN, BASE_TILT + offset),
        };

        apply_absolute(device, target_pan, target_tilt)?;

        thread::sleep(FRACTIONAL_SETTLE_TIME);

        let after_controls = cameras::read_controls(device).map_err(|error| error.to_string())?;

        let after_position = axis_position(&after_controls, axis)?;

        let after_image = capture_profiles(latest_frame)?;

        let (pixel_shift, profile_error) = match axis {
            TestAxis::Pan => {
                estimate_shift(&before_image.x, &after_image.x, MAX_IMAGE_SHIFT_PIXELS)
            }
            TestAxis::Tilt => {
                estimate_shift(&before_image.y, &after_image.y, MAX_IMAGE_SHIFT_PIXELS)
            }
        };

        let reported_delta = after_position - before_position;

        println!(
            "{},{offset:+.2},{before_position:.3},{after_position:.3},{reported_delta:+.3},{pixel_shift:+},{profile_error:.3}",
            axis.label()
        );
    }

    Ok(())
}

fn apply_absolute(device: &Device, pan: f32, tilt: f32) -> Result<(), String> {
    let controls = Controls {
        pan: Some(pan),
        tilt: Some(tilt),
        ..Default::default()
    };

    cameras::apply_controls(device, &controls).map_err(|error| error.to_string())
}

fn move_to_base(device: &Device, pan: f32, tilt: f32) -> Result<(), String> {
    apply_absolute(device, pan, tilt)?;

    let started = Instant::now();

    loop {
        let current = cameras::read_controls(device).map_err(|error| error.to_string())?;

        let pan_done = current.pan.is_some_and(|value| (value - pan).abs() <= 1.0);

        let tilt_done = current
            .tilt
            .is_some_and(|value| (value - tilt).abs() <= 1.0);

        if pan_done && tilt_done {
            thread::sleep(BASE_SETTLE_TIME);
            return Ok(());
        }

        if started.elapsed() >= BASE_MOVE_TIMEOUT {
            return Err(format!(
                "Timed out moving to base: wanted pan={pan:.3} tilt={tilt:.3}, actual pan={:?} tilt={:?}",
                current.pan, current.tilt
            ));
        }

        thread::sleep(BASE_POLL_INTERVAL);
    }
}

fn axis_position(controls: &Controls, axis: TestAxis) -> Result<f32, String> {
    match axis {
        TestAxis::Pan => controls
            .pan
            .ok_or_else(|| "Pan readback is unavailable".to_owned()),
        TestAxis::Tilt => controls
            .tilt
            .ok_or_else(|| "Tilt readback is unavailable".to_owned()),
    }
}

fn capture_profiles(latest_frame: &Arc<Mutex<Option<Frame>>>) -> Result<ImageProfiles, String> {
    for _ in 0..10 {
        {
            let mut frame = latest_frame.lock().unwrap_or_else(PoisonError::into_inner);

            *frame = None;
        }

        let started = Instant::now();

        let frame = loop {
            let next = latest_frame
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();

            if let Some(frame) = next {
                break frame;
            }

            if started.elapsed() >= FRAME_TIMEOUT {
                return Err("Timed out waiting for a fresh camera frame".to_owned());
            }

            thread::sleep(Duration::from_millis(5));
        };

        if let Ok(image) = egui_cameras::frame_to_color_image(&frame) {
            return Ok(build_profiles(&image));
        }
    }

    Err("Could not decode a fresh MJPEG frame after 10 attempts".to_owned())
}

fn build_profiles(image: &egui::ColorImage) -> ImageProfiles {
    let width = image.size[0];
    let height = image.size[1];

    let crop_x_start = width / 10;
    let crop_x_end = width - crop_x_start;

    let crop_y_start = height / 10;
    let crop_y_end = height - crop_y_start;

    let mut x_profile = vec![0.0_f32; width];
    let mut y_profile = vec![0.0_f32; height];

    for (x, value) in x_profile.iter_mut().enumerate() {
        let mut total = 0.0_f32;

        for y in crop_y_start..crop_y_end {
            total += grayscale(image.pixels[y * width + x]);
        }

        *value = total / (crop_y_end - crop_y_start) as f32;
    }

    for (y, value) in y_profile.iter_mut().enumerate() {
        let mut total = 0.0_f32;

        for x in crop_x_start..crop_x_end {
            total += grayscale(image.pixels[y * width + x]);
        }

        *value = total / (crop_x_end - crop_x_start) as f32;
    }

    ImageProfiles {
        x: x_profile,
        y: y_profile,
    }
}

fn grayscale(pixel: egui::Color32) -> f32 {
    pixel.r() as f32 * 0.2126 + pixel.g() as f32 * 0.7152 + pixel.b() as f32 * 0.0722
}

fn estimate_shift(before: &[f32], after: &[f32], maximum_shift: i32) -> (i32, f32) {
    let length = before.len().min(after.len());

    if length < 10 {
        return (0, f32::INFINITY);
    }

    let mean_before = before.iter().take(length).sum::<f32>() / length as f32;
    let mean_after = after.iter().take(length).sum::<f32>() / length as f32;

    let margin = length / 10;

    let mut best_shift = 0;
    let mut best_error = f32::INFINITY;

    for shift in -maximum_shift..=maximum_shift {
        let start = margin.max((-shift).max(0) as usize);

        let end = (length - margin).min((length as i32 - shift.max(0)) as usize);

        if end <= start {
            continue;
        }

        let mut error = 0.0_f32;
        let mut count = 0_usize;

        for index in start..end {
            let shifted_index = (index as i32 + shift) as usize;

            let before_value = before[index] - mean_before;
            let after_value = after[shifted_index] - mean_after;

            error += (before_value - after_value).abs();
            count += 1;
        }

        let average_error = error / count as f32;

        if average_error < best_error {
            best_error = average_error;
            best_shift = shift;
        }
    }

    (best_shift, best_error)
}
