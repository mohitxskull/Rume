#![no_main]

use libfuzzer_sys::fuzz_target;
use rume_enterprise::enterprise::scim::{
    ScimErrorResponse, ScimFilter, ScimGroup, ScimListResponse, ScimUser,
};

fuzz_target!(|data: &[u8]| {
    let _ = serde_json::from_slice::<ScimUser>(data);
    let _ = serde_json::from_slice::<ScimGroup>(data);
    let _ = serde_json::from_slice::<ScimListResponse<ScimUser>>(data);
    let _ = serde_json::from_slice::<ScimErrorResponse>(data);
    if let Ok(filter_str) = std::str::from_utf8(data) {
        let _ = ScimFilter::parse(filter_str);
    }
});
