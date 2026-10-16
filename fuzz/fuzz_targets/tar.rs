#![no_main]

use arkforge_artifact::tar::TarReader;
use libfuzzer_sys::fuzz_target;
use std::io::Cursor;

fuzz_target!(|data: &[u8]| {
    let mut reader = TarReader::new(Cursor::new(data));
    // Metadata headers are consumed inside next_member. Cap the walk so a
    // crafted chain cannot turn a short input into an unbounded loop.
    for _ in 0..32 {
        match reader.next_member() {
            Ok(Some(header)) => {
                let _ = reader.read_member_body(&header, |_| {});
            }
            _ => break,
        }
    }
});
