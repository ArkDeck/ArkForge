#![no_main]

use arkforge_core::digest::decode_canonical;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = decode_canonical(data);
});
