//! Direct CoreAudio HAL property queries.
//!
//! cpal (0.15 and 0.16) instantiates an AUHAL for every config query — an
//! input-enabled one even for output devices in 0.15 — which makes device
//! listing trip the macOS microphone permission check (and block indefinitely
//! while its prompt is up). Plain HAL property reads never touch the mic, so
//! listing, lookup and config selection use these instead; cpal is only used
//! to run the streams.

use std::ffi::{CStr, c_char, c_void};
use std::mem::{self, MaybeUninit};
use std::ptr;

use coreaudio_sys::{
    AudioBuffer, AudioBufferList, AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize,
    AudioObjectID, AudioObjectPropertyAddress, AudioObjectPropertySelector, AudioValueRange,
    CFRelease, CFStringGetCString, CFStringRef, kAudioDevicePropertyAvailableNominalSampleRates,
    kAudioDevicePropertyBufferFrameSizeRange, kAudioDevicePropertyDeviceNameCFString,
    kAudioDevicePropertyNominalSampleRate, kAudioDevicePropertyStreamConfiguration,
    kAudioDevicePropertyTransportType, kAudioDeviceTransportTypeBuiltIn,
    kAudioDeviceTransportTypeVirtual, kAudioHardwarePropertyDefaultInputDevice,
    kAudioHardwarePropertyDefaultOutputDevice, kAudioHardwarePropertyDevices,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal,
    kAudioObjectPropertyScopeInput, kAudioObjectPropertyScopeOutput, kAudioObjectSystemObject,
    kCFStringEncodingUTF8,
};

use crate::MicPermission;
use crate::device::{DeviceCaps, Direction};

fn address(
    selector: AudioObjectPropertySelector,
    dir: Option<Direction>,
) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: match dir {
            None => kAudioObjectPropertyScopeGlobal,
            Some(Direction::Input) => kAudioObjectPropertyScopeInput,
            Some(Direction::Output) => kAudioObjectPropertyScopeOutput,
        },
        mElement: kAudioObjectPropertyElementMain,
    }
}

/// Read a fixed-size property. `T` must be the property's plain-data type.
fn get<T: Copy>(object: AudioObjectID, addr: &AudioObjectPropertyAddress) -> Option<T> {
    let mut value = MaybeUninit::<T>::uninit();
    let mut size = mem::size_of::<T>() as u32;
    // SAFETY: the HAL writes at most `size` bytes into `value`.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            addr,
            0,
            ptr::null(),
            &mut size,
            value.as_mut_ptr().cast(),
        )
    };
    // SAFETY: success with the full size means `value` was initialized.
    (status == 0 && size as usize == mem::size_of::<T>()).then(|| unsafe { value.assume_init() })
}

/// Read a variable-length property into an 8-byte-aligned buffer (so the
/// caller can reinterpret it as the property's struct type), plus its exact
/// length in bytes.
fn get_bytes(
    object: AudioObjectID,
    addr: &AudioObjectPropertyAddress,
) -> Option<(Vec<u64>, usize)> {
    let mut size = 0u32;
    // SAFETY: plain size query.
    let status = unsafe { AudioObjectGetPropertyDataSize(object, addr, 0, ptr::null(), &mut size) };
    if status != 0 {
        return None;
    }
    let mut buf = vec![0u64; (size as usize).div_ceil(8)];
    // SAFETY: `buf` holds at least `size` bytes.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            addr,
            0,
            ptr::null(),
            &mut size,
            buf.as_mut_ptr().cast(),
        )
    };
    let len = (size as usize).min(buf.len() * mem::size_of::<u64>());
    (status == 0).then_some((buf, len))
}

fn get_array<T: Copy>(object: AudioObjectID, addr: &AudioObjectPropertyAddress) -> Vec<T> {
    let Some((bytes, len)) = get_bytes(object, addr) else {
        return Vec::new();
    };
    let n = len / mem::size_of::<T>();
    // SAFETY: the buffer is 8-byte aligned (≥ align of the HAL's element types)
    // and holds `n` whole elements written by the HAL.
    unsafe { std::slice::from_raw_parts(bytes.as_ptr().cast::<T>(), n) }.to_vec()
}

/// Every audio device, in HAL order — the same list, in the same order, that
/// cpal's `Host::devices()` enumerates.
pub(crate) fn device_ids() -> Vec<AudioObjectID> {
    get_array(
        kAudioObjectSystemObject,
        &address(kAudioHardwarePropertyDevices, None),
    )
}

fn name(id: AudioObjectID) -> Option<String> {
    let cf: CFStringRef = get(id, &address(kAudioDevicePropertyDeviceNameCFString, None))?;
    if cf.is_null() {
        return None;
    }
    let mut buf = [0 as c_char; 512];
    // SAFETY: `cf` is a valid CFString we own (Copy rule); `buf` is sized as passed.
    let ok =
        unsafe { CFStringGetCString(cf, buf.as_mut_ptr(), buf.len() as _, kCFStringEncodingUTF8) };
    // SAFETY: the property getter hands us a +1 reference.
    unsafe { CFRelease(cf as *const c_void) };
    // SAFETY: on success the buffer is NUL-terminated.
    (ok != 0).then(|| {
        unsafe { CStr::from_ptr(buf.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    })
}

fn channels(id: AudioObjectID, dir: Direction) -> u16 {
    let Some((bytes, len)) = get_bytes(
        id,
        &address(kAudioDevicePropertyStreamConfiguration, Some(dir)),
    ) else {
        return 0;
    };
    if len < mem::size_of::<u32>() {
        return 0;
    }
    let list = bytes.as_ptr().cast::<AudioBufferList>();
    // SAFETY: the HAL filled an AudioBufferList with `mNumberBuffers` trailing
    // AudioBuffers; bounds are re-checked against the byte length.
    unsafe {
        let n = (*list).mNumberBuffers as usize;
        let first = ptr::addr_of!((*list).mBuffers).cast::<AudioBuffer>();
        let header = first as usize - list as usize;
        let n = n.min(len.saturating_sub(header) / mem::size_of::<AudioBuffer>());
        (0..n)
            .map(|i| (*first.add(i)).mNumberChannels)
            .sum::<u32>()
            .min(u32::from(u16::MAX)) as u16
    }
}

fn default_id(dir: Direction) -> Option<AudioObjectID> {
    let selector = match dir {
        Direction::Input => kAudioHardwarePropertyDefaultInputDevice,
        Direction::Output => kAudioHardwarePropertyDefaultOutputDevice,
    };
    get(kAudioObjectSystemObject, &address(selector, None)).filter(|&id: &AudioObjectID| id != 0)
}

fn describe(
    id: AudioObjectID,
    dir: Direction,
    default: Option<AudioObjectID>,
) -> Option<DeviceCaps> {
    let channels = channels(id, dir);
    if channels == 0 {
        return None;
    }
    let nominal: f64 = get(id, &address(kAudioDevicePropertyNominalSampleRate, None))?;
    let rate_ranges = get_array::<AudioValueRange>(
        id,
        &address(kAudioDevicePropertyAvailableNominalSampleRates, None),
    )
    .into_iter()
    .map(|r| (r.mMinimum.round() as u32, r.mMaximum.round() as u32))
    .collect();
    let buffer_range = get::<AudioValueRange>(
        id,
        &address(kAudioDevicePropertyBufferFrameSizeRange, Some(dir)),
    )
    .map(|r| (r.mMinimum as u32, r.mMaximum as u32));
    let transport: Option<u32> = get(id, &address(kAudioDevicePropertyTransportType, None));
    Some(DeviceCaps {
        id,
        name: name(id)?,
        channels,
        default_rate: nominal.round() as u32,
        rate_ranges,
        buffer_range,
        is_default: default == Some(id),
        built_in: transport == Some(kAudioDeviceTransportTypeBuiltIn),
        virtual_transport: transport == Some(kAudioDeviceTransportTypeVirtual),
    })
}

/// All devices with at least one channel in `dir`, in HAL order.
pub(crate) fn devices(dir: Direction) -> Vec<DeviceCaps> {
    let default = default_id(dir);
    device_ids()
        .into_iter()
        .filter_map(|id| describe(id, dir, default))
        .collect()
}

#[link(name = "AVFoundation", kind = "framework")]
unsafe extern "C" {
    static AVMediaTypeAudio: *const c_void;
}

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const c_char) -> *const c_void;
    fn sel_registerName(name: *const c_char) -> *const c_void;
    fn objc_msgSend();
}

/// `[AVCaptureDevice authorizationStatusForMediaType:AVMediaTypeAudio]`. A pure
/// TCC preflight: never prompts, never blocks.
pub(crate) fn mic_permission() -> MicPermission {
    // SAFETY: standard objc runtime calls; objc_msgSend is cast to the exact
    // signature of `+authorizationStatusForMediaType:` (id, SEL, id) -> NSInteger,
    // as arm64 requires.
    let status = unsafe {
        let class = objc_getClass(c"AVCaptureDevice".as_ptr());
        if class.is_null() {
            return MicPermission::NotDetermined;
        }
        let sel = sel_registerName(c"authorizationStatusForMediaType:".as_ptr());
        let send: unsafe extern "C" fn(*const c_void, *const c_void, *const c_void) -> isize =
            mem::transmute(objc_msgSend as unsafe extern "C" fn());
        send(class, sel, AVMediaTypeAudio)
    };
    match status {
        1 => MicPermission::Restricted,
        2 => MicPermission::Denied,
        3 => MicPermission::Authorized,
        _ => MicPermission::NotDetermined,
    }
}
