use cameras::{Camera, Device, PixelFormat, Resolution, StreamConfig};
use std::error::Error;
use std::io;

pub struct PreviewInfo {
    pub width: u32,
    pub height: u32,
    pub framerate: u32,
    pub pixel_format: String,
}

pub fn open_preview(device: &Device) -> Result<(Camera, PreviewInfo), Box<dyn Error>> {
    let capabilities = cameras::probe(device)?;

    let requested = StreamConfig {
        resolution: Resolution {
            width: 1920,
            height: 1080,
        },
        framerate: 30,
        pixel_format: PixelFormat::Mjpeg,
    };

    let selected = cameras::best_format(&capabilities, &requested)
        .ok_or_else(|| io::Error::other("camera reported no usable video formats"))?;

    let selected_framerate = if (requested.framerate as f64) >= selected.framerate_range.min
        && (requested.framerate as f64) <= selected.framerate_range.max
    {
        requested.framerate
    } else {
        selected.framerate_range.max.round() as u32
    };

    let info = PreviewInfo {
        width: selected.resolution.width,
        height: selected.resolution.height,
        framerate: selected_framerate,
        pixel_format: format!("{:?}", selected.pixel_format),
    };

    let config = StreamConfig {
        resolution: selected.resolution,
        framerate: selected_framerate,
        pixel_format: selected.pixel_format,
    };

    println!();
    println!("Opening BareEye preview");
    println!("-----------------------");
    println!("Requested: 1920x1080 @ 30 FPS, MJPEG");
    println!(
        "Selected:  {}x{} @ {} FPS, {}",
        info.width, info.height, info.framerate, info.pixel_format
    );

    let camera = cameras::open(device, config)?;

    Ok((camera, info))
}
