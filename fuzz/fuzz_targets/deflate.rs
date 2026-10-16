#![no_main]

use arkforge_artifact::inflate::{GzipReader, InflateReader};
use libfuzzer_sys::fuzz_target;
use std::io::{Cursor, Read};

const READ_BUDGET: usize = 256 * 1024;

fn drain(mut reader: impl Read) {
    let mut output = [0u8; 4096];
    let mut total = 0usize;
    loop {
        match reader.read(&mut output) {
            Ok(0) | Err(_) => break,
            Ok(count) => {
                total = total.saturating_add(count);
                if total >= READ_BUDGET {
                    break;
                }
            }
        }
    }
}

fuzz_target!(|data: &[u8]| {
    drain(InflateReader::new(Cursor::new(data)));
    if let Ok(gzip) = GzipReader::new(Cursor::new(data)) {
        drain(gzip);
    }
});
