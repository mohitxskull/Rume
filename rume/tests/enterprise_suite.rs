use rume::prelude::*;

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
