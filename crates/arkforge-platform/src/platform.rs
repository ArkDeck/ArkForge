//! Win32 named-pipe, ACL and randomness binding.

use core::ffi::c_void;
use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr;

type Handle = *mut c_void;
type Bool = i32;
type Dword = u32;

#[repr(C)]
#[derive(Clone, Copy)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

const INVALID_HANDLE_VALUE: Handle = -1isize as Handle;
const GENERIC_READ: Dword = 0x8000_0000;
const GENERIC_WRITE: Dword = 0x4000_0000;
const OPEN_EXISTING: Dword = 3;
const PIPE_ACCESS_DUPLEX: Dword = 0x0000_0003;
const FILE_FLAG_FIRST_PIPE_INSTANCE: Dword = 0x0008_0000;
const PIPE_TYPE_BYTE: Dword = 0;
const PIPE_READMODE_BYTE: Dword = 0;
const PIPE_WAIT: Dword = 0;
const PIPE_NOWAIT: Dword = 0x0000_0001;
const PIPE_REJECT_REMOTE_CLIENTS: Dword = 0x0000_0008;
const PIPE_UNLIMITED_INSTANCES: Dword = 255;
const ERROR_PIPE_BUSY: Dword = 231;
const ERROR_PIPE_CONNECTED: Dword = 535;
const ERROR_PIPE_LISTENING: Dword = 536;
const SECURITY_SQOS_PRESENT: Dword = 0x0010_0000;
const SECURITY_IDENTIFICATION: Dword = 0x0001_0000;
const DUPLICATE_SAME_ACCESS: Dword = 2;
const TOKEN_QUERY: Dword = 0x0008;
const TOKEN_USER: Dword = 1;
const DACL_SECURITY_INFORMATION: Dword = 0x0000_0004;
const PROTECTED_DACL_SECURITY_INFORMATION: Dword = 0x8000_0000;
const SDDL_REVISION_1: Dword = 1;
const BCRYPT_USE_SYSTEM_PREFERRED_RNG: Dword = 0x0000_0002;
const PIPE_BUFFER_BYTES: Dword = 64 * 1024;
const WTD_UI_NONE: Dword = 2;
const WTD_REVOKE_NONE: Dword = 0;
const WTD_CHOICE_FILE: Dword = 1;
const WTD_STATEACTION_IGNORE: Dword = 0;
const WTD_CACHE_ONLY_URL_RETRIEVAL: Dword = 0x0000_1000;
const MOVEFILE_REPLACE_EXISTING: Dword = 0x0000_0001;
const MOVEFILE_WRITE_THROUGH: Dword = 0x0000_0008;

const WINTRUST_ACTION_GENERIC_VERIFY_V2: Guid = Guid {
    data1: 0x00aa_c56b,
    data2: 0xcd44,
    data3: 0x11d0,
    data4: [0x8c, 0xc2, 0x00, 0xc0, 0x4f, 0xc2, 0x95, 0xee],
};

#[repr(C)]
struct SecurityAttributes {
    length: Dword,
    security_descriptor: *mut c_void,
    inherit_handle: Bool,
}

#[repr(C)]
struct TokenUser {
    sid: *mut c_void,
    attributes: Dword,
}

#[repr(C)]
struct WinTrustFileInfo {
    size: Dword,
    file_path: *const u16,
    file: Handle,
    known_subject: *const Guid,
}

#[repr(C)]
struct WinTrustData {
    size: Dword,
    policy_callback_data: *mut c_void,
    sip_client_data: *mut c_void,
    ui_choice: Dword,
    revocation_checks: Dword,
    union_choice: Dword,
    file_info: *mut WinTrustFileInfo,
    state_action: Dword,
    state_data: Handle,
    url_reference: *const u16,
    provider_flags: Dword,
    ui_context: Dword,
    signature_settings: *mut c_void,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateNamedPipeW(
        name: *const u16,
        open_mode: Dword,
        pipe_mode: Dword,
        max_instances: Dword,
        out_buffer_size: Dword,
        in_buffer_size: Dword,
        default_timeout: Dword,
        security_attributes: *mut SecurityAttributes,
    ) -> Handle;
    fn ConnectNamedPipe(pipe: Handle, overlapped: *mut c_void) -> Bool;
    fn DisconnectNamedPipe(pipe: Handle) -> Bool;
    fn SetNamedPipeHandleState(
        pipe: Handle,
        mode: *mut Dword,
        max_collection_count: *mut Dword,
        collect_data_timeout: *mut Dword,
    ) -> Bool;
    fn WaitNamedPipeW(name: *const u16, timeout: Dword) -> Bool;
    fn CreateFileW(
        name: *const u16,
        desired_access: Dword,
        share_mode: Dword,
        security_attributes: *mut c_void,
        creation_disposition: Dword,
        flags_and_attributes: Dword,
        template: Handle,
    ) -> Handle;
    fn ReadFile(
        file: Handle,
        buffer: *mut c_void,
        bytes_to_read: Dword,
        bytes_read: *mut Dword,
        overlapped: *mut c_void,
    ) -> Bool;
    fn WriteFile(
        file: Handle,
        buffer: *const c_void,
        bytes_to_write: Dword,
        bytes_written: *mut Dword,
        overlapped: *mut c_void,
    ) -> Bool;
    fn FlushFileBuffers(file: Handle) -> Bool;
    fn CloseHandle(handle: Handle) -> Bool;
    fn GetLastError() -> Dword;
    fn GetCurrentProcess() -> Handle;
    fn DuplicateHandle(
        source_process: Handle,
        source_handle: Handle,
        target_process: Handle,
        target_handle: *mut Handle,
        desired_access: Dword,
        inherit_handle: Bool,
        options: Dword,
    ) -> Bool;
    fn LocalFree(memory: *mut c_void) -> *mut c_void;
    fn MoveFileExW(existing: *const u16, replacement: *const u16, flags: Dword) -> Bool;
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn OpenProcessToken(process: Handle, desired_access: Dword, token: *mut Handle) -> Bool;
    fn GetTokenInformation(
        token: Handle,
        information_class: Dword,
        information: *mut c_void,
        information_length: Dword,
        return_length: *mut Dword,
    ) -> Bool;
    fn ConvertSidToStringSidW(sid: *mut c_void, string_sid: *mut *mut u16) -> Bool;
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        string: *const u16,
        revision: Dword,
        descriptor: *mut *mut c_void,
        descriptor_size: *mut Dword,
    ) -> Bool;
    fn SetFileSecurityW(
        file_name: *const u16,
        security_information: Dword,
        descriptor: *mut c_void,
    ) -> Bool;
}

#[link(name = "bcrypt")]
unsafe extern "system" {
    fn BCryptGenRandom(
        algorithm: Handle,
        buffer: *mut u8,
        buffer_length: Dword,
        flags: Dword,
    ) -> i32;
}

#[link(name = "wintrust")]
unsafe extern "system" {
    fn WinVerifyTrust(window: Handle, action: *const Guid, data: *mut WinTrustData) -> i32;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    name: String,
}

impl Endpoint {
    pub fn display(&self) -> impl std::fmt::Display + '_ {
        &self.name
    }
}

pub struct Stream {
    handle: Handle,
    server_end: bool,
}

unsafe impl Send for Stream {}

impl Stream {
    pub fn try_clone(&self) -> io::Result<Self> {
        let process = unsafe { GetCurrentProcess() };
        let mut duplicate = INVALID_HANDLE_VALUE;
        if unsafe {
            DuplicateHandle(
                process,
                self.handle,
                process,
                &mut duplicate,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        } == 0
        {
            return Err(last_error());
        }
        Ok(Self {
            handle: duplicate,
            // Only one duplicate should disconnect the server instance.
            server_end: false,
        })
    }
}

impl Read for Stream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = Dword::try_from(buffer.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "read exceeds Win32 DWORD"))?;
        let mut actual = 0;
        if unsafe {
            ReadFile(
                self.handle,
                buffer.as_mut_ptr().cast(),
                count,
                &mut actual,
                ptr::null_mut(),
            )
        } == 0
        {
            let error = last_error();
            if matches!(error.raw_os_error(), Some(109 | 232 | 233)) {
                return Ok(0);
            }
            return Err(error);
        }
        Ok(actual as usize)
    }
}

impl Write for Stream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let count = Dword::try_from(buffer.len()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "write exceeds Win32 DWORD")
        })?;
        let mut actual = 0;
        if unsafe {
            WriteFile(
                self.handle,
                buffer.as_ptr().cast(),
                count,
                &mut actual,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(last_error());
        }
        Ok(actual as usize)
    }

    fn flush(&mut self) -> io::Result<()> {
        if unsafe { FlushFileBuffers(self.handle) } == 0 {
            Err(last_error())
        } else {
            Ok(())
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        if self.server_end {
            let _ = unsafe { DisconnectNamedPipe(self.handle) };
        }
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

pub struct Listener {
    endpoint: Endpoint,
    first_instance: bool,
    nonblocking: bool,
}

unsafe impl Send for Listener {}

impl Listener {
    pub fn set_nonblocking(&mut self, nonblocking: bool) -> io::Result<()> {
        self.nonblocking = nonblocking;
        Ok(())
    }

    pub fn accept(&mut self) -> io::Result<Stream> {
        let name = wide(&self.endpoint.name);
        let descriptor = SecurityDescriptor::current_user("GA", false)?;
        let mut attributes = SecurityAttributes {
            length: std::mem::size_of::<SecurityAttributes>() as Dword,
            security_descriptor: descriptor.raw,
            inherit_handle: 0,
        };
        let mut open_mode = PIPE_ACCESS_DUPLEX;
        if self.first_instance {
            open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
        }
        let wait_mode = if self.nonblocking {
            PIPE_NOWAIT
        } else {
            PIPE_WAIT
        };
        let handle = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                open_mode,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | wait_mode | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                PIPE_BUFFER_BYTES,
                PIPE_BUFFER_BYTES,
                0,
                &mut attributes,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(last_error());
        }
        let connected = unsafe { ConnectNamedPipe(handle, ptr::null_mut()) };
        let connect_error = unsafe { GetLastError() };
        if connected == 0 && connect_error != ERROR_PIPE_CONNECTED {
            let _ = unsafe { CloseHandle(handle) };
            if self.nonblocking && connect_error == ERROR_PIPE_LISTENING {
                return Err(io::Error::from(io::ErrorKind::WouldBlock));
            }
            let error = io::Error::from_raw_os_error(connect_error as i32);
            return Err(error);
        }
        if self.nonblocking {
            let mut blocking_mode = PIPE_READMODE_BYTE | PIPE_WAIT;
            if unsafe {
                SetNamedPipeHandleState(
                    handle,
                    &mut blocking_mode,
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            } == 0
            {
                let error = last_error();
                let _ = unsafe { DisconnectNamedPipe(handle) };
                let _ = unsafe { CloseHandle(handle) };
                return Err(error);
            }
        }
        self.first_instance = false;
        Ok(Stream {
            handle,
            server_end: true,
        })
    }
}

pub fn endpoint(runtime_dir: &Path, channel: &str) -> Endpoint {
    let canonical = runtime_dir
        .canonicalize()
        .unwrap_or_else(|_| runtime_dir.to_path_buf());
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for unit in canonical.as_os_str().encode_wide() {
        hash ^= u64::from(unit);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    Endpoint {
        name: format!(r"\\.\pipe\LOCAL\arkforge-{hash:016x}-{channel}"),
    }
}

pub fn connect(endpoint: &Endpoint) -> io::Result<Stream> {
    let name = wide(&endpoint.name);
    loop {
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                ptr::null_mut(),
                OPEN_EXISTING,
                SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                ptr::null_mut(),
            )
        };
        if handle != INVALID_HANDLE_VALUE {
            return Ok(Stream {
                handle,
                server_end: false,
            });
        }
        let error = unsafe { GetLastError() };
        if error != ERROR_PIPE_BUSY || unsafe { WaitNamedPipeW(name.as_ptr(), 250) } == 0 {
            return Err(io::Error::from_raw_os_error(error as i32));
        }
    }
}

pub fn bind(endpoint: &Endpoint) -> io::Result<Listener> {
    // The first actual instance uses FILE_FLAG_FIRST_PIPE_INSTANCE so a
    // squatting process cannot pre-create the authority endpoint.
    Ok(Listener {
        endpoint: endpoint.clone(),
        first_instance: true,
        nonblocking: false,
    })
}

pub fn protect_path(path: &Path, directory: bool) -> io::Result<()> {
    // Protected directory ACLs must carry inheritable object/container ACEs;
    // otherwise newly-created CAS and journal children could fall back to the
    // ambient directory ACL.
    let sddl = SecurityDescriptor::current_user("FA", directory)?;
    let path = wide(path.as_os_str());
    if unsafe {
        SetFileSecurityW(
            path.as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            sddl.raw,
        )
    } == 0
    {
        Err(last_error())
    } else {
        Ok(())
    }
}

pub fn verify_trusted_signature(path: &Path) -> io::Result<()> {
    let path = wide(path.as_os_str());
    let mut file_info = WinTrustFileInfo {
        size: std::mem::size_of::<WinTrustFileInfo>() as Dword,
        file_path: path.as_ptr(),
        file: ptr::null_mut(),
        known_subject: ptr::null(),
    };
    let mut data = WinTrustData {
        size: std::mem::size_of::<WinTrustData>() as Dword,
        policy_callback_data: ptr::null_mut(),
        sip_client_data: ptr::null_mut(),
        ui_choice: WTD_UI_NONE,
        revocation_checks: WTD_REVOKE_NONE,
        union_choice: WTD_CHOICE_FILE,
        file_info: &mut file_info,
        state_action: WTD_STATEACTION_IGNORE,
        state_data: ptr::null_mut(),
        url_reference: ptr::null(),
        provider_flags: WTD_CACHE_ONLY_URL_RETRIEVAL,
        ui_context: 0,
        signature_settings: ptr::null_mut(),
    };
    let status = unsafe {
        WinVerifyTrust(
            ptr::null_mut(),
            &WINTRUST_ACTION_GENERIC_VERIFY_V2,
            &mut data,
        )
    };
    if status == 0 {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("WinVerifyTrust returned HRESULT 0x{:08x}", status as u32),
        ))
    }
}

pub fn sync_directory(_path: &Path) -> io::Result<()> {
    // Windows exposes durable file flushes, but not a portable fsync operation
    // for a directory handle. Callers still sync the temporary file before an
    // atomic same-volume rename; treating the parent flush as satisfied keeps
    // that protocol executable on NTFS instead of failing on File::open(dir).
    Ok(())
}

pub fn replace_file(source: &Path, target: &Path) -> io::Result<()> {
    let source = wide(source.as_os_str());
    let target = wide(target.as_os_str());
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            target.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        Err(last_error())
    } else {
        Ok(())
    }
}

pub fn fill_random(bytes: &mut [u8]) -> io::Result<()> {
    let length = Dword::try_from(bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "random request is too large"))?;
    let status = unsafe {
        BCryptGenRandom(
            ptr::null_mut(),
            bytes.as_mut_ptr(),
            length,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if status < 0 {
        Err(io::Error::other(format!(
            "BCryptGenRandom returned NTSTATUS 0x{:08x}",
            status as u32
        )))
    } else {
        Ok(())
    }
}

pub fn unix_socket_path(_runtime_dir: &Path, _channel: &str) -> Option<PathBuf> {
    None
}

struct SecurityDescriptor {
    raw: *mut c_void,
}

impl SecurityDescriptor {
    fn current_user(access: &str, inherit_children: bool) -> io::Result<Self> {
        let sid = current_user_sid()?;
        let inheritance = if inherit_children { "OICI" } else { "" };
        let source = wide(OsStr::new(&format!(
            "D:P(A;{inheritance};{access};;;{sid})"
        )));
        let mut descriptor = ptr::null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                source.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(last_error());
        }
        Ok(Self { raw: descriptor })
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            let _ = unsafe { LocalFree(self.raw) };
        }
    }
}

fn current_user_sid() -> io::Result<String> {
    let mut token = INVALID_HANDLE_VALUE;
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(last_error());
    }
    let token = HandleGuard(token);
    let mut required = 0;
    let _ = unsafe { GetTokenInformation(token.0, TOKEN_USER, ptr::null_mut(), 0, &mut required) };
    if required < std::mem::size_of::<TokenUser>() as Dword {
        return Err(last_error());
    }
    let mut buffer = vec![0u8; required as usize];
    if unsafe {
        GetTokenInformation(
            token.0,
            TOKEN_USER,
            buffer.as_mut_ptr().cast(),
            required,
            &mut required,
        )
    } == 0
    {
        return Err(last_error());
    }
    let user = unsafe { &*buffer.as_ptr().cast::<TokenUser>() };
    let mut text: *mut u16 = ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(user.sid, &mut text) } == 0 {
        return Err(last_error());
    }
    let text_guard = LocalGuard(text.cast());
    let length = (0..)
        .find(|index| unsafe { *text.add(*index) } == 0)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "SID is not terminated"))?;
    let result = String::from_utf16(unsafe { std::slice::from_raw_parts(text, length) })
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "SID is not UTF-16"));
    drop(text_guard);
    result
}

struct HandleGuard(Handle);

impl Drop for HandleGuard {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

struct LocalGuard(*mut c_void);

impl Drop for LocalGuard {
    fn drop(&mut self) {
        let _ = unsafe { LocalFree(self.0) };
    }
}

fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain([0]).collect()
}

fn last_error() -> io::Error {
    io::Error::from_raw_os_error(unsafe { GetLastError() } as i32)
}
