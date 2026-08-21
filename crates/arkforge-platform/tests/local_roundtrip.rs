use arkforge_platform::{
    LocalChannel, LocalEndpoint, LocalListener, LocalStream, fill_random, replace_file,
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
    let server = std::thread::spawn(move || {
        let mut stream = listener.accept().unwrap();
        let mut request = [0u8; 4];
        stream.read_exact(&mut request).unwrap();
        assert_eq!(&request, b"ping");
        stream.write_all(b"pong").unwrap();
        stream.flush().unwrap();
    });
    let mut client = LocalStream::connect(&endpoint).unwrap();
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
}
