use cameras::Device;
use std::error::Error;
use std::ffi::c_void;
use std::io;
use std::mem::{size_of, size_of_val};
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

const PAN_PROPERTY_ID: u32 = 0;
const TILT_PROPERTY_ID: u32 = 1;
const PANTILT_PROPERTY_ID: u32 = 9;
const PAN_RELATIVE_PROPERTY_ID: u32 = 10;
const TILT_RELATIVE_PROPERTY_ID: u32 = 11;
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

const MEMBER_RANGES: u32 = 1;
const MEMBER_STEPPED_RANGES: u32 = 2;
const MEMBER_VALUES: u32 = 3;

#[repr(C, align(8))]
#[derive(Clone, Copy)]
struct KsPropertyDescriptionRaw {
    access_flags: u32,
    description_size: u32,
    prop_type_set: KsPropertyRaw,
    members_list_count: u32,
    reserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct KsMembersHeaderRaw {
    members_flags: u32,
    members_size: u32,
    members_count: u32,
    flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct KsSteppingLongRaw {
    stepping_delta: u32,
    reserved: u32,
    signed_minimum: i32,
    signed_maximum: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct KsBoundsLongRaw {
    signed_minimum: i32,
    signed_maximum: i32,
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

            let properties = [
                ("PAN", PAN_PROPERTY_ID),
                ("TILT", TILT_PROPERTY_ID),
                ("PANTILT", PANTILT_PROPERTY_ID),
                ("PAN_RELATIVE", PAN_RELATIVE_PROPERTY_ID),
                ("TILT_RELATIVE", TILT_RELATIVE_PROPERTY_ID),
                ("PANTILT_RELATIVE", PANTILT_RELATIVE_PROPERTY_ID),
            ];

            println!();
            println!("Camera-control BasicSupport matrix");
            println!("----------------------------------");

            for (name, property_id) in properties {
                println!();
                println!("{name} (property {property_id})");

                match query_basic_support(&control, property_id) {
                    Ok((access_flags, bytes_returned)) => {
                        println!("  BasicSupport: AVAILABLE");
                        println!("  Bytes returned: {bytes_returned}");
                        println!("  Access mask: 0x{access_flags:08X}");
                        println!("  GET supported: {}", access_flags & PROPERTY_TYPE_GET != 0);
                        println!("  SET supported: {}", access_flags & PROPERTY_TYPE_SET != 0);
                    }
                    Err(error) => {
                        println!("  BasicSupport: NOT AVAILABLE");
                        println!("  Query failed: {error}");
                    }
                }
            }

            println!();
            println!("Relative PTZ speed details");
            println!("--------------------------");

            print_basic_support_details(&control, "PAN_RELATIVE", PAN_RELATIVE_PROPERTY_ID);

            print_basic_support_details(&control, "TILT_RELATIVE", TILT_RELATIVE_PROPERTY_ID);
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

fn print_basic_support_details(control: &IKsControl, name: &str, property_id: u32) {
    let property = KsPropertyRaw {
        set: CAMERA_CONTROL_PROPERTY_SET,
        id: property_id,
        flags: PROPERTY_TYPE_BASICSUPPORT,
    };

    let mut buffer = [0u64; 128];
    let mut bytes_returned = 0u32;

    let result = unsafe {
        control.KsProperty(
            &property as *const KsPropertyRaw as *const KSIDENTIFIER,
            size_of::<KsPropertyRaw>() as u32,
            buffer.as_mut_ptr() as *mut c_void,
            size_of_val(&buffer) as u32,
            &mut bytes_returned,
        )
    };

    println!();
    println!("{name} full BasicSupport:");

    if let Err(error) = result {
        println!("  Query failed: {error}");
        return;
    }

    println!("  Bytes returned: {bytes_returned}");

    let byte_count = bytes_returned as usize;

    if byte_count < size_of::<KsPropertyDescriptionRaw>() {
        println!("  Driver returned no detailed range description.");
        return;
    }

    let bytes = unsafe {
        std::slice::from_raw_parts(
            buffer.as_ptr() as *const u8,
            byte_count.min(size_of_val(&buffer)),
        )
    };

    let Some(description) = read_unaligned::<KsPropertyDescriptionRaw>(bytes, 0) else {
        println!("  Could not decode KSPROPERTY_DESCRIPTION.");
        return;
    };

    println!("  Access mask: 0x{:08X}", description.access_flags);
    println!("  Description size: {}", description.description_size);
    println!("  Members lists: {}", description.members_list_count);

    let mut offset = size_of::<KsPropertyDescriptionRaw>();

    for list_index in 0..description.members_list_count {
        let Some(header) = read_unaligned::<KsMembersHeaderRaw>(bytes, offset) else {
            println!("  Member list {list_index}: truncated header");
            return;
        };

        offset += size_of::<KsMembersHeaderRaw>();

        println!(
            "  Member list {list_index}: type={} size={} count={} flags=0x{:08X}",
            header.members_flags, header.members_size, header.members_count, header.flags
        );

        match header.members_flags {
            MEMBER_STEPPED_RANGES => {
                for member_index in 0..header.members_count {
                    let Some(range) = read_unaligned::<KsSteppingLongRaw>(bytes, offset) else {
                        println!("    Stepped range {member_index}: truncated");
                        return;
                    };

                    println!(
                        "    Stepped range {member_index}: min={} max={} step={}",
                        range.signed_minimum, range.signed_maximum, range.stepping_delta
                    );

                    offset += header.members_size as usize;
                }
            }

            MEMBER_RANGES => {
                for member_index in 0..header.members_count {
                    let member_size = header.members_size as usize;
                    let Some(member_end) = offset.checked_add(member_size) else {
                        println!("    Range {member_index}: invalid size");
                        return;
                    };

                    if member_end > bytes.len() {
                        println!("    Range {member_index}: truncated");
                        return;
                    }

                    let member_bytes = &bytes[offset..member_end];

                    print!("    Raw 32-bit words:");

                    for word in member_bytes.chunks_exact(4) {
                        let value = i32::from_le_bytes([word[0], word[1], word[2], word[3]]);

                        print!(" {value}");
                    }

                    println!();

                    if member_size == size_of::<KsSteppingLongRaw>() {
                        let Some(range) = read_unaligned::<KsSteppingLongRaw>(bytes, offset) else {
                            println!("    16-byte range {member_index}: truncated");
                            return;
                        };

                        println!(
                            "    16-byte range decoded as stepping layout: min={} max={} step={} reserved={}",
                            range.signed_minimum,
                            range.signed_maximum,
                            range.stepping_delta,
                            range.reserved
                        );
                    } else if member_size >= size_of::<KsBoundsLongRaw>() {
                        let Some(range) = read_unaligned::<KsBoundsLongRaw>(bytes, offset) else {
                            println!("    Range {member_index}: truncated");
                            return;
                        };

                        println!(
                            "    Standard range {member_index}: min={} max={}",
                            range.signed_minimum, range.signed_maximum
                        );
                    } else {
                        println!("    Range {member_index}: unexpected member size {member_size}");
                    }

                    offset = member_end;
                }
            }

            MEMBER_VALUES => {
                for member_index in 0..header.members_count {
                    let Some(value) = read_unaligned::<i32>(bytes, offset) else {
                        println!("    Value {member_index}: truncated");
                        return;
                    };

                    println!("    Value {member_index}: {value}");

                    offset += header.members_size as usize;
                }
            }

            other => {
                let member_bytes = header.members_size as usize * header.members_count as usize;

                println!("    Unknown member representation: {other}");

                offset = offset.saturating_add(member_bytes);
            }
        }
    }
}

fn read_unaligned<T: Copy>(bytes: &[u8], offset: usize) -> Option<T> {
    let end = offset.checked_add(size_of::<T>())?;

    if end > bytes.len() {
        return None;
    }

    Some(unsafe { std::ptr::read_unaligned(bytes.as_ptr().add(offset) as *const T) })
}

fn query_basic_support(
    control: &IKsControl,
    property_id: u32,
) -> windows::core::Result<(u32, u32)> {
    debug_assert_eq!(size_of::<KsPropertyRaw>(), 24);

    let property = KsPropertyRaw {
        set: CAMERA_CONTROL_PROPERTY_SET,
        id: property_id,
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
