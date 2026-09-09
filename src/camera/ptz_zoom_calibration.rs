use cameras::{ControlRange, Controls, Device, Frame};
use eframe::egui;
use std::error::Error;
use std::io;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

const WIDE_HORIZONTAL_FOV_DEG: f32 = 72.5;
const PUBLISHED_TELE_HORIZONTAL_FOV_DEG: f32 = 6.9;
const PUBLISHED_OPTICAL_ZOOM: f32 = 12.0;

const ZOOM_SAMPLE_FRACTIONS: &[f32] = &[
    0.00, 0.05, 0.10, 0.20, 0.30, 0.40, 0.50, 0.60, 0.70, 0.80, 0.90, 1.00,
];

const ZOOM_SETTLE_TIME: Duration = Duration::from_millis(500);
const ZOOM_POLL_INTERVAL: Duration = Duration::from_millis(50);
const ZOOM_MOVE_TIMEOUT: Duration = Duration::from_secs(15);
const FRAME_TIMEOUT: Duration = Duration::from_secs(2);

const COARSE_SCALE_MIN: f32 = 1.0;
const COARSE_SCALE_MAX: f32 = 14.0;
const COARSE_SCALE_STEP: f32 = 0.05;
const FINE_SCALE_RADIUS: f32 = 0.12;
const FINE_SCALE_STEP: f32 = 0.005;

const GRID_WIDTH: usize = 48;
const GRID_HEIGHT: usize = 27;
const GRID_SPAN_X: f32 = 0.32;
const GRID_SPAN_Y: f32 = 0.32;

const ALIGNMENT_OFFSETS: &[f32] = &[-12.0, 0.0, 12.0];

pub fn run(device: &Device) -> Result<(), Box<dyn Error>> {
    println!();
    println!("BareEye optical zoom / FOV calibration");
    println!("======================================");
    println!("Device: {}", device.name);

    let capabilities = cameras::control_capabilities(device)?;

    let zoom_range = capabilities
        .zoom
        .ok_or_else(|| io::Error::other("Camera does not expose absolute zoom control"))?;

    let original = cameras::read_controls(device)?;
    let original_zoom = original.zoom.unwrap_or(zoom_range.default);

    println!(
        "Zoom control range: {:.0} .. {:.0}, step {:.0}, default {:.0}",
        zoom_range.min, zoom_range.max, zoom_range.step, zoom_range.default
    );

    println!(
        "Published EagleEye IV USB optical range: {:.1}x, {:.1}° .. {:.1}° HFOV",
        PUBLISHED_OPTICAL_ZOOM, PUBLISHED_TELE_HORIZONTAL_FOV_DEG, WIDE_HORIZONTAL_FOV_DEG
    );

    println!("Original zoom value: {original_zoom:.0}");

    let (camera, _) = super::open_preview(device)?;

    let latest_frame = Arc::new(Mutex::new(None::<Frame>));
    let pump_latest_frame = Arc::clone(&latest_frame);

    let pump = cameras::pump::spawn(camera, move |frame| {
        *pump_latest_frame
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(frame);
    });

    thread::sleep(Duration::from_millis(500));

    let calibration_result = run_calibration(device, &zoom_range, &latest_frame);

    println!();
    println!("Restoring original zoom...");

    let restore_result = move_zoom_and_wait(device, &zoom_range, original_zoom);

    egui_cameras::stop_and_join(pump);

    calibration_result.map_err(io::Error::other)?;
    restore_result.map_err(io::Error::other)?;

    println!("Camera restored to zoom={original_zoom:.0}");

    Ok(())
}

fn run_calibration(
    device: &Device,
    zoom_range: &ControlRange,
    latest_frame: &Arc<Mutex<Option<Frame>>>,
) -> Result<(), String> {
    println!();
    println!("Moving to maximum wide angle...");

    let wide_zoom = snap_to_step(zoom_range.min, zoom_range);

    let reported_wide = move_zoom_and_wait(device, zoom_range, wide_zoom)?;
    let wide_image = capture_image(latest_frame)?;

    println!();
    println!("Calibration results");
    println!("===================");
    println!("zoom_value,control_percent,reported_zoom,magnification,hfov_deg,match_score");

    println!("{wide_zoom:.0},0.0,{reported_wide:.0},1.000,{WIDE_HORIZONTAL_FOV_DEG:.3},1.00000");

    for fraction in ZOOM_SAMPLE_FRACTIONS.iter().copied().skip(1) {
        let requested = zoom_range.min + (zoom_range.max - zoom_range.min) * fraction;
        let target = snap_to_step(requested, zoom_range);

        let reported = move_zoom_and_wait(device, zoom_range, target)?;

        let image = capture_image(latest_frame)?;

        let (magnification, score) = estimate_magnification(&wide_image, &image);

        let hfov = horizontal_fov_for_magnification(magnification);

        println!(
            "{target:.0},{:.1},{reported:.0},{magnification:.3},{hfov:.3},{score:.5}",
            fraction * 100.0
        );
    }

    println!();
    println!("Published endpoint sanity check:");
    println!(
        "  expected maximum optical zoom: about {:.1}x",
        PUBLISHED_OPTICAL_ZOOM
    );
    println!(
        "  expected maximum-zoom HFOV: about {:.1}°",
        PUBLISHED_TELE_HORIZONTAL_FOV_DEG
    );

    Ok(())
}

fn move_zoom_and_wait(device: &Device, range: &ControlRange, target: f32) -> Result<f32, String> {
    let controls = Controls {
        zoom: Some(target),
        ..Default::default()
    };

    cameras::apply_controls(device, &controls).map_err(|error| error.to_string())?;

    let tolerance = range.step.abs().max(1.0);
    let started = Instant::now();

    loop {
        let current = cameras::read_controls(device).map_err(|error| error.to_string())?;

        if let Some(actual) = current.zoom
            && (actual - target).abs() <= tolerance
        {
            thread::sleep(ZOOM_SETTLE_TIME);
            return Ok(actual);
        }

        if started.elapsed() >= ZOOM_MOVE_TIMEOUT {
            return Err(format!(
                "Timed out moving zoom to {target:.0}; current zoom is {:?}",
                current.zoom
            ));
        }

        thread::sleep(ZOOM_POLL_INTERVAL);
    }
}

fn capture_image(latest_frame: &Arc<Mutex<Option<Frame>>>) -> Result<egui::ColorImage, String> {
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
            return Ok(image);
        }
    }

    Err("Could not decode a fresh MJPEG frame after 10 attempts".to_owned())
}

fn estimate_magnification(wide: &egui::ColorImage, zoomed: &egui::ColorImage) -> (f32, f32) {
    let coarse = search_scale(
        wide,
        zoomed,
        COARSE_SCALE_MIN,
        COARSE_SCALE_MAX,
        COARSE_SCALE_STEP,
    );

    let fine_min = (coarse.0 - FINE_SCALE_RADIUS).max(COARSE_SCALE_MIN);
    let fine_max = (coarse.0 + FINE_SCALE_RADIUS).min(COARSE_SCALE_MAX);

    search_scale(wide, zoomed, fine_min, fine_max, FINE_SCALE_STEP)
}

fn search_scale(
    wide: &egui::ColorImage,
    zoomed: &egui::ColorImage,
    minimum: f32,
    maximum: f32,
    step: f32,
) -> (f32, f32) {
    let mut best_scale = minimum;
    let mut best_score = -1.0_f32;

    let mut scale = minimum;

    while scale <= maximum + step * 0.5 {
        let score = best_alignment_score(wide, zoomed, scale);

        if score > best_score {
            best_score = score;
            best_scale = scale;
        }

        scale += step;
    }

    (best_scale, best_score)
}

fn best_alignment_score(wide: &egui::ColorImage, zoomed: &egui::ColorImage, scale: f32) -> f32 {
    let mut best_score = -1.0_f32;

    for &offset_y in ALIGNMENT_OFFSETS {
        for &offset_x in ALIGNMENT_OFFSETS {
            let score = correlation_score(wide, zoomed, scale, offset_x, offset_y);

            if score > best_score {
                best_score = score;
            }
        }
    }

    best_score
}

fn correlation_score(
    wide: &egui::ColorImage,
    zoomed: &egui::ColorImage,
    scale: f32,
    offset_x: f32,
    offset_y: f32,
) -> f32 {
    let wide_width = wide.size[0] as f32;
    let wide_height = wide.size[1] as f32;

    let zoomed_width = zoomed.size[0] as f32;
    let zoomed_height = zoomed.size[1] as f32;

    let mut sum_wide = 0.0_f32;
    let mut sum_zoomed = 0.0_f32;
    let mut sum_wide_squared = 0.0_f32;
    let mut sum_zoomed_squared = 0.0_f32;
    let mut sum_cross = 0.0_f32;
    let mut sample_count = 0.0_f32;

    for grid_y in 0..GRID_HEIGHT {
        let y_fraction = grid_y as f32 / (GRID_HEIGHT - 1) as f32;
        let normalized_y = (y_fraction * 2.0 - 1.0) * GRID_SPAN_Y;

        for grid_x in 0..GRID_WIDTH {
            let x_fraction = grid_x as f32 / (GRID_WIDTH - 1) as f32;
            let normalized_x = (x_fraction * 2.0 - 1.0) * GRID_SPAN_X;

            let zoomed_x = zoomed_width * 0.5 + normalized_x * zoomed_width;

            let zoomed_y = zoomed_height * 0.5 + normalized_y * zoomed_height;

            let wide_x = wide_width * 0.5 + normalized_x * wide_width / scale + offset_x;

            let wide_y = wide_height * 0.5 + normalized_y * wide_height / scale + offset_y;

            let Some(wide_value) = sample_luma(wide, wide_x, wide_y) else {
                continue;
            };

            let Some(zoomed_value) = sample_luma(zoomed, zoomed_x, zoomed_y) else {
                continue;
            };

            sum_wide += wide_value;
            sum_zoomed += zoomed_value;

            sum_wide_squared += wide_value * wide_value;
            sum_zoomed_squared += zoomed_value * zoomed_value;

            sum_cross += wide_value * zoomed_value;
            sample_count += 1.0;
        }
    }

    if sample_count < 100.0 {
        return -1.0;
    }

    let covariance = sum_cross - sum_wide * sum_zoomed / sample_count;

    let wide_variance = sum_wide_squared - sum_wide * sum_wide / sample_count;

    let zoomed_variance = sum_zoomed_squared - sum_zoomed * sum_zoomed / sample_count;

    let denominator = (wide_variance * zoomed_variance).sqrt();

    if denominator <= f32::EPSILON {
        return -1.0;
    }

    covariance / denominator
}

fn sample_luma(image: &egui::ColorImage, x: f32, y: f32) -> Option<f32> {
    let width = image.size[0];
    let height = image.size[1];

    if width < 2 || height < 2 {
        return None;
    }

    let maximum_x = (width - 1) as f32;
    let maximum_y = (height - 1) as f32;

    if x < 0.0 || y < 0.0 || x > maximum_x || y > maximum_y {
        return None;
    }

    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;

    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);

    let x_fraction = x - x0 as f32;
    let y_fraction = y - y0 as f32;

    let top_left = luminance(image.pixels[y0 * width + x0]);
    let top_right = luminance(image.pixels[y0 * width + x1]);
    let bottom_left = luminance(image.pixels[y1 * width + x0]);
    let bottom_right = luminance(image.pixels[y1 * width + x1]);

    let top = lerp(top_left, top_right, x_fraction);
    let bottom = lerp(bottom_left, bottom_right, x_fraction);

    Some(lerp(top, bottom, y_fraction))
}

fn luminance(pixel: egui::Color32) -> f32 {
    pixel.r() as f32 * 0.2126 + pixel.g() as f32 * 0.7152 + pixel.b() as f32 * 0.0722
}

fn lerp(start: f32, end: f32, amount: f32) -> f32 {
    start + (end - start) * amount
}

fn horizontal_fov_for_magnification(magnification: f32) -> f32 {
    let wide_half_angle = (WIDE_HORIZONTAL_FOV_DEG.to_radians() * 0.5).tan();

    2.0 * (wide_half_angle / magnification).atan().to_degrees()
}

fn snap_to_step(value: f32, range: &ControlRange) -> f32 {
    let value = value.clamp(range.min, range.max);
    let step = range.step.abs();

    if step <= f32::EPSILON {
        return value;
    }

    let steps_from_min = ((value - range.min) / step).round();

    (range.min + steps_from_min * step).clamp(range.min, range.max)
}
