use rume::enterprise::*;

#[test]
fn test_scim_models_and_json_serialization() {
    let mut user =
        ScimUser::new("usr-12345", "alice@enterprise.org").with_email("alice@enterprise.org", true);
    user.display_name = Some("Alice Smith".into());
    user.roles.push("admin".into());

    let json = serde_json::to_value(&user).unwrap();
    assert_eq!(json["id"], "usr-12345");
    assert_eq!(json["userName"], "alice@enterprise.org");
    assert_eq!(json["displayName"], "Alice Smith");
    assert_eq!(json["emails"][0]["value"], "alice@enterprise.org");
    assert_eq!(json["emails"][0]["primary"], true);
    assert_eq!(json["schemas"][0], SCIM_USER_SCHEMA);

    let list_response = ScimListResponse::new(vec![user], 1, 1);
    let list_json = serde_json::to_value(&list_response).unwrap();
    assert_eq!(list_json["totalResults"], 1);
    assert_eq!(list_json["itemsPerPage"], 1);
    assert_eq!(list_json["schemas"][0], SCIM_LIST_SCHEMA);

    let err = ScimErrorResponse::new(409, Some("uniqueness"), Some("User already exists"));
    let err_json = serde_json::to_value(&err).unwrap();
    assert_eq!(err_json["status"], "409");
    assert_eq!(err_json["scimType"], "uniqueness");
    assert_eq!(err_json["detail"], "User already exists");
}

#[test]
fn test_saml_assertion_parsing_and_validation() {
    let saml_xml = r#"
    <saml2:Assertion xmlns:saml2="urn:oasis:names:tc:SAML:2.0:assertion" ID="_abc123" IssueInstant="2026-11-02T12:00:00Z">
        <saml2:Issuer>https://idp.enterprise.com/saml</saml2:Issuer>
        <saml2:Subject>
            <saml2:NameID Format="urn:oasis:names:tc:SAML:1.1:nameid-format:emailAddress">bob@enterprise.com</saml2:NameID>
        </saml2:Subject>
        <saml2:Conditions>
            <saml2:AudienceRestriction>
                <saml2:Audience>https://sp.myapp.io/saml/metadata</saml2:Audience>
            </saml2:AudienceRestriction>
        </saml2:Conditions>
        <saml2:AttributeStatement>
            <saml2:Attribute Name="department">
                <saml2:AttributeValue>Engineering</saml2:AttributeValue>
            </saml2:Attribute>
            <saml2:Attribute Name="roles">
                <saml2:AttributeValue>Developer</saml2:AttributeValue>
                <saml2:AttributeValue>DevOps</saml2:AttributeValue>
            </saml2:Attribute>
        </saml2:AttributeStatement>
    </saml2:Assertion>
    "#;

    let assertion = SamlParser::parse_unverified(saml_xml).unwrap();
    assert_eq!(assertion.id, "_abc123");
    assert_eq!(assertion.issuer, "https://idp.enterprise.com/saml");
    assert_eq!(assertion.subject, "bob@enterprise.com");
    assert_eq!(
        assertion.audience.as_deref(),
        Some("https://sp.myapp.io/saml/metadata")
    );

    let dept = assertion.attributes.get("department").unwrap();
    assert_eq!(dept, &vec!["Engineering".to_string()]);

    let roles = assertion.attributes.get("roles").unwrap();
    assert_eq!(roles, &vec!["Developer".to_string(), "DevOps".to_string()]);

    // Validation pass
    let valid = SamlParser::validate(
        &assertion,
        1700000000,
        Some("https://sp.myapp.io/saml/metadata"),
    );
    assert!(valid.is_ok());

    // Audience mismatch rejection
    let mismatch = SamlParser::validate(&assertion, 1700000000, Some("https://other-sp.com"));
    assert!(matches!(mismatch, Err(SamlError::AudienceMismatch { .. })));
}

#[test]
fn test_saml_condition_timestamps_and_comment_injection() {
    let saml_xml = r#"
    <saml2:Assertion xmlns:saml2="urn:oasis:names:tc:SAML:2.0:assertion" ID="_time123">
        <saml2:Issuer>https://idp.enterprise.com</saml2:Issuer>
        <!-- Injected comment trying to spoof NameID:
        <saml2:NameID>ceo@enterprise.com</saml2:NameID>
        -->
        <saml2:Subject>
            <saml2:NameID>contractor@enterprise.com</saml2:NameID>
        </saml2:Subject>
        <saml2:Conditions NotBefore="2026-11-02T10:00:00Z" NotOnOrAfter="2026-11-02T14:00:00Z">
            <saml2:Audience>https://sp.myapp.io</saml2:Audience>
        </saml2:Conditions>
    </saml2:Assertion>
    "#;

    let assertion = SamlParser::parse_unverified(saml_xml).unwrap();
    // Comment was stripped, subject is contractor, not ceo
    assert_eq!(assertion.subject, "contractor@enterprise.com");
    assert!(assertion.not_before.is_some());
    assert!(assertion.not_on_or_after.is_some());

    let nb = assertion.not_before.unwrap();
    let noa = assertion.not_on_or_after.unwrap();
    assert!(nb < noa);

    // Before NotBefore: returns NotYetValid
    let res = SamlParser::validate(&assertion, nb - 100, Some("https://sp.myapp.io"));
    assert!(matches!(res, Err(SamlError::NotYetValid { .. })));

    // After NotOnOrAfter: returns Expired
    let res = SamlParser::validate(&assertion, noa + 100, Some("https://sp.myapp.io"));
    assert!(matches!(res, Err(SamlError::Expired { .. })));

    // Within window: returns Ok
    let res = SamlParser::validate(&assertion, nb + 100, Some("https://sp.myapp.io"));
    assert!(res.is_ok());
}

#[test]
fn test_saml_signature_verifier() {
    struct MockVerifier {
        should_pass: bool,
    }

    impl SamlSignatureVerifier for MockVerifier {
        fn verify_signature(&self, _raw_xml: &str) -> Result<(), SamlError> {
            if self.should_pass {
                Ok(())
            } else {
                Err(SamlError::InvalidSignature("bad digest".into()))
            }
        }
    }

    let saml_xml = r#"
    <saml2:Assertion xmlns:saml2="urn:oasis:names:tc:SAML:2.0:assertion" ID="_sig123">
        <saml2:Issuer>https://idp.enterprise.com</saml2:Issuer>
        <saml2:Subject><saml2:NameID>user@test.org</saml2:NameID></saml2:Subject>
    </saml2:Assertion>
    "#;

    let valid_verifier = MockVerifier { should_pass: true };
    let res = SamlParser::parse_with_verifier(saml_xml, Some(&valid_verifier));
    assert!(res.is_ok());

    let invalid_verifier = MockVerifier { should_pass: false };
    let res = SamlParser::parse_with_verifier(saml_xml, Some(&invalid_verifier));
    assert!(matches!(res, Err(SamlError::InvalidSignature(_))));

    // parse_signed convenience
    let signed_res = SamlParser::parse_signed(saml_xml, &valid_verifier);
    assert!(signed_res.is_ok());
}

#[test]
fn test_saml_signature_wrapping_and_strict_validation() {
    // 1. Detect Signature Wrapping Attack: multiple assertions
    let xsw_xml = r#"
    <saml2p:Response xmlns:saml2p="urn:oasis:names:tc:SAML:2.0:protocol" Destination="https://sp.myapp.io/saml/acs">
        <saml2:Assertion xmlns:saml2="urn:oasis:names:tc:SAML:2.0:assertion" ID="_legit">
            <saml2:Issuer>https://idp.enterprise.com</saml2:Issuer>
            <saml2:Subject><saml2:NameID>legit@enterprise.com</saml2:NameID></saml2:Subject>
        </saml2:Assertion>
        <saml2:Assertion xmlns:saml2="urn:oasis:names:tc:SAML:2.0:assertion" ID="_forged">
            <saml2:Issuer>https://idp.enterprise.com</saml2:Issuer>
            <saml2:Subject><saml2:NameID>attacker@enterprise.com</saml2:NameID></saml2:Subject>
        </saml2:Assertion>
    </saml2p:Response>
    "#;

    let res = SamlParser::parse_unverified(xsw_xml);
    assert!(matches!(res, Err(SamlError::SignatureWrappingDetected(_))));

    // 2. Detect Reference URI mismatch
    let ref_mismatch_xml = r##"
    <saml2:Assertion xmlns:saml2="urn:oasis:names:tc:SAML:2.0:assertion" ID="_actual123">
        <saml2:Issuer>https://idp.enterprise.com</saml2:Issuer>
        <saml2:Subject><saml2:NameID>legit@enterprise.com</saml2:NameID></saml2:Subject>
        <ds:Signature xmlns:ds="http://www.w3.org/2000/09/xmldsig#">
            <ds:SignedInfo>
                <ds:Reference URI="#_differentId" />
            </ds:SignedInfo>
        </ds:Signature>
    </saml2:Assertion>
    "##;

    let res_ref = SamlParser::parse_unverified(ref_mismatch_xml);
    assert!(matches!(
        res_ref,
        Err(SamlError::SignatureWrappingDetected(_))
    ));

    // 3. Strict validation: checks issuer and destination
    let valid_xml = r#"
    <saml2p:Response xmlns:saml2p="urn:oasis:names:tc:SAML:2.0:protocol" Destination="https://sp.myapp.io/saml/acs">
        <saml2:Assertion xmlns:saml2="urn:oasis:names:tc:SAML:2.0:assertion" ID="_single123">
            <saml2:Issuer>https://idp.enterprise.com</saml2:Issuer>
            <saml2:Subject><saml2:NameID>legit@enterprise.com</saml2:NameID></saml2:Subject>
            <saml2:Conditions>
                <saml2:Audience>https://sp.myapp.io</saml2:Audience>
            </saml2:Conditions>
        </saml2:Assertion>
    </saml2p:Response>
    "#;

    let assertion = SamlParser::parse_unverified(valid_xml).unwrap();
    assert_eq!(
        assertion.destination.as_deref(),
        Some("https://sp.myapp.io/saml/acs")
    );

    // Strict validation pass
    let ok = SamlParser::validate_strict(
        &assertion,
        1700000000,
        Some("https://sp.myapp.io"),
        Some("https://idp.enterprise.com"),
        Some("https://sp.myapp.io/saml/acs"),
    );
    assert!(ok.is_ok());

    // Issuer mismatch rejection
    let bad_issuer = SamlParser::validate_strict(
        &assertion,
        1700000000,
        Some("https://sp.myapp.io"),
        Some("https://malicious-idp.com"),
        Some("https://sp.myapp.io/saml/acs"),
    );
    assert!(matches!(bad_issuer, Err(SamlError::IssuerMismatch { .. })));

    // Destination mismatch rejection
    let bad_dest = SamlParser::validate_strict(
        &assertion,
        1700000000,
        Some("https://sp.myapp.io"),
        Some("https://idp.enterprise.com"),
        Some("https://other-sp.com/acs"),
    );
    assert!(matches!(
        bad_dest,
        Err(SamlError::DestinationMismatch { .. })
    ));

    // 4. Legitimate Signed Response Envelope (Reference URI matches Response ID, not Assertion ID)
    let signed_response_envelope = r##"
    <saml2p:Response xmlns:saml2p="urn:oasis:names:tc:SAML:2.0:protocol" ID="_resp_9999" Destination="https://sp.myapp.io/saml/acs">
        <ds:Signature xmlns:ds="http://www.w3.org/2000/09/xmldsig#">
            <ds:SignedInfo>
                <ds:Reference URI="#_resp_9999" />
            </ds:SignedInfo>
        </ds:Signature>
        <saml2:Assertion xmlns:saml2="urn:oasis:names:tc:SAML:2.0:assertion" ID="_assert_1111">
            <saml2:Issuer>https://idp.enterprise.com</saml2:Issuer>
            <saml2:Subject><saml2:NameID>legit@enterprise.com</saml2:NameID></saml2:Subject>
        </saml2:Assertion>
    </saml2p:Response>
    "##;

    let res_signed_envelope = SamlParser::parse_unverified(signed_response_envelope);
    assert!(
        res_signed_envelope.is_ok(),
        "Standard signed Response envelope must be accepted"
    );
    let assertion = res_signed_envelope.unwrap();
    assert_eq!(assertion.id, "_assert_1111");
    assert_eq!(assertion.subject, "legit@enterprise.com");
}

#[test]
fn test_saml_xsw_envelope_injection_prevention() {
    let response_envelope_xsw = r#"
    <saml2p:Response xmlns:saml2p="urn:oasis:names:tc:SAML:2.0:protocol" Destination="https://sp.myapp.io/saml/acs">
        <saml2:Subject xmlns:saml2="urn:oasis:names:tc:SAML:2.0:assertion">
            <saml2:NameID>attacker@evil.com</saml2:NameID>
        </saml2:Subject>
        <saml2:Assertion xmlns:saml2="urn:oasis:names:tc:SAML:2.0:assertion" ID="_legit123">
            <saml2:Issuer>https://idp.enterprise.com</saml2:Issuer>
            <saml2:Subject><saml2:NameID>legit@enterprise.com</saml2:NameID></saml2:Subject>
            <saml2:Conditions>
                <saml2:Audience>https://sp.myapp.io</saml2:Audience>
            </saml2:Conditions>
        </saml2:Assertion>
    </saml2p:Response>
    "#;

    let assertion = SamlParser::parse_unverified(response_envelope_xsw).unwrap();
    assert_eq!(
        assertion.subject, "legit@enterprise.com",
        "NameID must be extracted strictly from inside Assertion subtree, ignoring envelope injection"
    );
}

#[tokio::test]
async fn test_enterprise_idp_metadata_wiremock_integration() {
    let mock_server = wiremock::MockServer::start().await;
    let metadata_xml = r#"<EntityDescriptor entityID="https://mock-idp.example.com"><IDPSSODescriptor /></EntityDescriptor>"#;

    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/saml/metadata"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(metadata_xml))
        .mount(&mock_server)
        .await;

    let target_url = format!("{}/saml/metadata", mock_server.uri());
    let mut resp = ureq::get(&target_url).call().expect("fetch IdP metadata from mock server");
    assert_eq!(resp.status(), 200);
    let body = resp.body_mut().read_to_string().expect("read body");
    assert!(body.contains("mock-idp.example.com"));
}

#[test]
fn test_saml_timezone_offset_handling() {
    // 2026-11-02T12:00:00+02:00 in UTC is 2026-11-02T10:00:00Z
    // 2026-11-02T16:00:00+02:00 in UTC is 2026-11-02T14:00:00Z
    let xml_offset = r#"
    <saml2:Assertion xmlns:saml2="urn:oasis:names:tc:SAML:2.0:assertion" ID="_tz123">
        <saml2:Issuer>https://idp.enterprise.com</saml2:Issuer>
        <saml2:Subject><saml2:NameID>user@enterprise.com</saml2:NameID></saml2:Subject>
        <saml2:Conditions NotBefore="2026-11-02T12:00:00+02:00" NotOnOrAfter="2026-11-02T16:00:00+02:00">
            <saml2:Audience>https://sp.myapp.io</saml2:Audience>
        </saml2:Conditions>
    </saml2:Assertion>
    "#;

    let assertion = SamlParser::parse_unverified(xml_offset).unwrap();
    let nb = assertion.not_before.unwrap();
    let noa = assertion.not_on_or_after.unwrap();

    // In UTC: 2026-11-02T10:00:00Z should equal nb
    // Verify by parsing the UTC equivalent string
    let expected_utc_nb = parse_iso8601_strict("2026-11-02T10:00:00Z").unwrap();
    let expected_utc_noa = parse_iso8601_strict("2026-11-02T14:00:00Z").unwrap();

    assert_eq!(nb, expected_utc_nb, "timezone offset must be converted to UTC");
    assert_eq!(noa, expected_utc_noa, "timezone offset must be converted to UTC");
}

#[test]
fn test_saml_malformed_conditions_fail_closed() {
    // Malformed NotBefore timestamp must NOT be silently ignored as None
    let xml_bad_time = r#"
    <saml2:Assertion xmlns:saml2="urn:oasis:names:tc:SAML:2.0:assertion" ID="_bad_time">
        <saml2:Issuer>https://idp.enterprise.com</saml2:Issuer>
        <saml2:Subject><saml2:NameID>user@enterprise.com</saml2:NameID></saml2:Subject>
        <saml2:Conditions NotBefore="poisoned-invalid-timestamp" NotOnOrAfter="2026-11-02T14:00:00Z" />
    </saml2:Assertion>
    "#;

    let res = SamlParser::parse_unverified(xml_bad_time);
    assert!(
        matches!(res, Err(SamlError::Malformed(_))),
        "malformed NotBefore timestamp must fail closed with Malformed error"
    );

    // Invalid date out of bounds (month 99)
    let xml_bad_date = r#"
    <saml2:Assertion xmlns:saml2="urn:oasis:names:tc:SAML:2.0:assertion" ID="_bad_date">
        <saml2:Issuer>https://idp.enterprise.com</saml2:Issuer>
        <saml2:Subject><saml2:NameID>user@enterprise.com</saml2:NameID></saml2:Subject>
        <saml2:Conditions NotBefore="2026-99-99T12:00:00Z" />
    </saml2:Assertion>
    "#;

    let res_date = SamlParser::parse_unverified(xml_bad_date);
    assert!(
        matches!(res_date, Err(SamlError::Malformed(_))),
        "out of bounds timestamp must fail closed"
    );
}

#[test]
fn test_saml_trust_boundary_exact_assertion_verified() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    struct TrustBoundaryVerifier {
        expected_assertion_id: String,
        verified_exact_node: Arc<AtomicBool>,
    }

    impl SamlSignatureVerifier for TrustBoundaryVerifier {
        fn verify_signature(&self, _raw_xml: &str) -> Result<(), SamlError> {
            Ok(())
        }

        fn verify_assertion_signature(
            &self,
            _raw_xml: &str,
            assertion_id: &str,
            assertion_xml: &str,
        ) -> Result<(), SamlError> {
            if assertion_id == self.expected_assertion_id
                && assertion_xml.contains(&format!(r#"ID="{}""#, self.expected_assertion_id))
            {
                self.verified_exact_node.store(true, Ordering::SeqCst);
                Ok(())
            } else {
                Err(SamlError::InvalidSignature("assertion node ID mismatch at trust boundary".into()))
            }
        }
    }

    let saml_xml = r#"
    <saml2p:Response xmlns:saml2p="urn:oasis:names:tc:SAML:2.0:protocol" ID="_resp_bound_1">
        <saml2:Assertion xmlns:saml2="urn:oasis:names:tc:SAML:2.0:assertion" ID="_assert_bound_42">
            <saml2:Issuer>https://idp.enterprise.com</saml2:Issuer>
            <saml2:Subject><saml2:NameID>corp@enterprise.com</saml2:NameID></saml2:Subject>
        </saml2:Assertion>
    </saml2p:Response>
    "#;

    let verified_flag = Arc::new(AtomicBool::new(false));
    let verifier = TrustBoundaryVerifier {
        expected_assertion_id: "_assert_bound_42".to_string(),
        verified_exact_node: Arc::clone(&verified_flag),
    };

    let assertion = SamlParser::parse_signed(saml_xml, &verifier).unwrap();
    assert_eq!(assertion.id, "_assert_bound_42");
    assert!(
        verified_flag.load(Ordering::SeqCst),
        "verifier must verify the exact assertion node consumed by the application"
    );
}

#[test]
fn test_saml_default_namespace_without_prefix() {
    let raw_xml = r#"
    <Assertion xmlns="urn:oasis:names:tc:SAML:2.0:assertion" ID="_noprefix_123">
        <Issuer>https://idp.simple.org</Issuer>
        <Subject><NameID>plain@simple.org</NameID></Subject>
        <AttributeStatement>
            <Attribute Name="role">
                <AttributeValue><![CDATA[Admin & Architect]]></AttributeValue>
            </Attribute>
        </AttributeStatement>
    </Assertion>
    "#;

    let assertion = SamlParser::parse_unverified(raw_xml).unwrap();
    assert_eq!(assertion.id, "_noprefix_123");
    assert_eq!(assertion.issuer, "https://idp.simple.org");
    assert_eq!(assertion.subject, "plain@simple.org");
    assert_eq!(
        assertion.attributes.get("role").unwrap(),
        &vec!["Admin & Architect".to_string()]
    );
}
