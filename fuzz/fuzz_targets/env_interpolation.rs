#![no_main]

use std::collections::BTreeMap;

use libfuzzer_sys::fuzz_target;
use rume_foundation::env::interpolation::interpolate_value;

fuzz_target!(|data: &[u8]| {
    if let Ok(input) = std::str::from_utf8(data) {
        let mut vars = BTreeMap::new();
        vars.insert("A".to_string(), "${B}".to_string());
        vars.insert("B".to_string(), "ok".to_string());
        vars.insert("LOOP".to_string(), "${LOOP}".to_string());
        let _ = interpolate_value(input, &vars);
    }
});
