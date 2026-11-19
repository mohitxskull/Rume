#![no_main]

use libfuzzer_sys::fuzz_target;
use rume_data::persistence::backend::fuzz_support::fuzz_validate_sql_validators;

fuzz_target!(|data: &[u8]| {
    if let Ok(sql) = std::str::from_utf8(data) {
        fuzz_validate_sql_validators(sql);
    }
});
