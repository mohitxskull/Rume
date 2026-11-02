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

    let assertion = SamlParser::parse(saml_xml).unwrap();
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

    let assertion = SamlParser::parse(saml_xml).unwrap();
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
}
