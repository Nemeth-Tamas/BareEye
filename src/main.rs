mod camera;

use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    println!("BareEye EagleEye IV hardware probe");
    println!("===================================");

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

    camera::probe_device(eagleeye);

    if std::env::args().any(|argument| argument == "--ptz-test") {
        camera::ptz::run_console(eagleeye)?;
    } else {
        println!();
        println!("Run with --ptz-test to enter the manual PTZ test console.");
    }

    println!();
    println!("BareEye finished.");

    Ok(())
}
