use cameras::Device;
use std::error::Error;
use std::ffi::c_void;
use std::io;
use std::mem::size_of;
use windows::Win32::Foundation::{S_FALSE, S_OK};
use windows::Win32::Media::KernelStreaming::{IKsControl, KSIDENTIFIER};
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

const CAMERA_CONTROL_PROPERTY_SET: GUID = GUID::from_u128(0xc6e13370_30ac_11d0_a18c_00a0c9118956);

const PANTILT_RELATIVE_PROPERTY_ID: u32 = 17;

const PROPERTY_TYPE_GET: u32 = 1;
const PROPERTY_TYPE_SET: u32 = 2;
const PROPERTY_TYPE_BASICSUPPORT: u32 = 512;

#[repr(C, align(8))]
#[derive(Clone, Copy)]
struct KsPropertyRaw {
    set: GUID,
    id: u32,
    flags: u32,
}

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
        Ok(control) => {
            println!("IKsControl: AVAILABLE");
            println!("Low-level KS camera control path is accessible.");

            println!();
            println!("KSPROPERTY_CAMERACONTROL_PANTILT_RELATIVE");
            println!("-----------------------------------------");
            println!("Property ID: {PANTILT_RELATIVE_PROPERTY_ID}");

            match query_basic_support(&control) {
                Ok((access_flags, bytes_returned)) => {
                    println!("BasicSupport: AVAILABLE");
                    println!("Bytes returned: {bytes_returned}");
                    println!("Access mask: 0x{access_flags:08X}");
                    println!("GET supported: {}", access_flags & PROPERTY_TYPE_GET != 0);
                    println!("SET supported: {}", access_flags & PROPERTY_TYPE_SET != 0);
                }
                Err(error) => {
                    println!("BasicSupport: NOT AVAILABLE");
                    println!("Query failed: {error}");
                }
            }
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

fn query_basic_support(control: &IKsControl) -> windows::core::Result<(u32, u32)> {
    debug_assert_eq!(size_of::<KsPropertyRaw>(), 24);

    let property = KsPropertyRaw {
        set: CAMERA_CONTROL_PROPERTY_SET,
        id: PANTILT_RELATIVE_PROPERTY_ID,
        flags: PROPERTY_TYPE_BASICSUPPORT,
    };

    let mut access_flags = 0u32;
    let mut bytes_returned = 0u32;

    unsafe {
        control.KsProperty(
            &property as *const KsPropertyRaw as *const KSIDENTIFIER,
            size_of::<KsPropertyRaw>() as u32,
            &mut access_flags as *mut u32 as *mut c_void,
            size_of::<u32>() as u32,
            &mut bytes_returned,
        )?;
    }

    Ok((access_flags, bytes_returned))
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
