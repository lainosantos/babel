use anyhow::{Context, Result, ensure};
use windows::{
    Win32::{
        Foundation::PROPERTYKEY,
        Media::Audio::{
            DEVICE_STATE_ACTIVE, EDataFlow, Endpoints::IAudioEndpointVolume, IMMDevice,
            IMMDeviceEnumerator, IMMEndpoint, MMDeviceEnumerator, eCapture, eRender,
        },
        System::Com::{
            CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
            STGM_READ,
            StructuredStorage::{PROPVARIANT, PropVariantClear, PropVariantToString},
        },
    },
    core::{GUID, HSTRING, Interface},
};

use super::{
    DRIVER, SPEAKER_CAPTURE, SPEAKER_RENDER, Snapshot, VolumeState, Write, apply, compatible,
    validate,
};

const PROPERTY_SET: GUID = GUID::from_u128(0x76ad3c42_65b7_4da0_a823_907a8e5ac6c1);
const ROLE: PROPERTYKEY = PROPERTYKEY {
    fmtid: PROPERTY_SET,
    pid: 1,
};
const DRIVER_ID: PROPERTYKEY = PROPERTYKEY {
    fmtid: PROPERTY_SET,
    pid: 2,
};
const VOLUME_CONTROL: PROPERTYKEY = PROPERTYKEY {
    fmtid: PROPERTY_SET,
    pid: 3,
};
const EVENT_CONTEXT: GUID = GUID::from_u128(0x6a83ef44_dc2b_45c0_bceb_8d7a178d245c);

struct Apartment;
impl Apartment {
    fn enter() -> Result<Self> {
        // SAFETY: the caller stays on this OS thread for the entire synchronous
        // operation; every successful initialization is balanced by Drop.
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok() }
            .context("initializing Windows volume control")?;
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: all operation-local COM interfaces have already been dropped.
        unsafe { CoUninitialize() };
    }
}

struct Property(PROPVARIANT);
impl Drop for Property {
    fn drop(&mut self) {
        // SAFETY: GetValue initialized this owned PROPVARIANT exactly once.
        let _ = unsafe { PropVariantClear(&mut self.0) };
    }
}

fn enumerator() -> Result<IMMDeviceEnumerator> {
    // SAFETY: Apartment is live on this thread; Windows owns the COM allocation.
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
        .context("opening Windows endpoint volume enumeration")
}

fn property(device: &IMMDevice, key: &PROPERTYKEY) -> Result<String> {
    // SAFETY: device/key remain live, and the returned store/variant stay local.
    let store = unsafe { device.OpenPropertyStore(STGM_READ) }?;
    let value = Property(unsafe { store.GetValue(key) }?);
    let mut buffer = [0_u16; 4096];
    // SAFETY: the slice is bounded and valid for the complete call.
    unsafe { PropVariantToString(&value.0, &mut buffer) }?;
    let length = buffer
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(buffer.len());
    ensure!(
        length < buffer.len(),
        "Windows endpoint property is too long"
    );
    Ok(String::from_utf16(&buffer[..length])?)
}

fn supports_bridge(device: &IMMDevice, role: &str) -> bool {
    compatible(
        &property(device, &DRIVER_ID).unwrap_or_default(),
        &property(device, &ROLE).unwrap_or_default(),
        &property(device, &VOLUME_CONTROL).unwrap_or_default(),
        role,
    )
}

fn endpoint(enumerator: &IMMDeviceEnumerator, id: &str, direction: EDataFlow) -> Result<IMMDevice> {
    ensure!(
        !id.is_empty() && id.len() <= 4096 && !id.contains('\0'),
        "invalid Windows endpoint ID"
    );
    // SAFETY: the HSTRING remains valid for the call and COM is initialized.
    let device = unsafe { enumerator.GetDevice(&HSTRING::from(id)) }?;
    let endpoint: IMMEndpoint = device.cast()?;
    // SAFETY: live endpoint/device interfaces and out values managed by windows.
    ensure!(
        unsafe { endpoint.GetDataFlow() }? == direction,
        "Windows endpoint direction changed"
    );
    ensure!(
        unsafe { device.GetState() }? == DEVICE_STATE_ACTIVE,
        "Windows audio endpoint is disconnected or disabled"
    );
    Ok(device)
}

fn virtual_render(enumerator: &IMMDeviceEnumerator, capture_id: &str) -> Result<Option<IMMDevice>> {
    let capture = endpoint(enumerator, capture_id, eCapture)?;
    if !supports_bridge(&capture, SPEAKER_CAPTURE) {
        return Ok(None);
    }
    // SAFETY: enumeration and returned interfaces stay within this apartment.
    let devices = unsafe { enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) }?;
    let count = unsafe { devices.GetCount() }?;
    ensure!(
        count <= 256,
        "too many Windows render endpoints for volume inspection"
    );
    let mut selected = None;
    for index in 0..count {
        let device = unsafe { devices.Item(index) }?;
        if supports_bridge(&device, SPEAKER_RENDER) {
            ensure!(
                selected.is_none(),
                "multiple Babel speaker pairs make volume ownership ambiguous"
            );
            selected = Some(device);
        }
    }
    Ok(selected)
}

fn volume(device: &IMMDevice) -> Result<IAudioEndpointVolume> {
    // SAFETY: Windows validates interface support and returns an owned interface.
    let volume: IAudioEndpointVolume = unsafe { device.Activate(CLSCTX_ALL, None) }
        .context("opening Windows endpoint master-volume control")?;
    let (mut minimum, mut maximum, mut increment) = (0.0_f32, 0.0_f32, 0.0_f32);
    // The interface has no read-only CanSet query. Require a valid control range;
    // a later denied/disconnected write is still reported to the controller.
    unsafe { volume.GetVolumeRange(&mut minimum, &mut maximum, &mut increment) }?;
    ensure!(
        minimum.is_finite()
            && maximum.is_finite()
            && minimum < maximum
            && increment.is_finite()
            && increment > 0.0,
        "Windows endpoint has no writable master-volume range"
    );
    unsafe { volume.QueryHardwareSupport() }?;
    Ok(volume)
}

fn read(volume: &IAudioEndpointVolume) -> Result<VolumeState> {
    // SAFETY: scalar and BOOL are returned by the live COM interface.
    let state = VolumeState {
        level: unsafe { volume.GetMasterVolumeLevelScalar() }?,
        muted: unsafe { volume.GetMute() }?.as_bool(),
    };
    validate(state)?;
    Ok(state)
}

fn write(
    volume: &IAudioEndpointVolume,
    state: VolumeState,
    cancelled: &impl Fn() -> bool,
) -> Result<()> {
    // Set only the master: Windows preserves channel balance. Mute before any
    // level increase when requested, and unmute only after the new level exists.
    // SAFETY: finite normalized level, live interface and stable event GUID.
    apply(state, cancelled, |next| {
        unsafe {
            match next {
                Write::Mute(muted) => volume.SetMute(muted, &EVENT_CONTEXT)?,
                Write::Level(level) => volume.SetMasterVolumeLevelScalar(level, &EVENT_CONTEXT)?,
            }
        }
        Ok(())
    })
}

pub fn inspect(capture_id: &str, playback_id: &str) -> Result<Snapshot> {
    let _apartment = Apartment::enter()?;
    let enumerator = enumerator()?;
    let physical = endpoint(&enumerator, playback_id, eRender)?;
    ensure!(
        property(&physical, &DRIVER_ID).unwrap_or_default() != DRIVER,
        "select a physical speaker for volume synchronization"
    );
    let physical_state = read(&volume(&physical)?)?;
    let (virtual_device, limitation) = match virtual_render(&enumerator, capture_id) {
        Ok(Some(device)) => (Some(device), None),
        Ok(None) => (None, Some("Volume synchronization requires the updated Babel Windows driver with control-only volume support; use the physical output volume for this device".into())),
        Err(error) => (None, Some(format!("Virtual speaker volume is unavailable; the physical output remains adjustable: {error:#}"))),
    };
    let Some(virtual_device) = virtual_device else {
        return Ok(Snapshot {
            virtual_state: physical_state,
            physical_state,
            synchronized: false,
            limitation,
        });
    };
    match volume(&virtual_device).and_then(|volume| read(&volume)) {
        Ok(virtual_state) => Ok(Snapshot {
            virtual_state,
            physical_state,
            synchronized: true,
            limitation: None,
        }),
        Err(error) => Ok(Snapshot {
            virtual_state: physical_state,
            physical_state,
            synchronized: false,
            limitation: Some(format!(
                "Virtual speaker volume is unavailable; the physical output remains adjustable: {error:#}"
            )),
        }),
    }
}

pub fn set_virtual(
    capture_id: &str,
    state: VolumeState,
    cancelled: impl Fn() -> bool,
) -> Result<()> {
    validate(state)?;
    let _apartment = Apartment::enter()?;
    let enumerator = enumerator()?;
    let device = virtual_render(&enumerator, capture_id)?
        .context("the selected virtual speaker lacks verified control-only volume support")?;
    write(&volume(&device)?, state, &cancelled)
}

pub fn set_physical(
    playback_id: &str,
    state: VolumeState,
    cancelled: impl Fn() -> bool,
) -> Result<()> {
    validate(state)?;
    let _apartment = Apartment::enter()?;
    let enumerator = enumerator()?;
    let device = endpoint(&enumerator, playback_id, eRender)?;
    ensure!(
        property(&device, &DRIVER_ID).unwrap_or_default() != DRIVER,
        "volume target is a Babel virtual endpoint"
    );
    write(&volume(&device)?, state, &cancelled)
}
