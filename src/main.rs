mod app;
mod camera;

use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    let arguments: Vec<String> = std::env::args().collect();
    let debug = arguments.iter().any(|argument| argument == "--debug");

    let devices = cameras::devices()?;

    camera::print_device_list(&devices);

    if devices.is_empty() {
        println!();
        println!("No video capture devices were found.");
        return Ok(());
    }

    println!();
    println!("Searching for an EagleEye candidate...");

    let Some(eagleeye) = camera::find_eagleeye(&devices) else {
        println!("No EagleEye/Polycom camera was detected by name.");
        return Ok(());
    };

    if arguments.iter().any(|argument| argument == "--probe") {
        camera::probe_device(eagleeye);
        return Ok(());
    }

    if arguments.iter().any(|argument| argument == "--ptz-test") {
        camera::probe_device(eagleeye);
        camera::ptz::run_console(eagleeye)?;
        return Ok(());
    }

    let ptz = camera::ptz::ManualController::new(eagleeye.clone())?;
    let (camera, preview_info) = camera::open_preview(eagleeye)?;

    app::run(camera, preview_info, ptz, debug)?;

    Ok(())
}
