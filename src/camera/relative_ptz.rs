use cameras::Device;
use std::error::Error;
use std::io;
use windows::Win32::Foundation::{S_FALSE, S_OK};
use windows::Win32::Media::KernelStreaming::IKsControl;
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFMediaSource, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK, MF_VERSION, MFCreateAttributes,
    MFEnumDeviceSources, MFSTARTUP_FULL, MFShutdown, MFStartup,
};
use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::core::{GUID, Interface};

pub fn probe(device: &Device) -> Result<(), Box<dyn Error>> {
    let _com = ComGuard::init()?;
    let _mf = MfGuard::init()?;

    println!();
    println!("BareEye relative PTZ probe");
    println!("==========================");
    println!("Device: {}", device.name);
    println!("ID: {}", device.id.0);

    let activations = enumerate_activations()?;

    let mut matching_activation = None;

    for activation in activations {
        let symbolic_link = match read_string(
            &activation,
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
        ) {
            Ok(value) => value,
            Err(_) => continue,
        };

        if symbolic_link == device.id.0 {
            matching_activation = Some(activation);
            break;
        }
    }

    let activation = matching_activation.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "Could not find the EagleEye Media Foundation activation object",
        )
    })?;

    let source: IMFMediaSource = unsafe { activation.ActivateObject()? };

    println!("Media Foundation source opened.");

    match source.cast::<IKsControl>() {
        Ok(_) => {
            println!("IKsControl: AVAILABLE");
            println!("Low-level KS camera control path is accessible.");
        }
        Err(error) => {
            println!("IKsControl: NOT AVAILABLE");
            println!("QueryInterface failed: {error}");
        }
    }

    unsafe {
        let _ = source.Shutdown();
    }

    Ok(())
}

fn enumerate_activations() -> windows::core::Result<Vec<IMFActivate>> {
    unsafe {
        let mut attributes = None;
        MFCreateAttributes(&mut attributes, 1)?;

        let attributes = attributes.ok_or_else(|| {
            windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL)
        })?;

        attributes.SetGUID(
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
        )?;

        let mut raw_devices: *mut Option<IMFActivate> = std::ptr::null_mut();

        let mut count = 0u32;

        MFEnumDeviceSources(&attributes, &mut raw_devices, &mut count)?;

        let mut activations = Vec::with_capacity(count as usize);

        for index in 0..count as isize {
            let slot = raw_devices.offset(index);
            let activation = std::ptr::read(slot);

            if let Some(activation) = activation {
                activations.push(activation);
            }
        }

        CoTaskMemFree(Some(raw_devices as *const _));

        Ok(activations)
    }
}

fn read_string(activation: &IMFActivate, key: &GUID) -> windows::core::Result<String> {
    unsafe {
        let length = activation.GetStringLength(key)?;

        let mut buffer = vec![0u16; (length + 1) as usize];

        let mut written = 0u32;

        activation.GetString(key, &mut buffer, Some(&mut written))?;

        Ok(String::from_utf16_lossy(&buffer[..written as usize]))
    }
}

struct ComGuard {
    initialized: bool,
}

impl ComGuard {
    fn init() -> windows::core::Result<Self> {
        let result =
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };

        if result == S_OK || result == S_FALSE {
            Ok(Self { initialized: true })
        } else {
            result.ok()?;

            Ok(Self { initialized: false })
        }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.initialized {
            unsafe {
                CoUninitialize();
            }
        }
    }
}

struct MfGuard;

impl MfGuard {
    fn init() -> windows::core::Result<Self> {
        unsafe {
            MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
        }

        Ok(Self)
    }
}

impl Drop for MfGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = MFShutdown();
        }
    }
}
