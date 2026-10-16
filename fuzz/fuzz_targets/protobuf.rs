#![no_main]

use arkforge_ipc::framing::read_frame;
use arkforge_ipc::messages::{Hello, Request, Response};
use arkforge_ipc::wire::{FieldValue, Reader};
use libfuzzer_sys::fuzz_target;
use std::io::Cursor;

fn walk(input: &[u8], depth: usize) {
    if depth > 8 {
        return;
    }
    let mut reader = Reader::new(input);
    loop {
        let value = match reader.next_field() {
            Ok(Some((_, value))) => value,
            _ => break,
        };
        let bytes = match &value {
            FieldValue::Bytes(bytes) => Some(*bytes),
            _ => None,
        };
        if let Some(bytes) = bytes {
            // Depth limit is enforced here as well as by the recursive walk.
            let _ = reader.nested_reader(&value);
            walk(bytes, depth + 1);
        }
    }
}

fuzz_target!(|data: &[u8]| {
    walk(data, 0);
    let _ = Hello::decode(data);
    let _ = Request::decode(data);
    let _ = Response::decode(data);
    // Allocate a frame body only when the declared length is already present.
    // A peer can still force the production cap; this harness must not.
    if data.len() >= 4 {
        let declared = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
        if declared <= data.len() - 4 {
            let _ = read_frame(&mut Cursor::new(data));
        }
    }
});
