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

pub struct ManualController {
    device: Device,
    capabilities: ControlCapabilities,
    pan_target: f32,
    tilt_target: f32,
    zoom_target: f32,
    actual_pan: Option<f32>,
    actual_tilt: Option<f32>,
    actual_zoom: Option<f32>,
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

        Ok(Self {
            device,
            capabilities,
            pan_target,
            tilt_target,
            zoom_target,
            actual_pan: controls.pan,
            actual_tilt: controls.tilt,
            actual_zoom: controls.zoom,
        })
    }

    pub fn pan_target(&self) -> f32 {
        self.pan_target
    }

    pub fn tilt_target(&self) -> f32 {
        self.tilt_target
    }

    pub fn zoom_target(&self) -> f32 {
        self.zoom_target
    }

    pub fn actual_pan(&self) -> Option<f32> {
        self.actual_pan
    }

    pub fn actual_tilt(&self) -> Option<f32> {
        self.actual_tilt
    }

    pub fn actual_zoom(&self) -> Option<f32> {
        self.actual_zoom
    }

    pub fn refresh_actual(&mut self) -> Result<(), cameras::Error> {
        let controls = cameras::read_controls(&self.device)?;

        self.actual_pan = controls.pan;
        self.actual_tilt = controls.tilt;
        self.actual_zoom = controls.zoom;

        Ok(())
    }

    pub fn zoom_range(&self) -> Option<ControlRange> {
        self.capabilities.zoom
    }

    pub fn pan_by(&mut self, amount: f32) -> Result<(), cameras::Error> {
        self.set_axis_target(Axis::Pan, self.pan_target + amount)
    }

    pub fn tilt_by(&mut self, amount: f32) -> Result<(), cameras::Error> {
        self.set_axis_target(Axis::Tilt, self.tilt_target + amount)
    }

    pub fn zoom_by(&mut self, amount: f32) -> Result<(), cameras::Error> {
        self.set_axis_target(Axis::Zoom, self.zoom_target + amount)
    }

    pub fn set_zoom(&mut self, value: f32) -> Result<(), cameras::Error> {
        self.set_axis_target(Axis::Zoom, value)
    }

    pub fn center(&mut self) -> Result<(), cameras::Error> {
        let controls = Controls {
            pan: self.capabilities.pan.map(|range| range.default),
            tilt: self.capabilities.tilt.map(|range| range.default),
            ..Default::default()
        };

        cameras::apply_controls(&self.device, &controls)?;

        if let Some(range) = self.capabilities.pan {
            self.pan_target = range.default;
        }

        if let Some(range) = self.capabilities.tilt {
            self.tilt_target = range.default;
        }

        Ok(())
    }

    pub fn wide(&mut self) -> Result<(), cameras::Error> {
        let Some(range) = self.capabilities.zoom else {
            return Ok(());
        };

        self.set_axis_target(Axis::Zoom, range.default)
    }

    fn set_axis_target(&mut self, axis: Axis, value: f32) -> Result<(), cameras::Error> {
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

        cameras::apply_controls(&self.device, &controls)?;

        match axis {
            Axis::Pan => self.pan_target = value,
            Axis::Tilt => self.tilt_target = value,
            Axis::Zoom => self.zoom_target = value,
        }

        Ok(())
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
