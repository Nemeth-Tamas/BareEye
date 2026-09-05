use cameras::{ControlCapabilities, ControlRange, Controls, Device};
use std::error::Error;
use std::io::{self, Write};

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

pub fn run_console(device: &Device) -> Result<(), Box<dyn Error>> {
    println!();
    println!("BareEye PTZ test console");
    println!("========================");
    println!();
    print_help();
    print_state(device)?;

    loop {
        print!();
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

    println!("{} set to {}.", axis.label(), value);
    print_state(device)?;

    Ok(())
}

fn center_camera(device: &Device) -> Result<(), cameras::Error> {
    let capabilities = cameras::control_capabilities(device)?;

    let controls = Controls {
        pan: capabilities.pan.map(|range| range.default),
        tilt: capabilities.tilt.map(|range| range.default),
        ..Default::default()
    };

    cameras::apply_controls(device, &controls)?;

    println!("Pan and tilt returned to their default positions.");
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
