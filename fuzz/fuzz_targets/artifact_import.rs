#![no_main]

use arkforge_artifact::dayu200::{self, inspect as inspect_dayu200};
use arkforge_artifact::pac::inspect as inspect_pac;
use libfuzzer_sys::fuzz_target;
use std::io::Cursor;

fuzz_target!(|data: &[u8]| {
    let _ = inspect_dayu200(Cursor::new(data));
    let _ = inspect_pac(Cursor::new(data));
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = dayu200::parse_parameter(text, 512);
    }
});
