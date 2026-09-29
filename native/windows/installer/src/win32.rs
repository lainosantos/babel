//! Auditable SetupAPI boundary. All returned buffers are owned/bounded; handles
//! have RAII lifetimes. No pointers escape, device defaults or shell are used.
use super::{Action, HARDWARE_ID, is_own_hardware, oem_inf};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    ffi::OsStr,
    mem::size_of,
    os::windows::ffi::OsStrExt,
    path::Path,
    ptr::{null, null_mut},
};
use windows_sys::{
    Win32::{
        Devices::{DeviceAndDriverInstallation::*, Properties::*},
        Foundation::*,
        Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation},
        System::Threading::{CreateMutexW, GetCurrentProcess, OpenProcessToken},
    },
    core::GUID,
};

const MEDIA: GUID = GUID::from_u128(0x4d36e96c_e325_11ce_bfc1_08002be10318);
const MAX_WORDS: usize = 16_384;

fn wide(value: impl AsRef<OsStr>) -> Result<Vec<u16>> {
    let mut result: Vec<u16> = value.as_ref().encode_wide().collect();
    ensure!(
        !result.contains(&0) && result.len() < MAX_WORDS,
        "Invalid Windows path or identity"
    );
    result.push(0);
    Ok(result)
}
fn check(ok: i32, operation: &str) -> Result<()> {
    if ok == 0 {
        return Err(std::io::Error::last_os_error()).context(operation.to_owned());
    }
    Ok(())
}
struct DeviceSet(HDEVINFO);
impl Drop for DeviceSet {
    fn drop(&mut self) {
        // SAFETY: unique, live SetupAPI handle owned by this object.
        unsafe {
            SetupDiDestroyDeviceInfoList(self.0);
        }
    }
}
struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: unique live handle returned by CreateMutex/OpenProcessToken.
        unsafe {
            CloseHandle(self.0);
        }
    }
}
fn data() -> SP_DEVINFO_DATA {
    SP_DEVINFO_DATA {
        cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
        ..Default::default()
    }
}
fn same_media(guid: GUID) -> bool {
    guid.data1 == MEDIA.data1
        && guid.data2 == MEDIA.data2
        && guid.data3 == MEDIA.data3
        && guid.data4 == MEDIA.data4
}

fn elevated_and_locked() -> Result<Handle> {
    let mut token = null_mut();
    // SAFETY: current process pseudo-handle is valid; output points to a HANDLE.
    unsafe {
        check(
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token),
            "Read process token",
        )?;
    }
    let token = Handle(token);
    let mut elevation = TOKEN_ELEVATION::default();
    let mut size = 0;
    // SAFETY: TOKEN_ELEVATION matches the requested class and buffer length.
    unsafe {
        check(
            GetTokenInformation(
                token.0,
                TokenElevation,
                (&mut elevation as *mut TOKEN_ELEVATION).cast(),
                size_of::<TOKEN_ELEVATION>() as u32,
                &mut size,
            ),
            "Read elevation status",
        )?;
    }
    ensure!(
        elevation.TokenIsElevated != 0,
        "Run this command from an Administrator terminal; Babel does not elevate automatically"
    );
    let name = wide("Global\\BabelAudioInstallerV1")?;
    // SAFETY: default security descriptor, immutable nul-terminated name.
    let handle = unsafe { CreateMutexW(null(), 1, name.as_ptr()) };
    ensure!(
        !handle.is_null(),
        "Could not acquire installer mutex: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: read immediately after CreateMutex, before other Windows calls.
    let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    let handle = Handle(handle);
    ensure!(!existed, "Another Babel driver installer is running");
    Ok(handle)
}

fn hardware(set: &DeviceSet, device: &SP_DEVINFO_DATA) -> Result<Vec<String>> {
    let mut words = vec![0_u16; MAX_WORDS];
    let (mut kind, mut required) = (0, 0);
    // SAFETY: live set and member data; buffer is aligned and its byte length exact.
    let ok = unsafe {
        SetupDiGetDeviceRegistryPropertyW(
            set.0,
            device,
            SPDRP_HARDWAREID,
            &mut kind,
            words.as_mut_ptr().cast(),
            (words.len() * 2) as u32,
            &mut required,
        )
    };
    if ok == 0 {
        // SAFETY: last error from the failed query above.
        if unsafe { GetLastError() } == ERROR_INVALID_DATA {
            return Ok(Vec::new());
        }
        check(ok, "Read root device hardware IDs")?;
    }
    ensure!(
        kind == 7 && required as usize <= words.len() * 2 && required % 2 == 0,
        "Invalid hardware ID property type/length"
    ); // REG_MULTI_SZ
    Ok(words[..required as usize / 2]
        .split(|v| *v == 0)
        .filter(|part| !part.is_empty())
        .map(String::from_utf16_lossy)
        .collect())
}
fn property(set: &DeviceSet, device: &SP_DEVINFO_DATA, key: &DEVPROPKEY) -> Result<String> {
    let mut words = vec![0_u16; MAX_WORDS];
    let (mut kind, mut required) = (0, 0);
    // SAFETY: output is owned, aligned, bounded and not shared across API calls.
    unsafe {
        check(
            SetupDiGetDevicePropertyW(
                set.0,
                device,
                key,
                &mut kind,
                words.as_mut_ptr().cast(),
                (words.len() * 2) as u32,
                &mut required,
                0,
            ),
            "Read Babel device property",
        )?;
    }
    ensure!(
        kind == DEVPROP_TYPE_STRING
            && required >= 2
            && required as usize <= words.len() * 2
            && required % 2 == 0,
        "Invalid device string property"
    );
    let slice = &words[..required as usize / 2];
    ensure!(slice.last() == Some(&0), "Device string is not terminated");
    Ok(String::from_utf16(slice.strip_suffix(&[0]).unwrap())?)
}
fn instance(set: &DeviceSet, device: &SP_DEVINFO_DATA) -> Result<String> {
    let mut words = vec![0_u16; MAX_WORDS];
    let mut required = 0;
    // SAFETY: the device belongs to set; output length is in UTF-16 code units.
    unsafe {
        check(
            SetupDiGetDeviceInstanceIdW(
                set.0,
                device,
                words.as_mut_ptr(),
                words.len() as u32,
                &mut required,
            ),
            "Read Babel instance identity",
        )?;
    }
    ensure!(
        required > 0 && required as usize <= words.len(),
        "Invalid instance identity size"
    );
    Ok(String::from_utf16(&words[..required as usize - 1])?)
}
fn find() -> Result<(DeviceSet, Vec<SP_DEVINFO_DATA>)> {
    // SAFETY: all classes/enumerators, including disconnected devices. The
    // update API matches hardware IDs globally, so the preflight must too.
    let handle = unsafe { SetupDiGetClassDevsW(null(), null(), null_mut(), DIGCF_ALLCLASSES) };
    ensure!(
        handle != -1,
        "Could not enumerate audio root devices: {}",
        std::io::Error::last_os_error()
    );
    let set = DeviceSet(handle);
    let mut found = Vec::new();
    for index in 0..4096 {
        let mut device = data();
        // SAFETY: mutable output has the SDK's cbSize and proper lifetime.
        let ok = unsafe { SetupDiEnumDeviceInfo(set.0, index, &mut device) };
        if ok == 0 {
            // SAFETY: last error from the failed enumeration.
            if unsafe { GetLastError() } == ERROR_NO_MORE_ITEMS {
                return Ok((set, found));
            }
            check(ok, "Enumerate audio root devices")?;
        }
        let ids = hardware(&set, &device)?;
        if ids.iter().any(|id| id.eq_ignore_ascii_case(HARDWARE_ID)) {
            ensure!(
                same_media(device.ClassGuid)
                    && is_own_hardware(&ids)
                    && instance(&set, &device)?
                        .to_ascii_uppercase()
                        .starts_with("ROOT\\BABELAUDIO\\"),
                "Another device contains the Babel hardware ID with an unexpected class, enumerator or additional IDs; refusing a global hardware-ID update"
            );
            found.push(device);
        }
    }
    bail!("Device enumeration exceeded its fixed limit")
}
fn owned_package(set: &DeviceSet, device: &SP_DEVINFO_DATA) -> Result<String> {
    ensure!(
        property(set, device, &DEVPKEY_Device_DriverProvider)?.eq_ignore_ascii_case("Babel"),
        "The root device has a different driver provider; refusing to change it"
    );
    ensure!(
        property(set, device, &DEVPKEY_Device_Service)?.eq_ignore_ascii_case("BabelAudio"),
        "The root device has a different service; refusing to change it"
    );
    let inf = property(set, device, &DEVPKEY_Device_DriverInfPath)?;
    oem_inf(&inf)?;
    Ok(inf)
}
fn remove_node(set: &DeviceSet, device: &SP_DEVINFO_DATA) -> Result<bool> {
    let parameters = SP_REMOVEDEVICE_PARAMS {
        ClassInstallHeader: SP_CLASSINSTALL_HEADER {
            cbSize: size_of::<SP_CLASSINSTALL_HEADER>() as u32,
            InstallFunction: DIF_REMOVE,
        },
        Scope: DI_REMOVEDEVICE_GLOBAL,
        HwProfile: 0,
    };
    // SAFETY: header is first field of the SDK removal struct; byte count covers
    // it entirely. SetupAPI copies parameters before the local struct is dropped.
    unsafe {
        check(
            SetupDiSetClassInstallParamsW(
                set.0,
                device,
                &parameters.ClassInstallHeader,
                size_of::<SP_REMOVEDEVICE_PARAMS>() as u32,
            ),
            "Configure Babel removal",
        )?;
        check(
            SetupDiCallClassInstaller(DIF_REMOVE, set.0, device),
            "Remove Babel root device",
        )?;
    }
    let mut status = SP_DEVINSTALL_PARAMS_W {
        cbSize: size_of::<SP_DEVINSTALL_PARAMS_W>() as u32,
        ..Default::default()
    };
    // SAFETY: output is SDK-sized and the set remains alive after DIF_REMOVE.
    unsafe {
        check(
            SetupDiGetDeviceInstallParamsW(set.0, device, &mut status),
            "Read removal restart status",
        )?;
    }
    Ok(status.Flags & (DI_NEEDREBOOT | DI_NEEDRESTART) != 0)
}
fn signed_inf(path: &Path) -> Result<Vec<u16>> {
    ensure!(
        path.is_absolute()
            && path
                .file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case("BabelAudio.inf")),
        "Supply the absolute path to BabelAudio.inf"
    );
    ensure!(path.is_file(), "BabelAudio.inf is missing");
    let canonical = path.canonicalize()?;
    let mut inf = wide(&canonical)?;
    // SetupCopyOEMInf has a documented MAX_PATH limit. Preserve UTF-16 while
    // normalizing canonicalize's verbatim prefix for the SetupAPI family.
    let unc: Vec<u16> = "\\\\?\\UNC\\".encode_utf16().collect();
    let verbatim: Vec<u16> = "\\\\?\\".encode_utf16().collect();
    if inf.starts_with(&unc) {
        inf = [vec![92, 92], inf[unc.len()..].to_vec()].concat();
    } else if inf.starts_with(&verbatim) {
        inf.drain(..verbatim.len());
    }
    ensure!(
        inf.len() <= 260,
        "Move the driver package to a shorter absolute path (SetupAPI requires at most 259 UTF-16 characters)"
    );
    let mut class = GUID::default();
    let mut name = [0_u16; 256];
    // SAFETY: all strings are terminated and class/name outputs are SDK-sized.
    unsafe {
        check(
            SetupDiGetINFClassW(
                inf.as_ptr(),
                &mut class,
                name.as_mut_ptr(),
                name.len() as u32,
                null_mut(),
            ),
            "Read INF class",
        )?;
    }
    ensure!(
        same_media(class),
        "INF does not belong to the MEDIA device class"
    );
    struct Inf(*mut std::ffi::c_void);
    impl Drop for Inf {
        fn drop(&mut self) {
            // SAFETY: this handle was returned by SetupOpenInfFile and is owned.
            unsafe {
                SetupCloseInfFile(self.0);
            }
        }
    }
    // SAFETY: validated terminated filename; WIN4 parser, no error-line output.
    let handle = unsafe { SetupOpenInfFileW(inf.as_ptr(), null(), INF_STYLE_WIN4, null_mut()) };
    ensure!(
        handle != INVALID_HANDLE_VALUE,
        "Could not parse Babel INF: {}",
        std::io::Error::last_os_error()
    );
    let parsed = Inf(handle);
    let field = |section: &str, key: Option<&str>, index: u32| -> Result<String> {
        let section = wide(section)?;
        let key = key.map(wide).transpose()?;
        let mut context = INFCONTEXT::default();
        let mut words = vec![0_u16; MAX_WORDS];
        let mut required = 0;
        // SAFETY: live INF, terminated section/key and SDK context/output buffers.
        unsafe {
            check(
                SetupFindFirstLineW(
                    parsed.0,
                    section.as_ptr(),
                    key.as_ref().map_or(null(), |s| s.as_ptr()),
                    &mut context,
                ),
                "Read Babel INF identity",
            )?;
            check(
                SetupGetStringFieldW(
                    &context,
                    index,
                    words.as_mut_ptr(),
                    words.len() as u32,
                    &mut required,
                ),
                "Read Babel INF field",
            )?;
        }
        ensure!(
            required > 0 && required as usize <= words.len(),
            "Invalid INF field length"
        );
        Ok(String::from_utf16(&words[..required as usize - 1])?)
    };
    ensure!(
        field("Version", Some("Provider"), 1)? == "Babel"
            && field("Version", Some("CatalogFile"), 1)?.eq_ignore_ascii_case("BabelAudio.cat")
            && field("BabelAudio.NT.Services", Some("AddService"), 1)? == "BabelAudio",
        "Signed INF does not identify the Babel provider, catalog and service"
    );
    let models = if cfg!(target_arch = "aarch64") {
        "Babel.NTarm64.10.0...19041"
    } else {
        "Babel.NTamd64.10.0...19041"
    };
    ensure!(
        field(models, None, 2)?.eq_ignore_ascii_case(HARDWARE_ID),
        "INF hardware identity or architecture does not match Babel"
    );
    let mut signer = SP_INF_SIGNER_INFO_V2_W {
        cbSize: size_of::<SP_INF_SIGNER_INFO_V2_W>() as u32,
        ..Default::default()
    };
    // SAFETY: typed SDK signer buffer. No certificate/boot-policy changes occur.
    unsafe {
        check(
            SetupVerifyInfFileW(inf.as_ptr(), null(), &mut signer),
            "Verify signed driver catalog (unsigned/untrusted packages are not installed)",
        )?;
    }
    Ok(inf)
}

fn published_package(inf: &[u16]) -> Result<Option<String>> {
    let mut published = vec![0_u16; 260];
    let mut required = 0;
    // SAFETY: signed source INF and owned UTF-16 destination. The combined
    // documented flags prohibit both adding a new package and overwriting an
    // existing one. ERROR_FILE_EXISTS returns the content/catalog-matched OEM
    // identity. Unlike DriverStoreLocation, this accepts a distribution INF.
    let ok = unsafe {
        SetupCopyOEMInfW(
            inf.as_ptr(),
            null(),
            SPOST_NONE,
            SP_COPY_REPLACEONLY | SP_COPY_NOOVERWRITE,
            published.as_mut_ptr(),
            published.len() as u32,
            &mut required,
            null_mut(),
        )
    };
    if ok != 0 {
        return Ok(None);
    }
    // SAFETY: inspect immediately after the failed call above.
    let error = unsafe { GetLastError() };
    if matches!(error, ERROR_FILE_NOT_FOUND | ERROR_NOT_FOUND) {
        return Ok(None);
    }
    ensure!(
        error == ERROR_FILE_EXISTS,
        "Resolve Babel OEM package without staging: {}",
        std::io::Error::from_raw_os_error(error as i32)
    );
    let end = published
        .iter()
        .position(|v| *v == 0)
        .context("Invalid OEM path terminator")?;
    ensure!(
        end > 0,
        "No OEM identity was returned; refusing to guess a package"
    );
    let path = String::from_utf16(&published[..end])?;
    let name = Path::new(&path)
        .file_name()
        .and_then(|s| s.to_str())
        .context("Invalid OEM basename")?;
    Ok(Some(oem_inf(name)?.to_owned()))
}

fn uninstall_package(package: &str) -> Result<()> {
    let name = wide(oem_inf(package)?)?;
    // SAFETY: exact OEM basename from an owned device or the signed INF's store
    // mapping. Flags zero refuses removing a driver package that is still in use.
    unsafe {
        check(
            SetupUninstallOEMInfW(name.as_ptr(), 0, null()),
            "Babel device was removed, but the package is still in use. Reboot, then retry remove with the same --inf to finish cleanup",
        )
    }
}

pub(super) fn execute(action: Action) -> Result<Value> {
    // Read-only gate must stay before elevation, the installer mutex, and all
    // package/device mutations. Enumeration errors also prevent app removal.
    if matches!(action, Action::CheckAbsent) {
        let (_set, devices) = find()?;
        ensure!(
            devices.is_empty(),
            "The Babel Audio driver is still installed. Close audio applications and run drivers\\windows\\uninstall.ps1 from an Administrator PowerShell before uninstalling the app."
        );
        return Ok(json!({"status":"absent", "hardware_id":HARDWARE_ID}));
    }
    if matches!(action, Action::List) {
        let (set, devices) = find()?;
        let ids: Result<Vec<_>> = devices.iter().map(|d| instance(&set, d)).collect();
        return Ok(json!({"hardware_id": HARDWARE_ID, "instances": ids?}));
    }
    let _lock = elevated_and_locked()?;
    let (set, devices) = find()?;
    ensure!(
        devices.len() <= 1,
        "Several Babel root devices exist; refusing ambiguous modification"
    );
    match action {
        Action::Install { inf } => {
            let inf = signed_inf(&inf)?;
            let hardware = wide(HARDWARE_ID)?;
            if let Some(device) = devices.first() {
                owned_package(&set, device)?;
            }
            let fresh = devices.is_empty();
            let mut device = devices.first().copied().unwrap_or_else(data);
            if fresh {
                let name = wide("BabelAudio")?;
                let description = wide("Babel Audio v1")?;
                // SAFETY: all arguments point to live SDK structs/strings;
                // generated instance ID stays within this DeviceSet.
                unsafe {
                    check(
                        SetupDiCreateDeviceInfoW(
                            set.0,
                            name.as_ptr(),
                            &MEDIA,
                            description.as_ptr(),
                            null_mut(),
                            DICD_GENERATE_ID,
                            &mut device,
                        ),
                        "Create Babel root device",
                    )?;
                }
                let mut multi = hardware.clone();
                multi.push(0);
                // SAFETY: REG_MULTI_SZ has its required double nul; size in bytes.
                unsafe {
                    check(
                        SetupDiSetDeviceRegistryPropertyW(
                            set.0,
                            &mut device,
                            SPDRP_HARDWAREID,
                            multi.as_ptr().cast(),
                            (multi.len() * 2) as u32,
                        ),
                        "Set Babel hardware identity",
                    )?;
                    check(
                        SetupDiCallClassInstaller(DIF_REGISTERDEVICE, set.0, &device),
                        "Register Babel root device",
                    )?;
                }
            }
            let mut reboot = 0;
            // SAFETY: explicit hardware ID and trusted full INF; Windows enforces
            // kernel-signing policy. This cannot target another hardware ID.
            // Let Windows select only a better/equal eligible driver; never
            // force-downgrade a newer installed version. Any required restart
            // is reported through reboot instead of being started automatically.
            let installed = unsafe {
                UpdateDriverForPlugAndPlayDevicesW(
                    null_mut(),
                    hardware.as_ptr(),
                    inf.as_ptr(),
                    0,
                    &mut reboot,
                )
            };
            let unchanged =
                installed == 0 && !fresh && unsafe { GetLastError() } == ERROR_NO_MORE_ITEMS;
            if installed == 0 && !unchanged {
                let error = std::io::Error::last_os_error();
                if fresh {
                    remove_node(&set, &device)
                        .context("Install failed and rollback of the new Babel node also failed")?;
                }
                return Err(error).context("Install Babel signed driver");
            }
            // Do not claim success if the signed INF did not expose our service/provider.
            let package = owned_package(&set, &device)?;
            Ok(
                json!({"status":if unchanged {"unchanged"} else {"installed"}, "instance":instance(&set, &device)?, "oem_inf":package, "reboot_required":reboot != 0}),
            )
        }
        Action::Remove { inf } => {
            let Some(device) = devices.first() else {
                // Finish a prior removal after reboot, scoped to this signed INF.
                // Never enumerate/delete arbitrary oem*.inf or other audio drivers.
                if let Some(path) = inf {
                    let source = signed_inf(&path)?;
                    if let Some(package) = published_package(&source)? {
                        uninstall_package(&package)?;
                        return Ok(
                            json!({"status":"package_removed", "oem_inf":package, "reboot_required":false}),
                        );
                    }
                }
                return Ok(json!({"status":"not_installed", "reboot_required":false}));
            };
            let package = owned_package(&set, device)?;
            let id = instance(&set, device)?;
            let reboot = remove_node(&set, device)?;
            uninstall_package(&package)?;
            Ok(
                json!({"status":"removed", "instance":id, "oem_inf":package, "reboot_required":reboot}),
            )
        }
        Action::List | Action::CheckAbsent => unreachable!(),
    }
}
