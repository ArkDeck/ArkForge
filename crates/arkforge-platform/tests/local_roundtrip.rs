use arkforge_platform::{
    LocalChannel, LocalEndpoint, LocalListener, LocalStream, fill_random, replace_file,
    volume_available_bytes,
};
use std::io::{Read, Write};
use std::path::PathBuf;

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(label: &str) -> Self {
        #[cfg(target_os = "macos")]
        let temporary_base = PathBuf::from("/private/tmp");
        #[cfg(not(target_os = "macos"))]
        let temporary_base = std::env::temp_dir();
        let root = temporary_base.join(format!(
            "arkforge-platform-{label}-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Self(root)
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn the_local_channel_round_trips_bytes() {
    let root = TempRoot::new("roundtrip");
    let endpoint = LocalEndpoint::for_runtime(&root.0, LocalChannel::Public);
    let mut listener = LocalListener::bind(&endpoint).unwrap();
    // A successful bind must publish the endpoint before accept is called.
    // This order makes the Windows named-pipe listener contract deterministic
    // instead of depending on which side wins a thread-scheduling race.
    let mut client = LocalStream::connect(&endpoint).unwrap();
    let mut server = listener.accept().unwrap();
    let server = std::thread::spawn(move || {
        let mut request = [0u8; 4];
        server.read_exact(&mut request).unwrap();
        assert_eq!(&request, b"ping");
        server.write_all(b"pong").unwrap();
        // A Windows server-side FlushFileBuffers waits for the client to read
        // buffered bytes, so keep the two connected ends concurrent.
        server.flush().unwrap();
    });
    client.write_all(b"ping").unwrap();
    client.flush().unwrap();
    let mut reply = [0u8; 4];
    client.read_exact(&mut reply).unwrap();
    assert_eq!(&reply, b"pong");
    server.join().unwrap();
}

#[test]
fn random_and_replace_are_real_host_primitives() {
    let root = TempRoot::new("storage");
    let mut first = [0u8; 32];
    let mut second = [0u8; 32];
    fill_random(&mut first).unwrap();
    fill_random(&mut second).unwrap();
    assert_ne!(first, second);

    let target = root.0.join("target");
    let replacement = root.0.join("replacement");
    std::fs::write(&target, b"old").unwrap();
    std::fs::write(&replacement, b"new").unwrap();
    replace_file(&replacement, &target).unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"new");
    assert!(!replacement.exists());
    assert!(volume_available_bytes(&root.0).unwrap() > 0);
}
