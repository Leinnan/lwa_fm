//! Windows drives from the drive letters, and ejecting them the way
//! "Safely Remove Hardware" does: ask Plug and Play to remove the device,
//! which flushes the volume and names whatever is holding it open.
#![allow(unsafe_code)]

use std::{
    ffi::OsStr,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, bail};
use windows::{
    Win32::{
        Devices::DeviceAndDriverInstallation::{
            CM_Get_Parent, CM_Request_Device_EjectW, CR_SUCCESS, DIGCF_DEVICEINTERFACE,
            DIGCF_PRESENT, PNP_VETO_TYPE, PNP_VetoTypeUnknown, SP_DEVICE_INTERFACE_DATA,
            SP_DEVICE_INTERFACE_DETAIL_DATA_W, SP_DEVINFO_DATA, SetupDiDestroyDeviceInfoList,
            SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW, SetupDiGetDeviceInterfaceDetailW,
        },
        Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, NO_ERROR},
        NetworkManagement::WNet::{WNetCancelConnection2W, WNetGetConnectionW},
        Storage::FileSystem::{
            BusType1394, BusTypeMmc, BusTypeSd, BusTypeUsb, CreateFileW, FILE_FLAGS_AND_ATTRIBUTES,
            FILE_SHARE_READ, FILE_SHARE_WRITE, GetDiskFreeSpaceExW, GetDriveTypeW,
            GetLogicalDrives, GetVolumeInformationW, OPEN_EXISTING,
        },
        System::{
            Diagnostics::Debug::{SEM_FAILCRITICALERRORS, SetThreadErrorMode},
            IO::DeviceIoControl,
            Ioctl::{
                FSCTL_DISMOUNT_VOLUME, FSCTL_LOCK_VOLUME, GUID_DEVINTERFACE_CDROM,
                GUID_DEVINTERFACE_DISK, IOCTL_STORAGE_EJECT_MEDIA, IOCTL_STORAGE_GET_DEVICE_NUMBER,
                IOCTL_STORAGE_MEDIA_REMOVAL, IOCTL_STORAGE_QUERY_PROPERTY, PREVENT_MEDIA_REMOVAL,
                PropertyStandardQuery, STORAGE_DEVICE_DESCRIPTOR, STORAGE_DEVICE_NUMBER,
                STORAGE_PROPERTY_QUERY, StorageDeviceProperty,
            },
        },
    },
    core::{GUID, PCWSTR, PWSTR},
};

use super::{Drive, DriveKind};

// `GetDriveTypeW` results (winbase.h).
const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;
const DRIVE_REMOTE: u32 = 4;
const DRIVE_CDROM: u32 = 5;
const DRIVE_RAMDISK: u32 = 6;

// `STORAGE_DEVICE_NUMBER::DeviceType` values (winioctl.h).
const FILE_DEVICE_CD_ROM: u32 = 0x02;
const FILE_DEVICE_DVD: u32 = 0x33;

fn wide(text: impl AsRef<OsStr>) -> Vec<u16> {
    text.as_ref().encode_wide().chain(Some(0)).collect()
}

fn from_wide(buffer: &[u16]) -> String {
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

/// A handle closed on drop.
struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateFileW and is closed only here.
        _ = unsafe { CloseHandle(self.0) };
    }
}

/// Open `\\.\E:` (the volume, not its root folder).
fn open_volume(letter: char, access: u32) -> windows::core::Result<Handle> {
    open_device(&format!(r"\\.\{letter}:"), access)
}

fn open_device(path: &str, access: u32) -> windows::core::Result<Handle> {
    let path = wide(path);
    // SAFETY: `path` is NUL-terminated and outlives the call.
    unsafe {
        CreateFileW(
            PCWSTR(path.as_ptr()),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
    }
    .map(Handle)
}

/// `DeviceIoControl` with a plain input and output struct.
fn ioctl<I, O>(handle: &Handle, code: u32, input: Option<&I>, output: Option<&mut O>) -> bool {
    let mut returned = 0u32;
    let (in_ptr, in_len) = input.map_or((None, 0), |i| {
        (Some(std::ptr::from_ref(i).cast()), size_of::<I>() as u32)
    });
    let (out_ptr, out_len) = output.map_or((None, 0), |o| {
        (Some(std::ptr::from_mut(o).cast()), size_of::<O>() as u32)
    });
    // SAFETY: the buffers are live references of the sizes given.
    unsafe {
        DeviceIoControl(
            handle.0,
            code,
            in_ptr,
            in_len,
            out_ptr,
            out_len,
            Some(&raw mut returned),
            None,
        )
    }
    .is_ok()
}

pub fn list() -> Vec<Drive> {
    // An empty card reader or optical drive must fail quietly instead of
    // showing the "There is no disk in the drive" dialog.
    // SAFETY: only changes this thread's error mode.
    _ = unsafe { SetThreadErrorMode(SEM_FAILCRITICALERRORS, None) };
    // SAFETY: no arguments.
    let mask = unsafe { GetLogicalDrives() };
    (0u8..26)
        .filter(|bit| mask & (1 << bit) != 0)
        .filter_map(|bit| drive_info(char::from(b'A' + bit)))
        .collect()
}

fn drive_info(letter: char) -> Option<Drive> {
    let root = format!("{letter}:\\");
    let root_wide = wide(&root);
    // SAFETY: NUL-terminated path.
    let drive_type = unsafe { GetDriveTypeW(PCWSTR(root_wide.as_ptr())) };
    let mut kind = match drive_type {
        DRIVE_REMOVABLE => DriveKind::Removable,
        DRIVE_FIXED => DriveKind::Fixed,
        DRIVE_REMOTE => DriveKind::Network,
        DRIVE_CDROM => DriveKind::Optical,
        DRIVE_RAMDISK => DriveKind::Virtual,
        _ => return None,
    };
    if kind == DriveKind::Network {
        // Volume queries on an offline share can hang for seconds, so a
        // network drive gets its share name and no capacity.
        let label = remote_name(letter).unwrap_or_else(|| kind.generic_name().to_owned());
        return Some(Drive {
            mount: PathBuf::from(root),
            label,
            kind,
            total: 0,
            available: 0,
            file_system: String::new(),
        });
    }
    let mut label = [0u16; 261];
    let mut file_system = [0u16; 261];
    // SAFETY: the buffers are writable for their whole length.
    let has_media = unsafe {
        GetVolumeInformationW(
            PCWSTR(root_wide.as_ptr()),
            Some(&mut label),
            None,
            None,
            None,
            Some(&mut file_system),
        )
    }
    .is_ok();
    if !has_media {
        // Nothing to browse in an empty reader or drive.
        return None;
    }
    if kind == DriveKind::Fixed && is_external_bus(letter) {
        kind = DriveKind::Removable;
    }
    let (mut available, mut total) = (0u64, 0u64);
    // SAFETY: the out pointers are live locals.
    _ = unsafe {
        GetDiskFreeSpaceExW(
            PCWSTR(root_wide.as_ptr()),
            Some(&raw mut available),
            Some(&raw mut total),
            None,
        )
    };
    let label = from_wide(&label);
    Some(Drive {
        mount: PathBuf::from(root),
        label: if label.is_empty() {
            kind.generic_name().to_owned()
        } else {
            label
        },
        kind,
        total,
        available,
        file_system: from_wide(&file_system),
    })
}

/// External USB and `FireWire` disks report as fixed drives; their bus says
/// they can be unplugged.
fn is_external_bus(letter: char) -> bool {
    let Ok(volume) = open_volume(letter, 0) else {
        return false;
    };
    let query = STORAGE_PROPERTY_QUERY {
        PropertyId: StorageDeviceProperty,
        QueryType: PropertyStandardQuery,
        AdditionalParameters: [0],
    };
    let mut descriptor = STORAGE_DEVICE_DESCRIPTOR::default();
    ioctl(
        &volume,
        IOCTL_STORAGE_QUERY_PROPERTY,
        Some(&query),
        Some(&mut descriptor),
    ) && [BusTypeUsb, BusType1394, BusTypeSd, BusTypeMmc].contains(&descriptor.BusType)
}

/// `\\server\share` behind a mapped drive letter.
fn remote_name(letter: char) -> Option<String> {
    let local = wide(format!("{letter}:"));
    let mut buffer = [0u16; 512];
    let mut length = buffer.len() as u32;
    // SAFETY: `buffer` holds `length` characters.
    let status = unsafe {
        WNetGetConnectionW(
            PCWSTR(local.as_ptr()),
            Some(PWSTR(buffer.as_mut_ptr())),
            &raw mut length,
        )
    };
    (status == NO_ERROR).then(|| from_wide(&buffer))
}

fn drive_letter(mount: &Path) -> anyhow::Result<char> {
    let text = mount.to_string_lossy();
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(letter), Some(':')) if letter.is_ascii_alphabetic() => {
            Ok(letter.to_ascii_uppercase())
        }
        _ => bail!("{} is not a drive letter", mount.display()),
    }
}

pub fn eject(drive: &Drive) -> anyhow::Result<()> {
    let letter = drive_letter(&drive.mount)?;
    match drive.kind {
        DriveKind::Network => disconnect(letter),
        DriveKind::Optical => eject_media(letter),
        _ => match remove_device(letter) {
            Ok(()) => Ok(()),
            // A veto names what holds the drive; anything else (a card
            // reader whose slot cannot be removed) falls back to ejecting
            // just the media.
            Err(EjectError::Vetoed(reason)) => bail!("{reason}"),
            Err(EjectError::Other(err)) => {
                log::info!("Device removal of {letter}: failed ({err:#}), ejecting media");
                eject_media(letter)
            }
        },
    }
}

fn disconnect(letter: char) -> anyhow::Result<()> {
    let name = wide(format!("{letter}:"));
    // SAFETY: NUL-terminated name.
    let status = unsafe {
        WNetCancelConnection2W(
            PCWSTR(name.as_ptr()),
            windows::Win32::NetworkManagement::WNet::NET_CONNECT_FLAGS(0),
            false,
        )
    };
    if status != NO_ERROR {
        bail!(
            "Could not disconnect {letter}: ({})",
            windows::core::Error::from(status.to_hresult()).message()
        );
    }
    Ok(())
}

enum EjectError {
    /// Windows refused: a program or service has files open.
    Vetoed(String),
    Other(anyhow::Error),
}

impl From<anyhow::Error> for EjectError {
    fn from(err: anyhow::Error) -> Self {
        Self::Other(err)
    }
}

/// Remove the storage device behind a volume, as "Safely Remove Hardware".
fn remove_device(letter: char) -> Result<(), EjectError> {
    let number = {
        // Closed before the eject: our own handle would veto it.
        let volume = open_volume(letter, 0).context("Could not open the volume")?;
        let mut number = STORAGE_DEVICE_NUMBER::default();
        if !ioctl::<(), _>(
            &volume,
            IOCTL_STORAGE_GET_DEVICE_NUMBER,
            None,
            Some(&mut number),
        ) {
            return Err(anyhow::anyhow!("The drive did not report its device number").into());
        }
        number
    };
    let class = if matches!(number.DeviceType, FILE_DEVICE_CD_ROM | FILE_DEVICE_DVD) {
        GUID_DEVINTERFACE_CDROM
    } else {
        GUID_DEVINTERFACE_DISK
    };
    let disk = device_instance(&class, &number).context("Could not find the drive's device")?;
    let mut parent = 0u32;
    // SAFETY: `parent` is a live local.
    if unsafe { CM_Get_Parent(&raw mut parent, disk, 0) } != CR_SUCCESS {
        return Err(anyhow::anyhow!("Could not find the drive's controller").into());
    }
    // Windows sometimes vetoes the first attempt while it is still closing
    // handles the app just released, so retry a few times.
    let mut last_veto = String::new();
    for attempt in 0..4 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(400));
        }
        let mut veto_type = PNP_VetoTypeUnknown;
        let mut veto_name = [0u16; 520];
        // SAFETY: the veto buffers are live locals.
        let result = unsafe {
            CM_Request_Device_EjectW(parent, Some(&raw mut veto_type), Some(&mut veto_name), 0)
        };
        if result == CR_SUCCESS && veto_type == PNP_VetoTypeUnknown {
            return Ok(());
        }
        if veto_type == PNP_VetoTypeUnknown {
            return Err(
                anyhow::anyhow!("Windows could not remove the device ({})", result.0).into(),
            );
        }
        last_veto = veto_message(veto_type, &from_wide(&veto_name));
    }
    Err(EjectError::Vetoed(last_veto))
}

fn veto_message(veto: PNP_VETO_TYPE, name: &str) -> String {
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        PNP_VetoOutstandingOpen, PNP_VetoWindowsApp, PNP_VetoWindowsService,
    };
    let holder = Path::new(name)
        .file_name()
        .map_or_else(|| name.to_owned(), |n| n.to_string_lossy().into_owned());
    if veto == PNP_VetoWindowsApp && !holder.is_empty() {
        format!("The drive is in use by {holder}. Close it and try again.")
    } else if veto == PNP_VetoWindowsService && !holder.is_empty() {
        format!("The drive is in use by the {holder} service.")
    } else if [
        PNP_VetoOutstandingOpen,
        PNP_VetoWindowsApp,
        PNP_VetoWindowsService,
    ]
    .contains(&veto)
    {
        "The drive is in use. Close files and programs using it and try again.".to_owned()
    } else {
        format!("Windows refused to eject the drive (reason {}).", veto.0)
    }
}

/// The device instance of the disk or CD-ROM interface with `number`.
fn device_instance(class: &GUID, number: &STORAGE_DEVICE_NUMBER) -> anyhow::Result<u32> {
    // SAFETY: a fresh device info set, destroyed below.
    let set = unsafe {
        SetupDiGetClassDevsW(
            Some(std::ptr::from_ref(class)),
            PCWSTR::null(),
            None,
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
    }?;
    let found = (0..)
        .map_while(|index| {
            let mut interface = SP_DEVICE_INTERFACE_DATA {
                cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
                ..Default::default()
            };
            // SAFETY: `interface` is a live, sized local.
            unsafe {
                SetupDiEnumDeviceInterfaces(
                    set,
                    None,
                    std::ptr::from_ref(class),
                    index,
                    &raw mut interface,
                )
            }
            .ok()
            .map(|()| interface)
        })
        .find_map(|interface| {
            let (path, instance) = interface_path(set, &interface)?;
            let device = open_device(&path, 0).ok()?;
            let mut candidate = STORAGE_DEVICE_NUMBER::default();
            (ioctl::<(), _>(
                &device,
                IOCTL_STORAGE_GET_DEVICE_NUMBER,
                None,
                Some(&mut candidate),
            ) && candidate.DeviceType == number.DeviceType
                && candidate.DeviceNumber == number.DeviceNumber)
                .then_some(instance)
        });
    // SAFETY: `set` came from SetupDiGetClassDevsW.
    _ = unsafe { SetupDiDestroyDeviceInfoList(set) };
    found.context("No matching device")
}

/// The device path of an interface and the instance it belongs to.
fn interface_path(
    set: windows::Win32::Devices::DeviceAndDriverInstallation::HDEVINFO,
    interface: &SP_DEVICE_INTERFACE_DATA,
) -> Option<(String, u32)> {
    let mut required = 0u32;
    // SAFETY: size query; fails with "insufficient buffer" by design.
    _ = unsafe {
        SetupDiGetDeviceInterfaceDetailW(
            set,
            std::ptr::from_ref(interface),
            None,
            0,
            Some(&raw mut required),
            None,
        )
    };
    if (required as usize) < size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() {
        return None;
    }
    // u64 storage keeps the detail struct aligned.
    let mut buffer = vec![0u64; (required as usize).div_ceil(8)];
    let detail = buffer
        .as_mut_ptr()
        .cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
    let mut info = SP_DEVINFO_DATA {
        cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
        ..Default::default()
    };
    // SAFETY: `buffer` is `required` bytes, aligned; cbSize is the fixed
    // part of the struct as the API expects.
    unsafe {
        (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
        SetupDiGetDeviceInterfaceDetailW(
            set,
            std::ptr::from_ref(interface),
            Some(detail),
            required,
            None,
            Some(&raw mut info),
        )
        .ok()?;
    }
    // SAFETY: the path starts at `DevicePath` and is NUL-terminated within
    // the `required` bytes the API filled.
    let path = unsafe {
        let start = (&raw const (*detail).DevicePath).cast::<u16>();
        let offset = start.cast::<u8>().offset_from(buffer.as_ptr().cast::<u8>()) as usize;
        let max = (required as usize).saturating_sub(offset) / 2;
        from_wide(std::slice::from_raw_parts(start, max))
    };
    Some((path, info.DevInst))
}

/// Lock, dismount and eject the media, for optical drives and card readers
/// whose device stays attached.
fn eject_media(letter: char) -> anyhow::Result<()> {
    let volume = open_volume(letter, GENERIC_READ.0 | GENERIC_WRITE.0)
        .context("Could not open the drive")?;
    let locked = (0..20).any(|attempt| {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(100));
        }
        ioctl::<(), ()>(&volume, FSCTL_LOCK_VOLUME, None, None)
    });
    if !locked {
        bail!("The drive is in use. Close files and programs using it and try again.");
    }
    if !ioctl::<(), ()>(&volume, FSCTL_DISMOUNT_VOLUME, None, None) {
        bail!("Windows could not dismount the drive");
    }
    let allow = PREVENT_MEDIA_REMOVAL {
        PreventMediaRemoval: false,
    };
    _ = ioctl::<_, ()>(&volume, IOCTL_STORAGE_MEDIA_REMOVAL, Some(&allow), None);
    // Card readers have no tray; dismounted is already safe to remove.
    _ = ioctl::<(), ()>(&volume, IOCTL_STORAGE_EJECT_MEDIA, None, None);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_from_mounts() {
        assert_eq!(drive_letter(Path::new(r"e:\")).ok(), Some('E'));
        assert!(drive_letter(Path::new(r"\\server\share")).is_err());
    }

    #[test]
    fn lists_the_system_drive() {
        let system = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        let drives = list();
        assert!(
            drives
                .iter()
                .any(|d| d.mount.to_string_lossy().starts_with(&system) && d.total > 0),
            "{drives:?}"
        );
    }
}
