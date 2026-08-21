//! Auditable host-platform boundary for local IPC and owner-only storage.
//!
//! Unix uses owner-mode filesystem objects and Unix-domain sockets. Windows
//! uses byte-mode named pipes with remote clients rejected and an explicit
//! DACL containing only the current logon SID. All Win32 declarations and
//! unsafe operations are confined to this crate.

#![deny(unsafe_op_in_unsafe_fn)]

use std::fmt;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalChannel {
    Public,
    Controller,
    Supervisor,
}

impl LocalChannel {
    fn name(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Controller => "controller",
            Self::Supervisor => "supervisor",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalEndpoint {
    inner: platform::Endpoint,
}

impl LocalEndpoint {
    pub fn for_runtime(runtime_dir: &Path, channel: LocalChannel) -> Self {
        Self {
            inner: platform::endpoint(runtime_dir, channel.name()),
        }
    }

    pub fn display(&self) -> impl fmt::Display + '_ {
        self.inner.display()
    }
}

pub struct LocalStream {
    inner: platform::Stream,
}

impl fmt::Debug for LocalStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalStream")
            .finish_non_exhaustive()
    }
}

impl LocalStream {
    pub fn connect(endpoint: &LocalEndpoint) -> std::io::Result<Self> {
        platform::connect(&endpoint.inner).map(|inner| Self { inner })
    }

    pub fn try_clone(&self) -> std::io::Result<Self> {
        self.inner.try_clone().map(|inner| Self { inner })
    }
}

impl Read for LocalStream {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buffer)
    }
}

impl Write for LocalStream {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buffer)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

pub struct LocalListener {
    inner: platform::Listener,
}

impl fmt::Debug for LocalListener {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalListener")
            .finish_non_exhaustive()
    }
}

impl LocalListener {
    pub fn bind(endpoint: &LocalEndpoint) -> std::io::Result<Self> {
        platform::bind(&endpoint.inner).map(|inner| Self { inner })
    }

    pub fn set_nonblocking(&mut self, nonblocking: bool) -> std::io::Result<()> {
        self.inner.set_nonblocking(nonblocking)
    }

    pub fn accept(&mut self) -> std::io::Result<LocalStream> {
        self.inner.accept().map(|inner| LocalStream { inner })
    }
}

/// Applies an owner-only access boundary to an existing filesystem object.
pub fn protect_path(path: &Path, directory: bool) -> std::io::Result<()> {
    platform::protect_path(path, directory)
}

/// Fills bytes from the operating system CSPRNG.
pub fn fill_random(bytes: &mut [u8]) -> std::io::Result<()> {
    platform::fill_random(bytes)
}

/// Verifies that Windows trusts the file's Authenticode signature without
/// allowing the check to trigger network retrieval. Other hosts report that
/// this platform contract is unavailable.
pub fn verify_trusted_signature(path: &Path) -> std::io::Result<()> {
    platform::verify_trusted_signature(path)
}

/// Flushes directory metadata where the host exposes that primitive. Windows
/// file and rename durability is provided by the file handles themselves; the
/// standard Win32 directory handle has no portable `fsync` equivalent.
pub fn sync_directory(path: &Path) -> std::io::Result<()> {
    platform::sync_directory(path)
}

/// Atomically publishes a same-volume temporary file, replacing an older
/// target where the host supports that operation.
pub fn replace_file(source: &Path, target: &Path) -> std::io::Result<()> {
    platform::replace_file(source, target)
}

/// Returns bytes available to the current caller on the volume containing an
/// existing path. Quota-aware host APIs are used rather than total free space.
pub fn volume_available_bytes(path: &Path) -> std::io::Result<u64> {
    platform::volume_available_bytes(path)
}

/// The legacy filesystem name used only on Unix for stale-socket cleanup.
pub fn unix_socket_path(runtime_dir: &Path, channel: LocalChannel) -> Option<PathBuf> {
    platform::unix_socket_path(runtime_dir, channel.name())
}

#[cfg(unix)]
mod platform {
    use std::fs;
    use std::io::{Read, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Endpoint(PathBuf);

    impl Endpoint {
        pub fn display(&self) -> impl std::fmt::Display + '_ {
            self.0.display()
        }
    }

    pub struct Stream(UnixStream);

    impl Stream {
        pub fn try_clone(&self) -> std::io::Result<Self> {
            self.0.try_clone().map(Self)
        }
    }

    impl Read for Stream {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            self.0.read(buffer)
        }
    }

    impl Write for Stream {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.0.write(buffer)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.0.flush()
        }
    }

    pub struct Listener(UnixListener);

    impl Listener {
        pub fn set_nonblocking(&mut self, nonblocking: bool) -> std::io::Result<()> {
            self.0.set_nonblocking(nonblocking)
        }

        pub fn accept(&mut self) -> std::io::Result<Stream> {
            self.0.accept().map(|(stream, _)| Stream(stream))
        }
    }

    pub fn endpoint(runtime_dir: &Path, channel: &str) -> Endpoint {
        Endpoint(runtime_dir.join(format!("{channel}.sock")))
    }

    pub fn connect(endpoint: &Endpoint) -> std::io::Result<Stream> {
        UnixStream::connect(&endpoint.0).map(Stream)
    }

    pub fn bind(endpoint: &Endpoint) -> std::io::Result<Listener> {
        let _ = fs::remove_file(&endpoint.0);
        let listener = UnixListener::bind(&endpoint.0)?;
        fs::set_permissions(&endpoint.0, fs::Permissions::from_mode(0o600))?;
        Ok(Listener(listener))
    }

    pub fn protect_path(path: &Path, directory: bool) -> std::io::Result<()> {
        fs::set_permissions(
            path,
            fs::Permissions::from_mode(if directory { 0o700 } else { 0o600 }),
        )
    }

    pub fn fill_random(bytes: &mut [u8]) -> std::io::Result<()> {
        std::fs::File::open("/dev/urandom")?.read_exact(bytes)
    }

    pub fn verify_trusted_signature(_path: &Path) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Authenticode verification is available only on Windows",
        ))
    }

    pub fn sync_directory(path: &Path) -> std::io::Result<()> {
        std::fs::File::open(path)?.sync_all()
    }

    pub fn replace_file(source: &Path, target: &Path) -> std::io::Result<()> {
        std::fs::rename(source, target)
    }

    pub fn volume_available_bytes(path: &Path) -> std::io::Result<u64> {
        let output = std::process::Command::new("df")
            .arg("-Pk")
            .arg(path)
            .output()?;
        if !output.status.success() {
            return Err(std::io::Error::other("df reported a failure"));
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let line = text
            .lines()
            .nth(1)
            .ok_or_else(|| std::io::Error::other("df produced no data line"))?;
        let available_kb: u64 = line
            .split_whitespace()
            .nth(3)
            .and_then(|field| field.parse().ok())
            .ok_or_else(|| std::io::Error::other("df data line has no available column"))?;
        Ok(available_kb.saturating_mul(1024))
    }

    pub fn unix_socket_path(runtime_dir: &Path, channel: &str) -> Option<PathBuf> {
        Some(runtime_dir.join(format!("{channel}.sock")))
    }
}

#[cfg(windows)]
mod platform;

#[cfg(not(any(unix, windows)))]
compile_error!("ArkForge local IPC supports Unix and Windows hosts only");
