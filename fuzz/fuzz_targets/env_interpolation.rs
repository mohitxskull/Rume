#![no_main]

use std::collections::BTreeMap;

use libfuzzer_sys::fuzz_target;
use rume_foundation::env::interpolation::interpolate_value;

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        let mut lines = text.split('\n');
        let input = lines.next().unwrap_or_default();
        let mut vars = BTreeMap::new();
        for line in lines.take(32) {
            if let Some((k, v)) = line.split_once('=')
                && !k.is_empty()
            {
                vars.insert(k.to_string(), v.to_string());
            }
        }
        let _ = interpolate_value(input, &vars);
    }
});
