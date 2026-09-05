use cameras::{ControlRange, Device};

fn main() -> Result<(), cameras::Error> {
    println!("BareEye EagleEye IV hardware probe");
    println!("===================================");

    let devices = cameras::devices()?;

    println!();
    println!("Video devices found: {}", devices.len());

    if devices.is_empty() {
        println!("No video capture devices were found.");
        return Ok(());
    }

    for (index, device) in devices.iter().enumerate() {
        println!();
        println!("[{index}] {}", device.name);
        println!("    Device ID: {}", device.id.0);
    }

    println!();
    println!("Searching for an EagleEye candidate...");

    let Some(eagleeye) = devices.iter().find(|device| is_eagleeye_candidate(device)) else {
        println!("No EagleEye/Polycom camera was detected by name.");
        println!("The complete device list is shown above.");
        return Ok(());
    };

    println!();
    println!("Selected camera:");
    println!("    Name: {}", eagleeye.name);
    println!("    ID:   {}", eagleeye.id.0);

    probe_video_formats(eagleeye);
    probe_camera_controls(eagleeye);

    println!();
    println!("Hardware probe complete.");

    Ok(())
}

fn is_eagleeye_candidate(device: &Device) -> bool {
    let name = device.name.to_ascii_lowercase();

    name.contains("eagleeye")
        || name.contains("eagle eye")
        || name.contains("polycom")
        || name.contains("poly ")
}

fn probe_video_formats(device: &Device) {
    println!();
    println!("Video format probe");
    println!("------------------");

    match cameras::probe(device) {
        Ok(capabilities) => {
            println!("Reported formats: {}", capabilities.formats.len());

            for (index, format) in capabilities.formats.iter().enumerate() {
                println!();
                println!("Format {index}:");
                println!("{format:#?}");
            }
        }
        Err(error) => {
            println!("Video format probe failed:");
            println!("    {error}");
        }
    }
}

fn probe_camera_controls(device: &Device) {
    println!();
    println!("Camera control capability probe");
    println!("-------------------------------");

    match cameras::control_capabilities(device) {
        Ok(capabilities) => {
            print_range("Pan", capabilities.pan);
            print_range("Tilt", capabilities.tilt);
            print_range("Zoom", capabilities.zoom);
            print_range("Focus", capabilities.focus);

            println!();
            println!("Autofocus capability: {:?}", capabilities.auto_focus);
        }
        Err(error) => {
            println!("Camera control capability probe failed:");
            println!("    {error}");
        }
    }

    println!();
    println!("Current PTZ state");
    println!("-----------------");

    match cameras::read_controls(device) {
        Ok(controls) => {
            println!("Pan:   {:?}", controls.pan);
            println!("Tilt:  {:?}", controls.tilt);
            println!("Zoom:  {:?}", controls.zoom);
            println!("Focus: {:?}", controls.focus);
            println!("AF:    {:?}", controls.auto_focus);
        }
        Err(error) => {
            println!("Current camera-control read failed:");
            println!("    {error}");
        }
    }
}

fn print_range(name: &str, range: Option<ControlRange>) {
    match range {
        Some(range) => {
            println!();
            println!("{name}: SUPPORTED");
            println!("    Minimum: {}", range.min);
            println!("    Maximum: {}", range.max);
            println!("    Step:    {}", range.step);
            println!("    Default: {}", range.default);
        }
        None => {
            println!();
            println!("{name}: NOT REPORTED");
        }
    }
}
