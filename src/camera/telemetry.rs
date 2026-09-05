use cameras::{Frame, PixelFormat};
use std::time::{Duration, Instant};

const TIMING_GAP_THRESHOLD: Duration = Duration::from_millis(80);

#[derive(Copy, Clone, Default)]
pub struct StreamTelemetrySnapshot {
    pub arrived_frames: u64,
    pub arrival_fps: f32,
    pub dimension_anomalies: u64,
    pub format_anomalies: u64,
    pub timing_anomalies: u64,
}

pub struct StreamTelemetry {
    expected_width: u32,
    expected_height: u32,
    expected_pixel_format: PixelFormat,
    arrived_frames: u64,
    arrival_fps: f32,
    fps_window_frames: u64,
    fps_window_started: Instant,
    dimension_anomalies: u64,
    format_anomalies: u64,
    timing_anomalies: u64,
    last_timestamp: Option<Duration>,
}

impl StreamTelemetry {
    pub fn new(
        expected_width: u32,
        expected_height: u32,
        expected_pixel_format: PixelFormat,
    ) -> Self {
        Self {
            expected_width,
            expected_height,
            expected_pixel_format,
            arrived_frames: 0,
            arrival_fps: 0.0,
            fps_window_frames: 0,
            fps_window_started: Instant::now(),
            dimension_anomalies: 0,
            format_anomalies: 0,
            timing_anomalies: 0,
            last_timestamp: None,
        }
    }

    pub fn observe(&mut self, frame: &Frame) {
        self.arrived_frames += 1;
        self.fps_window_frames += 1;

        if frame.width != self.expected_width || frame.height != self.expected_height {
            self.dimension_anomalies += 1;
        }

        if frame.pixel_format != self.expected_pixel_format {
            self.format_anomalies += 1;
        }

        if let Some(previous_timestamp) = self.last_timestamp {
            if frame.timestamp < previous_timestamp
                || frame.timestamp.saturating_sub(previous_timestamp) > TIMING_GAP_THRESHOLD
            {
                self.timing_anomalies += 1;
            }
        }

        self.last_timestamp = Some(frame.timestamp);

        let elapsed = self.fps_window_started.elapsed();

        if elapsed.as_secs_f32() >= 1.0 {
            self.arrival_fps = self.fps_window_frames as f32 / elapsed.as_secs_f32();

            self.fps_window_frames = 0;
            self.fps_window_started = Instant::now();
        }
    }

    pub fn snapshot(&self) -> StreamTelemetrySnapshot {
        StreamTelemetrySnapshot {
            arrived_frames: self.arrived_frames,
            arrival_fps: self.arrival_fps,
            dimension_anomalies: self.dimension_anomalies,
            format_anomalies: self.format_anomalies,
            timing_anomalies: self.timing_anomalies,
        }
    }
}
