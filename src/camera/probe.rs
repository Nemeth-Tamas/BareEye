use cameras::{ControlRange, Device};

pub fn print_device_list(devices: &[Device]) {
    println!();
    println!("Video devices found: {}", devices.len());

    for (index, device) in devices.iter().enumerate() {
        println!();
        println!("[{index}] {}", device.name);
        println!("    Device ID: {}", device.id.0);
    }
}

pub fn find_eagleeye(devices: &[Device]) -> Option<&Device> {
    devices.iter().find(|device| is_eagleeye_candidate(device))
}

pub fn probe_device(device: &Device) {
    println!();
    println!("Selected camera:");
    println!("    Name: {}", device.name);
    println!("    ID:   {}", device.id.0);

    probe_video_formats(device);
    probe_camera_controls(device);
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
            println!(
                "Reported resolution/framerate/pixel-format tuples: {}",
                capabilities.formats.len()
            );
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
