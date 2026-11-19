#![no_main]

use libfuzzer_sys::fuzz_target;
use rume_enterprise::enterprise::saml::{
    SamlError, SamlParser, SamlSignatureVerifier, parse_iso8601_strict,
};

struct AcceptAllVerifier;

impl SamlSignatureVerifier for AcceptAllVerifier {
    fn verify_signature(&self, _xml: &str) -> Result<(), SamlError> {
        Ok(())
    }

    fn verify_assertion_signature(
        &self,
        _signed_xml: &str,
        _signature_value: &str,
        _expected_ref_id: &str,
    ) -> Result<(), SamlError> {
        Ok(())
    }
}

fuzz_target!(|data: &[u8]| {
    if let Ok(input) = std::str::from_utf8(data) {
        let _ = SamlParser::parse_signed(input, &AcceptAllVerifier);
        let _ = SamlParser::parse_entity_id(input);
        let _ = parse_iso8601_strict(input);
    }
});
