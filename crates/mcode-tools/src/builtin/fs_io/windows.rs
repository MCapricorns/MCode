//! Windows NT handle-relative file operations for the host file kernel.
//!
//! Child opens use relative `NtOpenFile`/`NtCreateFile` with
//! `FILE_OPEN_REPARSE_POINT`, reject reparse points and ADS names, and prove
//! identity with the 128-bit `FILE_ID_128` plus volume serial. NTSTATUS is
//! mapped with `RtlNtStatusToDosError`; `GetLastError` is never consulted
//! after an NT call.
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::mem::size_of;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::{Component, Path, PathBuf, Prefix};
use std::ptr::{null, null_mut};

use tokio_util::sync::CancellationToken;
use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::FILE_DISPOSITION_DELETE;
use windows_sys::Wdk::Storage::FileSystem::FILE_DISPOSITION_IGNORE_READONLY_ATTRIBUTE;
use windows_sys::Wdk::Storage::FileSystem::FILE_DISPOSITION_POSIX_SEMANTICS;
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_CREATE, FILE_DIRECTORY_FILE, FILE_DISPOSITION_INFORMATION,
    FILE_DISPOSITION_INFORMATION_EX, FILE_NON_DIRECTORY_FILE, FILE_OPEN_REPARSE_POINT,
    FILE_RENAME_INFORMATION, FILE_SYNCHRONOUS_IO_NONALERT, FileDispositionInformation,
    FileDispositionInformationEx, FileFullDirectoryInformation, FileRenameInformation,
    NtCreateFile, NtOpenFile, NtQueryDirectoryFile, NtSetInformationFile,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, ERROR_SUCCESS, HANDLE,
    INVALID_HANDLE_VALUE, LocalFree, NTSTATUS, OBJ_CASE_INSENSITIVE, RtlNtStatusToDosError,
    STATUS_NO_MORE_FILES, STATUS_SUCCESS, UNICODE_STRING,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
    SE_FILE_OBJECT, SetSecurityInfo,
};
use windows_sys::Win32::Security::{
    DACL_SECURITY_INFORMATION, GROUP_SECURITY_INFORMATION, GetSecurityDescriptorControl,
    GetTokenInformation, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, SE_DACL_PROTECTED, SECURITY_DESCRIPTOR, SECURITY_DESCRIPTOR_CONTROL,
    TOKEN_QUERY, TOKEN_USER, TokenUser, UNPROTECTED_DACL_SECURITY_INFORMATION,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL,
    FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_BASIC_INFO,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_ID_INFO,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FileBasicInfo, FileIdInfo,
    FlushFileBuffers, GetFileInformationByHandle, GetFileInformationByHandleEx, OPEN_EXISTING,
    SetFileInformationByHandle, WRITE_DAC, WRITE_OWNER,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use super::{
    ChildOpen, FileIdentity, FileKind, FileMeta, MAX_DIR_WIDTH, OpenedChild, WRITE_CHUNK,
    check_cancel, map_not_found,
};
use crate::builtin::fs_search::{strip_verbatim_prefix, validate_component_name};

const DIR_LIST_WORDS: usize = 8192;

fn ntstatus_error(status: NTSTATUS) -> io::Error {
    // `Nt*` calls return NTSTATUS and do not define `GetLastError`.
    // SAFETY: converting that returned status is the documented use of
    // `RtlNtStatusToDosError` and has no pointer preconditions.
    let code = unsafe { RtlNtStatusToDosError(status) };
    let code = i32::try_from(code).unwrap_or(i32::MAX);
    io::Error::from_raw_os_error(code)
}

fn win32_error(code: u32) -> io::Error {
    let code = i32::try_from(code).unwrap_or(i32::MAX);
    io::Error::from_raw_os_error(code)
}

fn encode_component(name: &OsStr) -> io::Result<Vec<u16>> {
    validate_component_name(name)?;
    let wide: Vec<u16> = name.encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path component contains NUL",
        ));
    }
    Ok(wide)
}

fn into_file(handle: HANDLE) -> File {
    // SAFETY: `handle` is a fresh successful NT handle and no other owner
    // will close it after this transfer.
    unsafe { File::from_raw_handle(handle) }
}

fn windows_extended_length_path(path: &Path) -> PathBuf {
    if !path.is_absolute() {
        return path.to_path_buf();
    }
    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return path.to_path_buf();
    };
    match prefix.kind() {
        Prefix::Disk(_) => {
            let mut extended = OsString::from(r"\\?\");
            extended.push(path.as_os_str());
            PathBuf::from(extended)
        }
        Prefix::UNC(server, share) => {
            let mut authority = OsString::from(r"\\?\UNC\");
            authority.push(server);
            authority.push(r"\");
            authority.push(share);
            let mut extended = PathBuf::from(authority);
            for component in components {
                if !matches!(component, Component::RootDir) {
                    extended.push(component.as_os_str());
                }
            }
            extended
        }
        _ => path.to_path_buf(),
    }
}

struct HandleInfo {
    identity: FileIdentity,
    kind: FileKind,
    reparse: bool,
    nlink: u32,
    attributes: u32,
    size: u64,
    mtime: i64,
}

fn query_handle(file: &File) -> io::Result<HandleInfo> {
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `file` is live; `information` is writable documented storage.
    let success = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) };
    if success == 0 {
        return Err(io::Error::last_os_error());
    }
    let kind = if information.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        FileKind::Directory
    } else {
        FileKind::File
    };
    let mut id_info = FILE_ID_INFO::default();
    let id_size = u32::try_from(size_of::<FILE_ID_INFO>()).expect("FILE_ID_INFO fits in u32");
    // SAFETY: `id_info` is writable `FILE_ID_INFO` storage. ReFS uniqueness
    // requires the 128-bit `FileId`.
    let id_ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            (&raw mut id_info).cast(),
            id_size,
        )
    };
    if id_ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let write = information.ftLastWriteTime;
    let mtime = (i64::from(write.dwHighDateTime) << 32) | i64::from(write.dwLowDateTime);
    Ok(HandleInfo {
        identity: FileIdentity {
            volume: id_info.VolumeSerialNumber,
            file_id: id_info.FileId.Identifier,
        },
        kind,
        reparse: information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0,
        nlink: information.nNumberOfLinks,
        attributes: information.dwFileAttributes,
        size: (u64::from(information.nFileSizeHigh) << 32) | u64::from(information.nFileSizeLow),
        mtime,
    })
}

fn meta_from_file(file: &File, reject_reparse: bool) -> io::Result<FileMeta> {
    let info = query_handle(file)?;
    if reject_reparse && info.reparse {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "reparse-point traversal is not permitted",
        ));
    }
    Ok(FileMeta {
        identity: info.identity,
        kind: info.kind,
        size: info.size,
        mtime_secs: info.mtime,
        mtime_nsecs: 0,
        nlink: u64::from(info.nlink),
        unix_mode: 0,
        unix_uid: 0,
        unix_gid: 0,
        windows_attributes: info.attributes,
    })
}

fn enforce_same_volume(parent: &File, child: &File) -> io::Result<()> {
    let parent_info = query_handle(parent)?;
    if parent_info.kind != FileKind::Directory {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "walk parent is no longer a directory",
        ));
    }
    let child_info = query_handle(child)?;
    if parent_info.identity.volume != child_info.identity.volume {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "mount traversal is not permitted",
        ));
    }
    Ok(())
}

fn nt_open(
    parent: &File,
    name: &OsStr,
    desired_access: u32,
    options: u32,
    case_insensitive: bool,
) -> io::Result<File> {
    let mut wide = encode_component(name)?;
    let byte_len = wide
        .len()
        .checked_mul(size_of::<u16>())
        .and_then(|length| u16::try_from(length).ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path component is too long"))?;
    let object_name = UNICODE_STRING {
        Length: byte_len,
        MaximumLength: byte_len,
        Buffer: wide.as_mut_ptr(),
    };
    let object_attributes = OBJECT_ATTRIBUTES {
        Length: u32::try_from(size_of::<OBJECT_ATTRIBUTES>())
            .expect("OBJECT_ATTRIBUTES size must fit in u32"),
        RootDirectory: parent.as_raw_handle(),
        ObjectName: &object_name,
        Attributes: if case_insensitive {
            OBJ_CASE_INSENSITIVE
        } else {
            0
        },
        SecurityDescriptor: null(),
        SecurityQualityOfService: null(),
    };
    let mut handle = INVALID_HANDLE_VALUE;
    let mut io_status = IO_STATUS_BLOCK::default();
    // SAFETY: `parent` stays live; `object_name` references `wide` for this
    // call; output pointers reference initialized writable storage.
    let status = unsafe {
        NtOpenFile(
            &mut handle,
            desired_access,
            &object_attributes,
            &mut io_status,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            options,
        )
    };
    if status < 0 {
        return Err(map_not_found(ntstatus_error(status)));
    }
    Ok(into_file(handle))
}

#[expect(
    clippy::too_many_arguments,
    reason = "NT create needs parent, name, access, attributes, disposition, options, share, and optional SD together"
)]
fn nt_create(
    parent: &File,
    name: &OsStr,
    desired_access: u32,
    attributes: u32,
    disposition: u32,
    options: u32,
    share_access: u32,
    security_descriptor: *const SECURITY_DESCRIPTOR,
) -> io::Result<File> {
    let mut wide = encode_component(name)?;
    let byte_len = wide
        .len()
        .checked_mul(size_of::<u16>())
        .and_then(|length| u16::try_from(length).ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path component is too long"))?;
    let object_name = UNICODE_STRING {
        Length: byte_len,
        MaximumLength: byte_len,
        Buffer: wide.as_mut_ptr(),
    };
    let object_attributes = OBJECT_ATTRIBUTES {
        Length: u32::try_from(size_of::<OBJECT_ATTRIBUTES>())
            .expect("OBJECT_ATTRIBUTES size must fit in u32"),
        RootDirectory: parent.as_raw_handle(),
        ObjectName: &object_name,
        Attributes: 0,
        SecurityDescriptor: security_descriptor,
        SecurityQualityOfService: null(),
    };
    let mut handle = INVALID_HANDLE_VALUE;
    let mut io_status = IO_STATUS_BLOCK::default();
    // SAFETY: parent handle, name buffer, optional security descriptor, and
    // output pointers are live for the call. NTSTATUS is the return value.
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            desired_access,
            &object_attributes,
            &mut io_status,
            null(),
            attributes,
            share_access,
            disposition,
            options,
            null(),
            0,
        )
    };
    if status < 0 {
        return Err(map_not_found(ntstatus_error(status)));
    }
    Ok(into_file(handle))
}

/// Owner-only protected DACL used for payload temps.
///
/// The descriptor is allocated by
/// `ConvertStringSecurityDescriptorToSecurityDescriptorW` and freed with
/// `LocalFree`. It must stay alive for the `NtCreateFile` that consumes it.
struct PrivateSd(PSECURITY_DESCRIPTOR);

impl PrivateSd {
    fn as_ptr(&self) -> *const SECURITY_DESCRIPTOR {
        self.0.cast()
    }
}

impl Drop for PrivateSd {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: `ConvertStringSecurityDescriptorToSecurityDescriptorW`
            // allocated this descriptor.
            let _ = unsafe { LocalFree(self.0.cast()) };
        }
    }
}

/// Builds a protected DACL granting full access only to the current user
/// and SYSTEM, so a permissive parent cannot make the payload temp
/// world-readable.
fn private_temp_descriptor() -> io::Result<PrivateSd> {
    let mut token = INVALID_HANDLE_VALUE;
    // SAFETY: `GetCurrentProcess` is a pseudo-handle that is not closed;
    // `token` is written only on success and is then an owned handle.
    let ok = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    struct TokenGuard(HANDLE);
    impl Drop for TokenGuard {
        fn drop(&mut self) {
            if self.0 != INVALID_HANDLE_VALUE && !self.0.is_null() {
                // SAFETY: `OpenProcessToken` returned this owned handle.
                let _ = unsafe { CloseHandle(self.0) };
            }
        }
    }
    let token = TokenGuard(token);
    let mut needed = 0u32;
    // SAFETY: size probe; `needed` is written even when the call fails with
    // `ERROR_INSUFFICIENT_BUFFER`.
    let _ = unsafe { GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut needed) };
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buffer = vec![0u8; needed as usize];
    // SAFETY: `buffer` is writable storage of `needed` bytes.
    let ok = unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    if (needed as usize) < size_of::<TOKEN_USER>() || (needed as usize) > buffer.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "token user information is truncated",
        ));
    }
    // SAFETY: `GetTokenInformation` filled a `TOKEN_USER` at `buffer`.
    let user = unsafe { buffer.as_ptr().cast::<TOKEN_USER>().read_unaligned() };
    let mut sid_text: windows_sys::core::PWSTR = null_mut();
    // SAFETY: `user.User.Sid` aliases `buffer`; `sid_text` is written on
    // success and owned by `LocalFree`.
    let ok = unsafe { ConvertSidToStringSidW(user.User.Sid, &mut sid_text) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    struct SidText(*mut u16);
    impl Drop for SidText {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: `ConvertSidToStringSidW` allocated this string.
                let _ = unsafe { LocalFree(self.0.cast()) };
            }
        }
    }
    let sid_text = SidText(sid_text);
    let mut sid_len = 0usize;
    // SAFETY: `sid_text` is a live NUL-terminated UTF-16 allocation.
    unsafe {
        while *sid_text.0.add(sid_len) != 0 {
            sid_len += 1;
        }
    }
    let sid = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(sid_text.0, sid_len) });
    // Protected DACL: current user and SYSTEM only. `P` blocks parent
    // inheritance so a shared directory cannot reopen the payload.
    let sddl = format!("D:P(A;;FA;;;{sid})(A;;FA;;;SY)");
    let mut wide: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: `wide` is a live NUL-terminated SDDL string; on success `sd`
    // is an allocation that `PrivateSd` frees.
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide.as_mut_ptr(),
            1, // SDDL_REVISION_1
            &mut sd,
            null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(PrivateSd(sd))
}

/// Opens the host-selected session cwd. The cwd path itself may follow.
///
/// # Errors
///
/// Returns an I/O error when `path` cannot be opened as a directory.
pub(super) fn open_allowed_root(path: &Path) -> io::Result<File> {
    let path = windows_extended_length_path(&strip_verbatim_prefix(path));
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path contains NUL",
        ));
    }
    wide.push(0);
    let mut handle = INVALID_HANDLE_VALUE;
    for access in [FILE_GENERIC_READ | FILE_GENERIC_WRITE, FILE_GENERIC_READ] {
        // SAFETY: `wide` is a live NUL-terminated UTF-16 path.
        handle = unsafe {
            windows_sys::Win32::Storage::FileSystem::CreateFileW(
                wide.as_ptr(),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                null_mut(),
            )
        };
        if handle != INVALID_HANDLE_VALUE {
            break;
        }
    }
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let file = into_file(handle);
    let meta = meta_from_file(&file, false)?;
    if meta.kind != FileKind::Directory {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "session cwd is not a directory",
        ));
    }
    Ok(file)
}

fn open_flags(how: ChildOpen) -> (u32, u32, Option<FileKind>) {
    match how {
        ChildOpen::Directory => (
            FILE_GENERIC_READ | FILE_GENERIC_WRITE,
            FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            Some(FileKind::Directory),
        ),
        ChildOpen::ExistingFile => (
            FILE_GENERIC_READ,
            FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            Some(FileKind::File),
        ),
        ChildOpen::Probe => (
            FILE_GENERIC_READ,
            FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            None,
        ),
    }
}

/// Opens one no-follow child relative to `parent`.
///
/// # Errors
///
/// Returns an I/O error when the name is unsafe, a reparse point is present,
/// the type is wrong, or a volume boundary is crossed.
pub(super) fn open_child(parent: &File, name: &OsStr, how: ChildOpen) -> io::Result<OpenedChild> {
    open_child_named(parent, name, how, true)
}

fn open_child_named(
    parent: &File,
    name: &OsStr,
    how: ChildOpen,
    alias: bool,
) -> io::Result<OpenedChild> {
    let (access, options, expect) = open_flags(how);
    let file = match nt_open(parent, name, access, options, alias) {
        Ok(file) => file,
        Err(error) if access == FILE_GENERIC_READ | FILE_GENERIC_WRITE => {
            nt_open(parent, name, FILE_GENERIC_READ, options, alias).map_err(|_| error)?
        }
        Err(error) => return Err(error),
    };
    enforce_same_volume(parent, &file)?;
    let meta = meta_from_file(&file, true)?;
    if let Some(expect) = expect
        && meta.kind != expect
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "opened object type changed before it was used",
        ));
    }
    Ok(OpenedChild {
        file,
        meta,
        delete_handle: None,
    })
}

/// Creates a directory component or opens it when it already exists.
///
/// # Errors
///
/// Returns an I/O error when the name cannot be created or opened as a
/// directory without following a reparse point.
pub(super) fn ensure_directory(parent: &File, name: &OsStr) -> io::Result<OpenedChild> {
    match nt_create(
        parent,
        name,
        FILE_GENERIC_READ | FILE_GENERIC_WRITE,
        FILE_ATTRIBUTE_NORMAL,
        FILE_CREATE,
        FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
        FILE_SHARE_READ | FILE_SHARE_DELETE,
        null(),
    ) {
        Ok(file) => {
            enforce_same_volume(parent, &file)?;
            let meta = meta_from_file(&file, true)?;
            if meta.kind != FileKind::Directory {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "created object is not a directory",
                ));
            }
            Ok(OpenedChild {
                file,
                meta,
                delete_handle: None,
            })
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            open_child_named(parent, name, ChildOpen::Directory, false)
        }
        Err(error) => Err(error),
    }
}

/// Unique on-disk directory-entry spelling for `want` inside `parent`.
///
/// # Errors
///
/// Zero or several matches fail closed so a case alias or same-directory
/// hardlink pair cannot keep an unproven name.
pub(super) fn unique_component_name(
    parent: &File,
    want: FileIdentity,
    cancel: &CancellationToken,
) -> io::Result<OsString> {
    let mut words = vec![0u64; DIR_LIST_WORDS];
    let mut restart = true;
    let mut found: Option<OsString> = None;
    let mut scanned = 0usize;
    loop {
        check_cancel(cancel)?;
        let byte_len = u32::try_from(words.len().saturating_mul(8)).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "directory listing buffer is too large",
            )
        })?;
        let mut io_status = IO_STATUS_BLOCK::default();
        // SAFETY: `parent` is a live synchronous directory handle; `words`
        // is aligned writable storage; `ReturnSingleEntry` is true.
        let status = unsafe {
            NtQueryDirectoryFile(
                parent.as_raw_handle(),
                null_mut(),
                None,
                null(),
                &mut io_status,
                words.as_mut_ptr().cast(),
                byte_len,
                FileFullDirectoryInformation,
                true,
                null(),
                restart,
            )
        };
        if status == STATUS_NO_MORE_FILES {
            break;
        }
        if status != STATUS_SUCCESS {
            return Err(ntstatus_error(status));
        }
        restart = false;
        let name = parse_full_dir_name(words.as_ptr().cast(), io_status.Information)?;
        if name == "." || name == ".." {
            continue;
        }
        scanned += 1;
        if scanned > MAX_DIR_WIDTH {
            return Err(io::Error::other(
                "directory is too wide to prove unique on-disk component spelling",
            ));
        }
        let opened = match nt_open(
            parent,
            &name,
            FILE_GENERIC_READ,
            FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            false,
        ) {
            Ok(file) => file,
            Err(_) => continue,
        };
        let Ok(meta) = meta_from_file(&opened, true) else {
            continue;
        };
        if meta.identity == want {
            if found.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "multiple directory entries share the opened identity",
                ));
            }
            found = Some(name);
        }
    }
    found.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "opened object has no unique on-disk file name",
        )
    })
}

fn parse_full_dir_name(buffer: *const u8, used: usize) -> io::Result<OsString> {
    const NAME_LEN_OFFSET: usize = 60;
    const NAME_OFFSET: usize = 68;
    if used < NAME_OFFSET {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "directory query returned an invalid byte count",
        ));
    }
    // SAFETY: `used` covers the header; the kernel filled this buffer.
    let name_len = unsafe { buffer.add(NAME_LEN_OFFSET).cast::<u32>().read_unaligned() } as usize;
    if NAME_OFFSET.saturating_add(name_len) > used || !name_len.is_multiple_of(2) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "directory query returned an invalid name length",
        ));
    }
    let units = name_len / 2;
    let mut wide = vec![0u16; units];
    // SAFETY: `name_len` bytes of UTF-16 follow the header inside `used`.
    unsafe {
        std::ptr::copy_nonoverlapping(
            buffer.add(NAME_OFFSET).cast::<u16>(),
            wide.as_mut_ptr(),
            units,
        );
    }
    Ok(OsString::from_wide(&wide))
}

/// Duplicates `file`, keeping all access it was granted (including
/// `DELETE`).
///
/// A restrictive DACL copied onto the file later can deny a fresh by-name
/// open, but access granted on an already-open handle is never
/// re-evaluated, so the duplicate keeps fail-safe cleanup possible.
///
/// # Errors
///
/// Returns an I/O error when `DuplicateHandle` fails.
fn duplicate_delete_handle(file: &File) -> io::Result<File> {
    let mut target = INVALID_HANDLE_VALUE;
    // SAFETY: `GetCurrentProcess` returns a pseudo-handle that needs no
    // release; `file` stays live for the call; on success `target` holds a
    // fresh handle that `into_file` takes over.
    let ok = unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            file.as_raw_handle(),
            GetCurrentProcess(),
            &mut target,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(into_file(target))
}

/// Clears `FILE_ATTRIBUTE_READONLY` on `file`, preserving other attributes.
///
/// The handle must hold `FILE_WRITE_ATTRIBUTES`; creation-time temp handles
/// do. Zeroed time fields in `FILE_BASIC_INFO` leave timestamps unchanged.
///
/// # Errors
///
/// Returns an I/O error when the attribute query or update fails.
fn clear_readonly_attribute(file: &File) -> io::Result<()> {
    let info = query_handle(file)?;
    let mut basic = FILE_BASIC_INFO {
        CreationTime: 0,
        LastAccessTime: 0,
        LastWriteTime: 0,
        ChangeTime: 0,
        FileAttributes: info.attributes & !FILE_ATTRIBUTE_READONLY,
    };
    if basic.FileAttributes == 0 {
        basic.FileAttributes = FILE_ATTRIBUTE_NORMAL;
    }
    let size = u32::try_from(size_of::<FILE_BASIC_INFO>()).expect("FILE_BASIC_INFO fits in u32");
    // SAFETY: `file` owns the handle; `basic` is live `FILE_BASIC_INFO`
    // storage of `size` bytes.
    let ok = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileBasicInfo,
            (&raw mut basic).cast(),
            size,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Requests the legacy on-close deletion disposition on `file`.
///
/// # Errors
///
/// Returns an I/O error when the disposition request fails (for example
/// `STATUS_CANNOT_DELETE` while `FILE_ATTRIBUTE_READONLY` is set).
fn set_legacy_delete(file: &File) -> io::Result<()> {
    let mut info = FILE_DISPOSITION_INFORMATION { DeleteFile: true };
    let mut io_status = IO_STATUS_BLOCK::default();
    let size = u32::try_from(size_of::<FILE_DISPOSITION_INFORMATION>())
        .expect("FILE_DISPOSITION_INFORMATION fits in u32");
    // SAFETY: `file` owns the handle; `info` is live disposition storage.
    let status = unsafe {
        NtSetInformationFile(
            file.as_raw_handle(),
            &mut io_status,
            (&raw mut info).cast(),
            size,
            FileDispositionInformation,
        )
    };
    if status < 0 {
        return Err(ntstatus_error(status));
    }
    Ok(())
}

/// Marks `file` for deletion on a retained creation-time handle.
///
/// Used by the temp guard on a handle that already holds `DELETE`, so a
/// restrictive DACL copied from the source cannot block cleanup after a
/// failed publish. The temp may also carry `FILE_ATTRIBUTE_READONLY`
/// copied from a read-only source, which the legacy disposition refuses
/// with `STATUS_CANNOT_DELETE`; a POSIX-semantics disposition deletes
/// read-only files immediately, and systems without that information class
/// fall back to clearing the read-only attribute on this handle and
/// retrying the legacy disposition.
///
/// # Errors
///
/// Returns an I/O error when the POSIX disposition and the clear-attribute
/// fallback both fail.
pub(super) fn mark_delete(file: &File) -> io::Result<()> {
    #[cfg(test)]
    if let Some(error) = super::delete_fault(file) {
        return Err(error);
    }
    let mut posix = FILE_DISPOSITION_INFORMATION_EX {
        Flags: FILE_DISPOSITION_DELETE
            | FILE_DISPOSITION_POSIX_SEMANTICS
            | FILE_DISPOSITION_IGNORE_READONLY_ATTRIBUTE,
    };
    let mut io_status = IO_STATUS_BLOCK::default();
    let size = u32::try_from(size_of::<FILE_DISPOSITION_INFORMATION_EX>())
        .expect("FILE_DISPOSITION_INFORMATION_EX fits in u32");
    // SAFETY: `file` owns the handle; `posix` is live disposition storage.
    let status = unsafe {
        NtSetInformationFile(
            file.as_raw_handle(),
            &mut io_status,
            (&raw mut posix).cast(),
            size,
            FileDispositionInformationEx,
        )
    };
    if status >= 0 {
        return Ok(());
    }
    clear_readonly_attribute(file)?;
    set_legacy_delete(file)
}

/// Requests a checked deletion of a just-created temp file unless its
/// ownership transfers to the caller.
///
/// The guard is armed on the original creation handle — which already
/// holds `DELETE` and `FILE_WRITE_ATTRIBUTES` — from the first instant the
/// name exists, so every post-create failure (volume check, stat, type
/// check, `DELETE` duplication) cleans up through a handle whose access a
/// later restrictive DACL cannot revoke.
struct CreatedTempGuard<'a> {
    file: &'a File,
    armed: bool,
}

impl CreatedTempGuard<'_> {
    /// Disarms the guard without deleting; ownership is transferring.
    fn disarm(&mut self) {
        self.armed = false;
    }

    /// Reports `primary`, deleting the created name first when it failed.
    ///
    /// A failed deletion is folded into the returned error, so residue is
    /// never silently presented as a clean failure.
    fn finish<T>(mut self, primary: io::Result<T>) -> io::Result<T> {
        match primary {
            Ok(value) => {
                self.disarm();
                Ok(value)
            }
            Err(error) => match self.delete_marked() {
                Ok(()) => Err(error),
                Err(cleanup) => Err(io::Error::new(
                    error.kind(),
                    format!("{error}; failed to remove rejected temporary file: {cleanup}"),
                )),
            },
        }
    }

    /// Marks the created file for deletion and reports the result.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the disposition request (and its
    /// read-only fallback) fails.
    fn delete_marked(&mut self) -> io::Result<()> {
        if !self.armed {
            return Ok(());
        }
        let result = mark_delete(self.file);
        if result.is_ok() {
            self.armed = false;
        }
        result
    }
}

impl Drop for CreatedTempGuard<'_> {
    fn drop(&mut self) {
        // `finish` reports deletion failures. This is only a panic/unwind
        // fallback, where Drop cannot return another error.
        let _ = self.delete_marked();
    }
}

/// Creates a same-parent exclusive temp file.
///
/// A `DELETE`-capable duplicate handle is returned in
/// `OpenedChild::delete_handle` for the caller's fail-safe guard, so a
/// restrictive DACL copied from the source cannot block cleanup after a
/// failed publish. The delete-on-error guard is armed on the creation
/// handle immediately after `NtCreateFile(FILE_CREATE)` succeeds, before
/// any fallible volume/stat/type/duplicate step can return the linked name
/// to the caller.
///
/// # Errors
///
/// Returns an I/O error when exclusive create fails, a post-create check
/// fails, or the checked cleanup after such a failure fails.
pub(super) fn create_temp(parent: &File, name: &OsStr) -> io::Result<OpenedChild> {
    create_temp_with(
        parent,
        name,
        |file| {
            enforce_same_volume(parent, file)?;
            meta_from_file(file, true)
        },
        duplicate_delete_handle,
    )
}

/// Testable core of [`create_temp`] with injected post-create checks and
/// `DELETE` duplication so early-error cleanup can be fault-tested.
fn create_temp_with<C, D>(
    parent: &File,
    name: &OsStr,
    checks: C,
    duplicate: D,
) -> io::Result<OpenedChild>
where
    C: FnOnce(&File) -> io::Result<FileMeta>,
    D: FnOnce(&File) -> io::Result<File>,
{
    let security = private_temp_descriptor()?;
    let file = nt_create(
        parent,
        name,
        FILE_GENERIC_WRITE | FILE_GENERIC_READ | WRITE_DAC | WRITE_OWNER | DELETE,
        FILE_ATTRIBUTE_NORMAL,
        FILE_CREATE,
        FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
        // No `FILE_SHARE_READ`: a foreign reader cannot open the payload
        // while this handle (or its DELETE duplicate) remains.
        FILE_SHARE_DELETE,
        security.as_ptr(),
    )?;
    drop(security);
    #[cfg(test)]
    super::note_delete_fault_temp(parent, &file);
    // Armed before any fallible post-create step runs.
    let mut guard = CreatedTempGuard {
        file: &file,
        armed: true,
    };
    let checked = (|| {
        let meta = checks(&file)?;
        if meta.kind != FileKind::File {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "temporary file is not a regular file",
            ));
        }
        let delete_handle = duplicate(&file)?;
        Ok((meta, delete_handle))
    })();
    match checked {
        Ok((meta, delete_handle)) => {
            // Ownership transfers into the caller's guard; no deletion is
            // requested.
            guard.disarm();
            drop(guard);
            Ok(OpenedChild {
                file,
                meta,
                delete_handle: Some(delete_handle),
            })
        }
        Err(primary) => guard.finish(Err(primary)),
    }
}

/// Creates a never-written probe that inherits the parent's default DACL.
///
/// The probe is used only to record the security a normal create in this
/// directory would receive. It is never written and is unlinked after the
/// payload is published. Exclusive create is cleaned up on any post-create
/// failure through the creation handle.
///
/// # Errors
///
/// Returns an I/O error when exclusive create, a type/volume check, or
/// mandatory cleanup fails.
pub(super) fn create_security_probe(parent: &File, name: &OsStr) -> io::Result<File> {
    let file = nt_create(
        parent,
        name,
        FILE_GENERIC_READ | DELETE,
        FILE_ATTRIBUTE_NORMAL,
        FILE_CREATE,
        FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
        FILE_SHARE_READ | FILE_SHARE_DELETE,
        null(),
    )?;
    let mut guard = CreatedTempGuard {
        file: &file,
        armed: true,
    };
    let checked = (|| {
        enforce_same_volume(parent, &file)?;
        let meta = meta_from_file(&file, true)?;
        if meta.kind != FileKind::File {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "temporary file is not a regular file",
            ));
        }
        Ok(())
    })();
    match checked {
        Ok(()) => {
            guard.disarm();
            drop(guard);
            Ok(file)
        }
        Err(primary) => guard.finish(Err(primary)),
    }
}

/// Writes `bytes` in chunks, then flushes and synchronizes `file`.
///
/// # Errors
///
/// Returns an I/O error on write, flush, sync, or cancellation.
pub(super) fn write_all_sync(
    file: &mut File,
    bytes: &[u8],
    cancel: &CancellationToken,
) -> io::Result<()> {
    let mut offset = 0usize;
    while offset < bytes.len() {
        check_cancel(cancel)?;
        let end = (offset + WRITE_CHUNK).min(bytes.len());
        file.write_all(&bytes[offset..end])?;
        offset = end;
    }
    file.flush()?;
    sync_file(file)
}

/// Reads `file` up to `declared_size` and fails if the stream disagrees.
///
/// # Errors
///
/// Returns an I/O error when the read is cancelled, exceeds `max_bytes`, or
/// does not match `declared_size`.
pub(super) fn read_exact_capped(
    file: &mut File,
    declared_size: u64,
    max_bytes: u64,
    cancel: &CancellationToken,
) -> io::Result<Vec<u8>> {
    if declared_size > max_bytes {
        return Err(io::Error::other("file exceeds the read size limit"));
    }
    let expected = usize::try_from(declared_size)
        .map_err(|_| io::Error::other("file exceeds the read size limit"))?;
    let mut out = Vec::new();
    let mut buf = [0u8; WRITE_CHUNK];
    loop {
        check_cancel(cancel)?;
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        let next = out.len().saturating_add(read);
        if next as u64 > declared_size || next as u64 > max_bytes {
            return Err(io::Error::other(
                "file grew or exceeded the read size limit",
            ));
        }
        out.extend_from_slice(&buf[..read]);
    }
    if out.len() != expected {
        return Err(io::Error::other(
            "file size changed during read or did not match metadata",
        ));
    }
    Ok(out)
}

pub(super) fn current_meta(file: &File) -> io::Result<FileMeta> {
    meta_from_file(file, true)
}

/// Copies owner, group, DACL, and file attributes from `src` onto `dst`.
///
/// SACL and integrity labels are not claimed: requesting them requires
/// privileges this process may not have. A protected source DACL keeps its
/// protected control so the publish cannot silently re-enable inheritance.
/// DACL copy failure fails closed.
///
/// # Errors
///
/// Returns an I/O error when owner, group, DACL, or attributes cannot be
/// preserved.
pub(super) fn copy_safe_mode(src_meta: &FileMeta, src: &File, dst: &File) -> io::Result<()> {
    copy_security_info(
        src,
        dst,
        OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
    )?;
    let mut basic = FILE_BASIC_INFO {
        CreationTime: 0,
        LastAccessTime: 0,
        LastWriteTime: 0,
        ChangeTime: 0,
        FileAttributes: src_meta.windows_attributes
            & !FILE_ATTRIBUTE_DIRECTORY
            & !FILE_ATTRIBUTE_REPARSE_POINT,
    };
    if basic.FileAttributes == 0 {
        basic.FileAttributes = FILE_ATTRIBUTE_NORMAL;
    }
    let size = u32::try_from(size_of::<FILE_BASIC_INFO>()).expect("FILE_BASIC_INFO fits in u32");
    // SAFETY: `basic` is live `FILE_BASIC_INFO` storage of `size` bytes.
    let ok = unsafe {
        SetFileInformationByHandle(
            dst.as_raw_handle(),
            FileBasicInfo,
            (&raw mut basic).cast(),
            size,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn copy_security_info(src: &File, dst: &File, mut info: u32) -> io::Result<()> {
    let mut owner = null_mut();
    let mut group = null_mut();
    let mut dacl = null_mut();
    let mut sacl = null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: output pointers are writable; a successful call allocates `sd`
    // which `LocalFree` owns.
    let status = unsafe {
        GetSecurityInfo(
            src.as_raw_handle(),
            SE_FILE_OBJECT,
            info,
            &mut owner,
            &mut group,
            &mut dacl,
            &mut sacl,
            &mut sd,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(win32_error(status));
    }
    struct SdGuard(PSECURITY_DESCRIPTOR);
    impl Drop for SdGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: `GetSecurityInfo` allocated this descriptor.
                let _ = unsafe { LocalFree(self.0.cast()) };
            }
        }
    }
    let _guard = SdGuard(sd);
    // Mirror the source DACL's protected control bit: `SetSecurityInfo` only
    // honors it through the `PROTECTED_DACL_SECURITY_INFORMATION` flag, not
    // through the descriptor's control word.
    let mut control: SECURITY_DESCRIPTOR_CONTROL = 0;
    let mut revision = 0u32;
    // SAFETY: `sd` is alive under `_guard`; both outputs are writable.
    let ok = unsafe { GetSecurityDescriptorControl(sd, &mut control, &mut revision) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    if control & SE_DACL_PROTECTED != 0 {
        info |= PROTECTED_DACL_SECURITY_INFORMATION;
    } else {
        // The payload temp is created with a protected owner-only DACL.
        // Copying an inheriting source without this flag would freeze the
        // destination as protected.
        info |= UNPROTECTED_DACL_SECURITY_INFORMATION;
    }
    // SAFETY: `sd` remains alive in `_guard`; SID/ACL pointers alias it.
    let status = unsafe {
        SetSecurityInfo(
            dst.as_raw_handle(),
            SE_FILE_OBJECT,
            info,
            owner,
            group,
            dacl,
            null(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(win32_error(status));
    }
    Ok(())
}

pub(super) fn unlink_child(parent: &File, name: &OsStr) -> io::Result<()> {
    let file = nt_open(
        parent,
        name,
        FILE_GENERIC_WRITE | DELETE,
        FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
        false,
    )?;
    set_legacy_delete(&file)
}

fn rename_child(parent: &File, file: &File, dest_name: &OsStr, replace: bool) -> io::Result<()> {
    let wide = encode_component(dest_name)?;
    let name_bytes = wide.len().saturating_mul(2);
    let header = size_of::<FILE_RENAME_INFORMATION>();
    let extra = name_bytes.saturating_sub(2);
    let total = header.saturating_add(extra);
    let words = total.div_ceil(8).max(1);
    let mut buf = vec![0u64; words];
    let info = buf.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
    let name_len = u32::try_from(name_bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "rename name is too long"))?;
    // SAFETY: `buf` is aligned and large enough for the header plus name.
    unsafe {
        (*info).Anonymous.Flags = u32::from(replace);
        // Same-directory publish: NULL RootDirectory plus a single component.
        // A parent handle here can share-lock the source on some NTFS paths.
        let _ = parent;
        (*info).RootDirectory = std::ptr::null_mut();
        (*info).FileNameLength = name_len;
        std::ptr::copy_nonoverlapping(wide.as_ptr(), (*info).FileName.as_mut_ptr(), wide.len());
    }
    let length = u32::try_from(total)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "rename buffer is too large"))?;
    let mut io_status = IO_STATUS_BLOCK::default();
    // SAFETY: `file` and `parent` stay live; `buf` holds the rename info.
    let status = unsafe {
        NtSetInformationFile(
            file.as_raw_handle(),
            &mut io_status,
            buf.as_ptr().cast(),
            length,
            FileRenameInformation,
        )
    };
    if status < 0 {
        return Err(ntstatus_error(status));
    }
    Ok(())
}

/// Publishes `temp_name` over `dest_name` in `parent` (existing target).
///
/// # Errors
///
/// Returns an I/O error when relative `FileRenameInformation` fails.
pub(super) fn publish_replace(
    parent: &File,
    temp: &File,
    _temp_name: &OsStr,
    dest_name: &OsStr,
) -> io::Result<()> {
    rename_child(parent, temp, dest_name, true)
}

/// Publishes `temp_name` as a new `dest_name` and fails if it exists.
///
/// # Errors
///
/// Returns an I/O error when the destination already exists or rename fails.
pub(super) fn publish_create_only(
    parent: &File,
    temp: &File,
    _temp_name: &OsStr,
    dest_name: &OsStr,
) -> io::Result<()> {
    rename_child(parent, temp, dest_name, false)
}

pub(super) fn sync_file(file: &File) -> io::Result<()> {
    // SAFETY: `file` is a live handle.
    let ok = unsafe { FlushFileBuffers(file.as_raw_handle()) };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub(super) fn sync_parent(dir: &File) -> io::Result<()> {
    sync_file(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn injected_failure(stage: &str) -> io::Error {
        io::Error::other(format!("injected {stage} failure"))
    }

    fn assert_name_absent(dir: &tempfile::TempDir, name: &str) {
        assert!(
            !dir.path().join(name).exists(),
            "rejected temp name must be deleted"
        );
    }

    #[test]
    fn temp_volume_failure_deletes_created_name() {
        let dir = tempfile::tempdir().unwrap();
        let parent = open_allowed_root(dir.path()).unwrap();
        let name = OsStr::new("mcode-write-volume.tmp");
        let error = create_temp_with(
            &parent,
            name,
            |_| Err(injected_failure("volume")),
            duplicate_delete_handle,
        )
        .err()
        .expect("injected volume failure must be returned");
        assert!(error.to_string().contains("injected volume failure"));
        assert_name_absent(&dir, "mcode-write-volume.tmp");
    }

    #[test]
    fn temp_stat_failure_deletes_created_name() {
        let dir = tempfile::tempdir().unwrap();
        let parent = open_allowed_root(dir.path()).unwrap();
        let name = OsStr::new("mcode-write-stat.tmp");
        let error = create_temp_with(
            &parent,
            name,
            |_| Err(injected_failure("stat")),
            duplicate_delete_handle,
        )
        .err()
        .expect("injected stat failure must be returned");
        assert!(error.to_string().contains("injected stat failure"));
        assert_name_absent(&dir, "mcode-write-stat.tmp");
    }

    #[test]
    fn temp_type_failure_deletes_created_name() {
        let dir = tempfile::tempdir().unwrap();
        let parent = open_allowed_root(dir.path()).unwrap();
        let name = OsStr::new("mcode-write-type.tmp");
        let error = create_temp_with(
            &parent,
            name,
            |file| {
                let mut meta = meta_from_file(file, true)?;
                meta.kind = FileKind::Directory;
                Ok(meta)
            },
            duplicate_delete_handle,
        )
        .err()
        .expect("a non-file temp must be rejected");
        assert!(
            error
                .to_string()
                .contains("temporary file is not a regular file"),
            "{error}"
        );
        assert_name_absent(&dir, "mcode-write-type.tmp");
    }

    #[test]
    fn temp_duplicate_failure_deletes_created_name() {
        let dir = tempfile::tempdir().unwrap();
        let parent = open_allowed_root(dir.path()).unwrap();
        let name = OsStr::new("mcode-write-dup.tmp");
        let error = create_temp_with(
            &parent,
            name,
            |file| meta_from_file(file, true),
            |_| Err(injected_failure("duplicate")),
        )
        .err()
        .expect("injected duplicate failure must be returned");
        assert!(error.to_string().contains("injected duplicate failure"));
        // Cleanup must run on the creation handle (never a by-name reopen)
        // and must actually remove the temp.
        assert_name_absent(&dir, "mcode-write-dup.tmp");
    }

    #[test]
    fn temp_stat_failure_reports_cleanup_error() {
        let dir = tempfile::tempdir().unwrap();
        let parent = open_allowed_root(dir.path()).unwrap();
        let name = OsStr::new("mcode-write-stat-cleanup.tmp");
        let fault = crate::builtin::fs_io::install_delete_fault_under(dir.path())
            .expect("delete fault fixture must install");
        let error = create_temp_with(
            &parent,
            name,
            |_| Err(injected_failure("stat")),
            duplicate_delete_handle,
        )
        .err()
        .expect("injected stat failure must be returned");
        assert!(
            error.to_string().contains("injected stat failure"),
            "{error}"
        );
        assert!(
            error.to_string().contains("injected mcode delete failure"),
            "cleanup failure must be folded into the returned error: {error}"
        );
        assert!(
            dir.path().join("mcode-write-stat-cleanup.tmp").exists(),
            "faulted cleanup must leave documented residue"
        );
        drop(fault);
        std::fs::remove_file(dir.path().join("mcode-write-stat-cleanup.tmp")).ok();
    }
}
