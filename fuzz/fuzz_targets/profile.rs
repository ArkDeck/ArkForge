#![no_main]

use arkforge_core::profile;
use arkforge_core::yaml;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let _ = yaml::parse(text);
    let _ = profile::load(text);
});
