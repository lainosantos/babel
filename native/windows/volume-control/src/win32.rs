use anyhow::{Context, Result, ensure};
use windows::{
    Win32::{
        Media::Audio::{
            DEVICE_STATE_ACTIVE, Endpoints::IAudioEndpointVolume, IMMDevice, IMMDeviceEnumerator,
            IMMEndpoint, MMDeviceEnumerator, eRender,
        },
        System::Com::{
            CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
        },
    },
    core::{GUID, HSTRING, Interface},
};

use super::{VolumeState, Write, apply, validate};

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

fn enumerator() -> Result<IMMDeviceEnumerator> {
    // SAFETY: Apartment is live on this thread; Windows owns the COM allocation.
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
        .context("opening Windows endpoint volume enumeration")
}

fn endpoint(enumerator: &IMMDeviceEnumerator, id: &str) -> Result<IMMDevice> {
    ensure!(
        !id.is_empty() && id.len() <= 4096 && !id.contains('\0'),
        "invalid Windows endpoint ID"
    );
    // SAFETY: the HSTRING remains valid for the call and COM is initialized.
    let device = unsafe { enumerator.GetDevice(&HSTRING::from(id)) }?;
    let endpoint: IMMEndpoint = device.cast()?;
    // SAFETY: live endpoint/device interfaces and out values managed by windows.
    ensure!(
        unsafe { endpoint.GetDataFlow() }? == eRender,
        "Windows endpoint direction changed"
    );
    ensure!(
        unsafe { device.GetState() }? == DEVICE_STATE_ACTIVE,
        "Windows audio endpoint is disconnected or disabled"
    );
    Ok(device)
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

pub fn inspect(playback_id: &str) -> Result<VolumeState> {
    let _apartment = Apartment::enter()?;
    let enumerator = enumerator()?;
    read(&volume(&endpoint(&enumerator, playback_id)?)?)
}

pub fn set_physical(
    playback_id: &str,
    state: VolumeState,
    cancelled: impl Fn() -> bool,
) -> Result<()> {
    validate(state)?;
    let _apartment = Apartment::enter()?;
    let enumerator = enumerator()?;
    let device = endpoint(&enumerator, playback_id)?;
    write(&volume(&device)?, state, &cancelled)
}
