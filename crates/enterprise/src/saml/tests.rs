#[cfg(test)]
mod saml_parser_and_iso8601_proptests {
    use super::super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn parse_unverified_never_panics_on_arbitrary_utf8(xml in "\\PC*") {
            let _ = SamlParser::parse_unverified(&xml);
            let _ = SamlParser::parse_entity_id(&xml);
        }

        #[test]
        fn parse_iso8601_strict_never_panics_on_arbitrary_or_multibyte_offsets(
            s in "\\PC*",
            suffix in "\\PC*",
        ) {
            let _ = parse_iso8601_strict(&s);
            let combined = format!("2026-11-19T12:00:00{suffix}");
            let _ = parse_iso8601_strict(&combined);
        }

        #[test]
        fn parse_iso8601_strict_converts_valid_rfc3339_instants_and_rejects_pre_epoch(
            year in 1970i32..=2099,
            month in 1u32..=12,
            day in 1u32..=28,
            hour in 0u32..=23,
            minute in 0u32..=59,
            second in 0u32..=59,
            use_z in any::<bool>(),
            positive_offset in any::<bool>(),
            off_h in 0u32..=23,
            off_m in 0u32..=59,
        ) {
            let naive = chrono::NaiveDate::from_ymd_opt(year, month, day)
                .unwrap()
                .and_hms_opt(hour, minute, second)
                .unwrap();

            let (tz_suffix, offset_secs) = if use_z {
                ("Z".to_string(), 0i64)
            } else {
                let sign = if positive_offset { '+' } else { '-' };
                let secs = (off_h as i64) * 3600 + (off_m as i64) * 60;
                let signed = if positive_offset { secs } else { -secs };
                (format!("{sign}{off_h:02}:{off_m:02}"), signed)
            };

            let ts = format!(
                "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}{tz_suffix}"
            );
            let expected_utc = naive.and_utc().timestamp() - offset_secs;

            match expected_utc {
                secs if secs >= 0 => {
                    let parsed = parse_iso8601_strict(&ts)
                        .expect("valid post-epoch RFC 3339 timestamp must succeed");
                    prop_assert_eq!(parsed, secs as u64);
                }
                _ => {
                    let err = parse_iso8601_strict(&ts)
                        .expect_err("pre-1970 UTC instant must fail closed");
                    prop_assert!(matches!(err, SamlError::Malformed(_)));
                }
            }
        }

        #[test]
        fn parse_iso8601_strict_rejects_out_of_range_offsets_space_separator_and_pre_1970(
            bad_hour in 24u32..=99,
            bad_min in 60u32..=99,
            pre_year in 1900i32..=1969,
            sign in prop::sample::select(vec!['+', '-']),
        ) {
            let bad_offset_h = format!("2026-11-19T12:00:00{sign}{bad_hour:02}:00");
            prop_assert!(matches!(
                parse_iso8601_strict(&bad_offset_h),
                Err(SamlError::Malformed(_))
            ));

            let bad_offset_m = format!("2026-11-19T12:00:00{sign}00:{bad_min:02}");
            prop_assert!(matches!(
                parse_iso8601_strict(&bad_offset_m),
                Err(SamlError::Malformed(_))
            ));

            prop_assert!(matches!(
                parse_iso8601_strict("2026-11-19 12:00:00Z"),
                Err(SamlError::Malformed(_))
            ));

            let pre_epoch = format!("{pre_year:04}-06-15T12:00:00Z");
            prop_assert!(matches!(
                parse_iso8601_strict(&pre_epoch),
                Err(SamlError::Malformed(_))
            ));
        }
    }

    #[test]
    fn signed_mode_rejects_reference_outside_signature_signed_info() {
        use std::sync::atomic::{AtomicBool, Ordering};

        struct PanicIfCalled(AtomicBool);
        impl SamlSignatureVerifier for PanicIfCalled {
            fn verify_assertion_signature(
                &self,
                _: &str,
                _: &str,
                _: &str,
            ) -> Result<(), SamlError> {
                self.0.store(true, Ordering::SeqCst);
                Ok(())
            }
        }

        // Stray `<ds:Reference URI="#_a1"/>` outside `<ds:Signature><ds:SignedInfo>`.
        let xml = r##"<saml:Assertion xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" xmlns:ds="http://www.w3.org/2000/09/xmldsig#" ID="_a1">
            <saml:Issuer>https://idp.example.com</saml:Issuer>
            <ds:Reference URI="#_a1"/>
            <saml:Subject><saml:NameID>alice@example.org</saml:NameID></saml:Subject>
        </saml:Assertion>"##;

        let verifier = PanicIfCalled(AtomicBool::new(false));
        let err = SamlParser::parse_signed(xml, &verifier)
            .expect_err("stray ds:Reference outside SignedInfo must be rejected");
        assert!(matches!(err, SamlError::SignatureWrappingDetected(_)));
        assert!(
            !verifier.0.load(Ordering::SeqCst),
            "verifier must not be invoked when ds:Reference lacks ds:Signature/ds:SignedInfo ancestry"
        );
    }

    #[test]
    fn claims_assemble_across_cdata_comments_entities_and_nested_issuer() {
        let xml = r#"<saml:Assertion xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="_a2">
            <saml:Issuer>https://idp.example.com<saml:Issuer>nested</saml:Issuer>/sso</saml:Issuer>
            <saml:Subject>
                <saml:NameID>alice<![CDATA[@example.org]]></saml:NameID>
            </saml:Subject>
            <saml:Conditions>
                <saml:AudienceRestriction>
                    <saml:Audience>https://sp.example.com/a&amp;b&#63;v=1</saml:Audience>
                </saml:AudienceRestriction>
            </saml:Conditions>
            <saml:AttributeStatement>
                <saml:Attribute Name="roles">
                    <saml:AttributeValue>team <!-- split --> lead &amp; ops</saml:AttributeValue>
                    <saml:AttributeValue>auditor</saml:AttributeValue>
                </saml:Attribute>
            </saml:AttributeStatement>
        </saml:Assertion>"#;

        let assertion = SamlParser::parse_unverified(xml).expect("valid assertion must parse");
        assert_eq!(assertion.issuer, "https://idp.example.com/sso");
        assert_eq!(assertion.subject, "alice@example.org");
        assert_eq!(
            assertion.audiences,
            vec!["https://sp.example.com/a&b?v=1".to_string()]
        );
        assert_eq!(
            assertion.attributes.get("roles"),
            Some(&vec!["team  lead & ops".to_string(), "auditor".to_string()])
        );
    }

    #[test]
    fn unresolved_xml_entity_reference_fails_closed() {
        let xml = r#"<saml:Assertion xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="_a3">
            <saml:Issuer>https://idp.example.com</saml:Issuer>
            <saml:Subject><saml:NameID>alice&unknown;example.org</saml:NameID></saml:Subject>
        </saml:Assertion>"#;

        let err = SamlParser::parse_unverified(xml)
            .expect_err("unresolved XML entity reference must fail closed");
        assert!(matches!(err, SamlError::Malformed(_)));
    }

    #[test]
    fn nested_attribute_inside_attribute_value_keeps_owning_attribute_name() {
        let xml = r#"<saml:Assertion xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="_a4">
            <saml:Issuer>https://idp.example.com</saml:Issuer>
            <saml:Subject>
                <saml:NameID>alice@example.org</saml:NameID>
            </saml:Subject>
            <saml:AttributeStatement>
                <saml:Attribute Name="roles">
                    <saml:AttributeValue>user<saml:Attribute Name="meta"/></saml:AttributeValue>
                    <saml:AttributeValue>admin</saml:AttributeValue>
                </saml:Attribute>
            </saml:AttributeStatement>
        </saml:Assertion>"#;

        let assertion = SamlParser::parse_unverified(xml).expect("valid assertion must parse");
        assert_eq!(
            assertion.attributes.get("roles"),
            Some(&vec!["user".to_string(), "admin".to_string()]),
            "nested <Attribute> must neither rename nor clear the owning attribute"
        );
        assert!(!assertion.attributes.contains_key("meta"));
    }

    #[test]
    fn signature_subtree_injected_subject_conditions_and_attributes_are_ignored() {
        struct AcceptVerifier;
        impl SamlSignatureVerifier for AcceptVerifier {
            fn verify_assertion_signature(
                &self,
                _: &str,
                _: &str,
                _: &str,
            ) -> Result<(), SamlError> {
                Ok(())
            }
        }

        let xml = r##"<saml:Assertion xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" xmlns:ds="http://www.w3.org/2000/09/xmldsig#" ID="_a_xsw_sig">
            <saml:Issuer>https://idp.example.com</saml:Issuer>
            <ds:Signature>
                <ds:SignedInfo>
                    <ds:Reference URI="#_a_xsw_sig"/>
                </ds:SignedInfo>
                <ds:Object>
                    <saml:Subject>
                        <saml:NameID>alice@acme.example</saml:NameID>
                    </saml:Subject>
                    <saml:Conditions NotBefore="2020-01-01T00:00:00Z" NotOnOrAfter="2099-01-01T00:00:00Z">
                        <saml:AudienceRestriction>
                            <saml:Audience>https://forged-sp.example.com</saml:Audience>
                        </saml:AudienceRestriction>
                    </saml:Conditions>
                    <saml:AttributeStatement>
                        <saml:Attribute Name="roles">
                            <saml:AttributeValue>admin</saml:AttributeValue>
                        </saml:Attribute>
                    </saml:AttributeStatement>
                </ds:Object>
            </ds:Signature>
            <saml:Subject>
                <saml:NameID>mallory@acme.example</saml:NameID>
            </saml:Subject>
            <saml:Conditions NotBefore="2026-11-02T11:00:00Z" NotOnOrAfter="2026-11-02T13:00:00Z">
                <saml:AudienceRestriction>
                    <saml:Audience>https://real-sp.example.com</saml:Audience>
                </saml:AudienceRestriction>
            </saml:Conditions>
            <saml:AttributeStatement>
                <saml:Attribute Name="roles">
                    <saml:AttributeValue>member</saml:AttributeValue>
                </saml:Attribute>
            </saml:AttributeStatement>
        </saml:Assertion>"##;

        let assertion = SamlParser::parse_signed(xml, &AcceptVerifier)
            .expect("valid signed assertion with ds:Object payload must parse real claims");
        assert_eq!(
            assertion.subject, "mallory@acme.example",
            "Subject injected inside ds:Signature/ds:Object must be ignored"
        );
        assert_eq!(
            assertion.audiences,
            vec!["https://real-sp.example.com".to_string()],
            "Audience injected inside ds:Signature/ds:Object must be ignored"
        );
        assert_eq!(
            assertion.attributes.get("roles"),
            Some(&vec!["member".to_string()]),
            "Attribute values injected inside ds:Signature/ds:Object must be ignored"
        );
        assert_eq!(
            assertion.not_before,
            Some(parse_iso8601_strict("2026-11-02T11:00:00Z").unwrap()),
            "NotBefore must come from the outer Conditions, not ds:Signature/ds:Object"
        );
        assert_eq!(
            assertion.not_on_or_after,
            Some(parse_iso8601_strict("2026-11-02T13:00:00Z").unwrap()),
            "NotOnOrAfter must come from the outer Conditions, not ds:Signature/ds:Object"
        );
    }

    #[test]
    fn signed_mode_rejects_unsigned_response_issuer_fallback_when_assertion_lacks_issuer() {
        struct AcceptVerifier;
        impl SamlSignatureVerifier for AcceptVerifier {
            fn verify_assertion_signature(
                &self,
                _: &str,
                _: &str,
                _: &str,
            ) -> Result<(), SamlError> {
                Ok(())
            }
        }

        let xml = r##"<saml2p:Response xmlns:saml2p="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" xmlns:ds="http://www.w3.org/2000/09/xmldsig#" ID="_resp_forged">
            <saml:Issuer>https://attacker-chosen-issuer.example.com</saml:Issuer>
            <saml:Assertion ID="_assert_no_issuer">
                <ds:Signature>
                    <ds:SignedInfo>
                        <ds:Reference URI="#_assert_no_issuer"/>
                    </ds:SignedInfo>
                </ds:Signature>
                <saml:Subject>
                    <saml:NameID>user@enterprise.com</saml:NameID>
                </saml:Subject>
            </saml:Assertion>
        </saml2p:Response>"##;

        let err = SamlParser::parse_signed(xml, &AcceptVerifier)
            .expect_err("signed assertion missing its own <Issuer> must not fall back to unsigned Response <Issuer>");
        assert!(matches!(err, SamlError::Malformed(_)));
    }

    #[test]
    fn advice_extension_audience_cannot_override_assertion_conditions_audience() {
        let xml = r#"<saml:Assertion xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" xmlns:ext="urn:example:ext" ID="_a_adv_aud">
            <saml:Issuer>https://idp.example.com</saml:Issuer>
            <saml:Subject>
                <saml:NameID>alice@acme.example</saml:NameID>
            </saml:Subject>
            <saml:Advice>
                <ext:Payload>
                    <saml:AudienceRestriction>
                        <saml:Audience>https://forged-advice-sp.example.com</saml:Audience>
                    </saml:AudienceRestriction>
                </ext:Payload>
            </saml:Advice>
            <saml:Conditions NotBefore="2026-11-02T11:00:00Z" NotOnOrAfter="2026-11-02T13:00:00Z">
                <saml:AudienceRestriction>
                    <saml:Audience>https://real-sp.example.com</saml:Audience>
                </saml:AudienceRestriction>
            </saml:Conditions>
        </saml:Assertion>"#;

        let assertion = SamlParser::parse_unverified(xml)
            .expect("valid assertion with Advice extension must parse");
        assert_eq!(
            assertion.audiences,
            vec!["https://real-sp.example.com".to_string()],
            "Audience nested under Advice/ext:Payload must not override Assertion/Conditions/AudienceRestriction/Audience"
        );
    }

    #[test]
    fn duplicate_id_attribute_triggers_signature_wrapping_detected() {
        let xml = r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="_dup">
            <saml:Assertion ID="_dup">
                <saml:Issuer>https://idp.example.com</saml:Issuer>
                <saml:Subject><saml:NameID>alice@example.org</saml:NameID></saml:Subject>
            </saml:Assertion>
        </samlp:Response>"#;
        let err = SamlParser::parse_unverified(xml).unwrap_err();
        assert!(matches!(err, SamlError::SignatureWrappingDetected(_)));
    }

    #[test]
    fn multi_audience_matches_second_entry_and_subject_confirmation_and_replay_cache_work() {
        let xml = r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="_r1" Destination="https://sp.example.com/acs" InResponseTo="_req_42">
            <saml:Assertion ID="_a_multi">
                <saml:Issuer>https://idp.example.com</saml:Issuer>
                <saml:Subject>
                    <saml:NameID>alice@example.org</saml:NameID>
                    <saml:SubjectConfirmation Method="urn:oasis:names:tc:SAML:2.0:cm:bearer">
                        <saml:SubjectConfirmationData Recipient="https://sp.example.com/acs" NotOnOrAfter="2030-01-01T00:00:00Z" InResponseTo="_req_42"/>
                    </saml:SubjectConfirmation>
                </saml:Subject>
                <saml:Conditions NotBefore="2020-01-01T00:00:00Z" NotOnOrAfter="2030-01-01T00:00:00Z">
                    <saml:AudienceRestriction>
                        <saml:Audience>https://other-sp.example.com</saml:Audience>
                        <saml:Audience>https://sp.example.com</saml:Audience>
                    </saml:AudienceRestriction>
                </saml:Conditions>
            </saml:Assertion>
        </samlp:Response>"#;

        let assertion = SamlParser::parse_unverified(xml).expect("valid assertion");
        assert_eq!(assertion.audiences.len(), 2);

        let now = parse_iso8601_strict("2025-06-01T00:00:00Z").unwrap();
        let replay_cache = InMemorySamlReplayCache::new();
        let cfg = SamlValidationConfig {
            expected_audience: Some("https://sp.example.com"),
            expected_issuer: Some("https://idp.example.com"),
            expected_destination: Some("https://sp.example.com/acs"),
            expected_recipient: Some("https://sp.example.com/acs"),
            expected_in_response_to: Some("_req_42"),
            clock_skew_secs: 30,
            replay_cache: Some(&replay_cache),
            ..Default::default()
        };
        SamlParser::validate_with_config(&assertion, now, &cfg).expect("first use succeeds");

        // Second use of the same assertion ID must fail with AssertionReplayed.
        let replay_err = SamlParser::validate_with_config(&assertion, now, &cfg).unwrap_err();
        assert!(matches!(replay_err, SamlError::AssertionReplayed(_)));

        // Wrong Recipient and wrong InResponseTo fail closed.
        let bad_recipient = SamlParser::validate_with_config(
            &assertion,
            now,
            &SamlValidationConfig {
                expected_recipient: Some("https://wrong.example.com/acs"),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(matches!(bad_recipient, SamlError::RecipientMismatch { .. }));

        let bad_irt = SamlParser::validate_with_config(
            &assertion,
            now,
            &SamlValidationConfig {
                expected_in_response_to: Some("_req_999"),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(matches!(bad_irt, SamlError::InResponseToMismatch { .. }));
    }

    #[test]
    fn normalized_attribute_unescapes_recipient_entities() {
        let xml = r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="_r_esc">
            <saml:Assertion ID="_a_esc">
                <saml:Issuer>https://idp.example.com</saml:Issuer>
                <saml:Subject>
                    <saml:NameID>alice@example.org</saml:NameID>
                    <saml:SubjectConfirmation Method="urn:oasis:names:tc:SAML:2.0:cm:bearer">
                        <saml:SubjectConfirmationData Recipient="https://sp.example/acs?a=1&amp;b=2" NotOnOrAfter="2030-01-01T00:00:00Z"/>
                    </saml:SubjectConfirmation>
                </saml:Subject>
                <saml:Conditions NotBefore="2020-01-01T00:00:00Z" NotOnOrAfter="2030-01-01T00:00:00Z"/>
            </saml:Assertion>
        </samlp:Response>"#;
        let assertion = SamlParser::parse_unverified(xml).expect("valid escaped recipient");
        let now = parse_iso8601_strict("2025-06-01T00:00:00Z").unwrap();
        SamlParser::validate_with_config(
            &assertion,
            now,
            &SamlValidationConfig {
                expected_recipient: Some("https://sp.example/acs?a=1&b=2"),
                ..Default::default()
            },
        )
        .expect("normalized &amp; in Recipient must match unescaped URL");
    }

    #[test]
    fn multiple_bearer_subject_confirmations_succeed_when_first_is_valid_and_second_is_invalid() {
        let xml = r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="_r_sc" InResponseTo="_req_ok">
            <saml:Assertion ID="_a_sc">
                <saml:Issuer>https://idp.example.com</saml:Issuer>
                <saml:Subject>
                    <saml:NameID>alice@example.org</saml:NameID>
                    <saml:SubjectConfirmation Method="urn:oasis:names:tc:SAML:2.0:cm:bearer">
                        <saml:SubjectConfirmationData Recipient="https://sp.example/acs" NotOnOrAfter="2030-01-01T00:00:00Z" InResponseTo="_req_ok"/>
                    </saml:SubjectConfirmation>
                    <saml:SubjectConfirmation Method="urn:oasis:names:tc:SAML:2.0:cm:bearer">
                        <saml:SubjectConfirmationData Recipient="https://other-sp.example/acs" NotOnOrAfter="2020-01-01T00:00:00Z" InResponseTo="_req_other"/>
                    </saml:SubjectConfirmation>
                </saml:Subject>
                <saml:Conditions NotBefore="2020-01-01T00:00:00Z" NotOnOrAfter="2030-01-01T00:00:00Z"/>
            </saml:Assertion>
        </samlp:Response>"#;
        let assertion = SamlParser::parse_unverified(xml).expect("valid assertion");
        assert_eq!(assertion.subject_confirmations.len(), 2);
        let now = parse_iso8601_strict("2025-06-01T00:00:00Z").unwrap();
        SamlParser::validate_with_config(
            &assertion,
            now,
            &SamlValidationConfig {
                expected_recipient: Some("https://sp.example/acs"),
                expected_in_response_to: Some("_req_ok"),
                ..Default::default()
            },
        )
        .expect("valid first bearer SubjectConfirmation must not be overwritten by second");
    }

    #[test]
    fn multiple_audience_restriction_groups_require_and_across_groups() {
        let xml = r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="_r_aud">
            <saml:Assertion ID="_a_aud">
                <saml:Issuer>https://idp.example.com</saml:Issuer>
                <saml:Subject><saml:NameID>alice@example.org</saml:NameID></saml:Subject>
                <saml:Conditions NotBefore="2020-01-01T00:00:00Z" NotOnOrAfter="2030-01-01T00:00:00Z">
                    <saml:AudienceRestriction>
                        <saml:Audience>https://other-sp.example.com</saml:Audience>
                    </saml:AudienceRestriction>
                    <saml:AudienceRestriction>
                        <saml:Audience>https://this-sp.example.com</saml:Audience>
                    </saml:AudienceRestriction>
                </saml:Conditions>
            </saml:Assertion>
        </samlp:Response>"#;
        let assertion = SamlParser::parse_unverified(xml).expect("valid assertion");
        let now = parse_iso8601_strict("2025-06-01T00:00:00Z").unwrap();
        let err = SamlParser::validate_with_config(
            &assertion,
            now,
            &SamlValidationConfig {
                expected_audience: Some("https://this-sp.example.com"),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(
            matches!(err, SamlError::AudienceMismatch { .. }),
            "separate <AudienceRestriction> groups require AND semantics"
        );
    }

    #[test]
    fn expected_in_response_to_does_not_fall_back_to_unsigned_response_attribute() {
        let xml = r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="_r_irt" InResponseTo="_req_forged">
            <saml:Assertion ID="_a_irt">
                <saml:Issuer>https://idp.example.com</saml:Issuer>
                <saml:Subject>
                    <saml:NameID>alice@example.org</saml:NameID>
                    <saml:SubjectConfirmation Method="urn:oasis:names:tc:SAML:2.0:cm:bearer">
                        <saml:SubjectConfirmationData Recipient="https://sp.example/acs" NotOnOrAfter="2030-01-01T00:00:00Z"/>
                    </saml:SubjectConfirmation>
                </saml:Subject>
                <saml:Conditions NotBefore="2020-01-01T00:00:00Z" NotOnOrAfter="2030-01-01T00:00:00Z"/>
            </saml:Assertion>
        </samlp:Response>"#;
        let assertion = SamlParser::parse_unverified(xml).expect("valid assertion");
        let now = parse_iso8601_strict("2025-06-01T00:00:00Z").unwrap();
        let err = SamlParser::validate_with_config(
            &assertion,
            now,
            &SamlValidationConfig {
                expected_in_response_to: Some("_req_forged"),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(
            matches!(err, SamlError::InResponseToMismatch { .. }),
            "must not accept unsigned Response/@InResponseTo when SubjectConfirmationData omits InResponseTo"
        );
    }

    #[test]
    fn replay_cache_retains_entry_through_clock_skew_window() {
        let xml = r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="_r_skew">
            <saml:Assertion ID="_a_skew">
                <saml:Issuer>https://idp.example.com</saml:Issuer>
                <saml:Subject>
                    <saml:NameID>alice@example.org</saml:NameID>
                    <saml:SubjectConfirmation Method="urn:oasis:names:tc:SAML:2.0:cm:bearer">
                        <saml:SubjectConfirmationData NotOnOrAfter="1970-01-01T00:16:40Z"/>
                    </saml:SubjectConfirmation>
                </saml:Subject>
                <saml:Conditions NotBefore="1970-01-01T00:00:01Z" NotOnOrAfter="1970-01-01T00:16:40Z"/>
            </saml:Assertion>
        </samlp:Response>"#;
        let assertion = SamlParser::parse_unverified(xml).expect("valid assertion");
        assert_eq!(assertion.not_on_or_after, Some(1000));

        let cache = InMemorySamlReplayCache::new();
        let cfg = SamlValidationConfig {
            clock_skew_secs: 30,
            replay_cache: Some(&cache),
            ..Default::default()
        };
        SamlParser::validate_with_config(&assertion, 999, &cfg).expect("initial validation ok");

        // Prune at exact NotOnOrAfter (1000): the entry must survive through 1000 + 30 = 1030.
        cache.prune_expired(1000);
        let err = SamlParser::validate_with_config(&assertion, 1001, &cfg).unwrap_err();
        assert!(
            matches!(err, SamlError::AssertionReplayed(_)),
            "replay within skew window must still be rejected after pruning at not_on_or_after"
        );
    }

    #[test]
    fn typed_condition_audience_restriction_is_enforced_and_unknown_condition_fails_closed() {
        let xml_excluded = r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" ID="_r_cond">
            <saml:Assertion ID="_a_cond">
                <saml:Issuer>https://idp.example.com</saml:Issuer>
                <saml:Subject>
                    <saml:NameID>alice@example.org</saml:NameID>
                    <saml:SubjectConfirmation Method="urn:oasis:names:tc:SAML:2.0:cm:bearer">
                        <saml:SubjectConfirmationData NotOnOrAfter="2030-01-01T00:00:00Z"/>
                    </saml:SubjectConfirmation>
                </saml:Subject>
                <saml:Conditions NotBefore="2020-01-01T00:00:00Z" NotOnOrAfter="2030-01-01T00:00:00Z">
                    <saml:AudienceRestriction>
                        <saml:Audience>https://this-sp.example.com</saml:Audience>
                    </saml:AudienceRestriction>
                    <saml:Condition xsi:type="saml:AudienceRestrictionType">
                        <saml:Audience>https://other-sp.example.com</saml:Audience>
                    </saml:Condition>
                </saml:Conditions>
            </saml:Assertion>
        </samlp:Response>"#;
        let assertion = SamlParser::parse_unverified(xml_excluded).expect("typed condition parses");
        let now = parse_iso8601_strict("2025-06-01T00:00:00Z").unwrap();
        let err = SamlParser::validate_with_config(
            &assertion,
            now,
            &SamlValidationConfig {
                expected_audience: Some("https://this-sp.example.com"),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(matches!(err, SamlError::AudienceMismatch { .. }));

        let xml_unknown = r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" ID="_r_unk">
            <saml:Assertion ID="_a_unk">
                <saml:Issuer>https://idp.example.com</saml:Issuer>
                <saml:Subject><saml:NameID>alice@example.org</saml:NameID></saml:Subject>
                <saml:Conditions NotBefore="2020-01-01T00:00:00Z" NotOnOrAfter="2030-01-01T00:00:00Z">
                    <saml:Condition xsi:type="custom:CustomConditionType"/>
                </saml:Conditions>
            </saml:Assertion>
        </samlp:Response>"#;
        let err_unknown = SamlParser::parse_unverified(xml_unknown).unwrap_err();
        assert!(matches!(err_unknown, SamlError::Malformed(_)));

        // A foreign namespace prefix bound to `AudienceRestrictionType` must NOT be
        // treated as SAML's built-in AudienceRestrictionType; it must fail closed.
        let xml_foreign_ns = r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" ID="_r_fns">
            <saml:Assertion ID="_a_fns">
                <saml:Issuer>https://idp.example.com</saml:Issuer>
                <saml:Subject><saml:NameID>alice@example.org</saml:NameID></saml:Subject>
                <saml:Conditions NotBefore="2020-01-01T00:00:00Z" NotOnOrAfter="2030-01-01T00:00:00Z">
                    <saml:Condition xmlns:ext="urn:example:ext" xsi:type="ext:AudienceRestrictionType">
                        <saml:Audience>https://this-sp.example.com</saml:Audience>
                    </saml:Condition>
                </saml:Conditions>
            </saml:Assertion>
        </samlp:Response>"#;
        let err_foreign = SamlParser::parse_unverified(xml_foreign_ns).unwrap_err();
        assert!(matches!(err_foreign, SamlError::Malformed(_)));
    }

    #[test]
    fn subject_confirmation_requires_explicit_bearer_method_and_not_on_or_after_and_outer_in_response_to()
     {
        let now = parse_iso8601_strict("2025-06-01T00:00:00Z").unwrap();

        // Missing Method attribute on SubjectConfirmation must NOT be treated as bearer.
        let xml_no_method = r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="_r_nm">
            <saml:Assertion ID="_a_nm">
                <saml:Issuer>https://idp.example.com</saml:Issuer>
                <saml:Subject>
                    <saml:NameID>alice@example.org</saml:NameID>
                    <saml:SubjectConfirmation>
                        <saml:SubjectConfirmationData Recipient="https://sp.example/acs" NotOnOrAfter="2030-01-01T00:00:00Z"/>
                    </saml:SubjectConfirmation>
                </saml:Subject>
                <saml:Conditions NotBefore="2020-01-01T00:00:00Z" NotOnOrAfter="2030-01-01T00:00:00Z"/>
            </saml:Assertion>
        </samlp:Response>"#;
        let a_no_method = SamlParser::parse_unverified(xml_no_method).expect("parses");
        assert!(a_no_method.subject_confirmations.is_empty());
        assert!(matches!(
            SamlParser::validate_with_config(&a_no_method, now, &SamlValidationConfig::default()),
            Err(SamlError::Malformed(_))
        ));

        // Holder-of-key only confirmation must be rejected by default Web Browser SSO validation.
        let xml_hok = r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="_r_hok">
            <saml:Assertion ID="_a_hok">
                <saml:Issuer>https://idp.example.com</saml:Issuer>
                <saml:Subject>
                    <saml:NameID>alice@example.org</saml:NameID>
                    <saml:SubjectConfirmation Method="urn:oasis:names:tc:SAML:2.0:cm:holder-of-key">
                        <saml:SubjectConfirmationData NotOnOrAfter="2030-01-01T00:00:00Z"/>
                    </saml:SubjectConfirmation>
                </saml:Subject>
                <saml:Conditions NotBefore="2020-01-01T00:00:00Z" NotOnOrAfter="2030-01-01T00:00:00Z"/>
            </saml:Assertion>
        </samlp:Response>"#;
        let a_hok = SamlParser::parse_unverified(xml_hok).expect("parses");
        assert!(matches!(
            SamlParser::validate_with_config(&a_hok, now, &SamlValidationConfig::default()),
            Err(SamlError::Malformed(_))
        ));

        // Bearer confirmation lacking NotOnOrAfter must be rejected; a 2nd valid one can still pass.
        let xml_no_exp = r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="_r_ne">
            <saml:Assertion ID="_a_ne">
                <saml:Issuer>https://idp.example.com</saml:Issuer>
                <saml:Subject>
                    <saml:NameID>alice@example.org</saml:NameID>
                    <saml:SubjectConfirmation Method="urn:oasis:names:tc:SAML:2.0:cm:bearer">
                        <saml:SubjectConfirmationData Recipient="https://sp.example/acs"/>
                    </saml:SubjectConfirmation>
                </saml:Subject>
                <saml:Conditions NotBefore="2020-01-01T00:00:00Z" NotOnOrAfter="2030-01-01T00:00:00Z"/>
            </saml:Assertion>
        </samlp:Response>"#;
        let a_no_exp = SamlParser::parse_unverified(xml_no_exp).expect("parses");
        assert!(matches!(
            SamlParser::validate_with_config(&a_no_exp, now, &SamlValidationConfig::default()),
            Err(SamlError::MissingExpirationCondition)
        ));

        // Solicited Response omitting outer Response/@InResponseTo must fail closed even if SCD has it.
        let xml_missing_outer_irt = r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="_r_mo">
            <saml:Assertion ID="_a_mo">
                <saml:Issuer>https://idp.example.com</saml:Issuer>
                <saml:Subject>
                    <saml:NameID>alice@example.org</saml:NameID>
                    <saml:SubjectConfirmation Method="urn:oasis:names:tc:SAML:2.0:cm:bearer">
                        <saml:SubjectConfirmationData Recipient="https://sp.example/acs" NotOnOrAfter="2030-01-01T00:00:00Z" InResponseTo="_req_ok"/>
                    </saml:SubjectConfirmation>
                </saml:Subject>
                <saml:Conditions NotBefore="2020-01-01T00:00:00Z" NotOnOrAfter="2030-01-01T00:00:00Z"/>
            </saml:Assertion>
        </samlp:Response>"#;
        let a_missing_outer = SamlParser::parse_unverified(xml_missing_outer_irt).expect("parses");
        assert!(matches!(
            SamlParser::validate_with_config(
                &a_missing_outer,
                now,
                &SamlValidationConfig {
                    expected_in_response_to: Some("_req_ok"),
                    ..Default::default()
                }
            ),
            Err(SamlError::InResponseToMismatch { .. })
        ));
    }

    #[test]
    fn xsi_type_qname_trims_xml_schema_whitespace_before_namespace_resolution() {
        let xml = r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" ID="_r_qname_ws">
            <saml:Assertion ID="_a_qname_ws">
                <saml:Issuer>https://idp.example.com</saml:Issuer>
                <saml:Subject>
                    <saml:NameID>alice@example.org</saml:NameID>
                    <saml:SubjectConfirmation Method="urn:oasis:names:tc:SAML:2.0:cm:bearer">
                        <saml:SubjectConfirmationData Recipient="https://sp.example/acs" NotOnOrAfter="2030-01-01T00:00:00Z"/>
                    </saml:SubjectConfirmation>
                </saml:Subject>
                <saml:Conditions NotBefore="2020-01-01T00:00:00Z" NotOnOrAfter="2030-01-01T00:00:00Z">
                    <saml:Condition xsi:type="  saml:AudienceRestrictionType  ">
                        <saml:Audience>https://sp.example</saml:Audience>
                    </saml:Condition>
                </saml:Conditions>
            </saml:Assertion>
        </samlp:Response>"#;

        let assertion = SamlParser::parse_unverified(xml)
            .expect("whitespace-padded xsi:type QName in SAML namespace must be accepted");
        assert_eq!(
            assertion.audience_restrictions,
            vec![vec!["https://sp.example".to_string()]]
        );
    }

    #[test]
    fn xsi_type_unicode_whitespace_prefix_is_not_trimmed_into_saml_prefix() {
        // U+1680 is a valid XML name character: the prefix `\u{1680}saml` is bound to a foreign
        // namespace and must NOT be collapsed into the SAML-bound `saml` prefix.
        let xml = "<samlp:Response xmlns:samlp=\"urn:oasis:names:tc:SAML:2.0:protocol\" xmlns:saml=\"urn:oasis:names:tc:SAML:2.0:assertion\" xmlns:\u{1680}saml=\"urn:example:foreign\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" ID=\"_r_u1680\">\
            <saml:Assertion ID=\"_a_u1680\">\
                <saml:Issuer>https://idp.example.com</saml:Issuer>\
                <saml:Subject><saml:NameID>alice@example.org</saml:NameID></saml:Subject>\
                <saml:Conditions NotBefore=\"2020-01-01T00:00:00Z\" NotOnOrAfter=\"2030-01-01T00:00:00Z\">\
                    <saml:Condition xsi:type=\"\u{1680}saml:AudienceRestrictionType\">\
                        <saml:Audience>https://sp.example</saml:Audience>\
                    </saml:Condition>\
                </saml:Conditions>\
            </saml:Assertion>\
        </samlp:Response>";

        assert!(
            matches!(
                SamlParser::parse_unverified(xml),
                Err(SamlError::Malformed(_))
            ),
            "foreign-namespace type behind a U+1680 prefix must fail closed"
        );
    }

    #[test]
    fn fractional_second_validity_bounds_are_not_truncated() {
        let xml = r#"<saml:Assertion xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="_a_frac">
            <saml:Issuer>https://idp.example.com</saml:Issuer>
            <saml:Subject>
                <saml:NameID>alice@example.org</saml:NameID>
                <saml:SubjectConfirmation Method="urn:oasis:names:tc:SAML:2.0:cm:bearer">
                    <saml:SubjectConfirmationData Recipient="https://sp.example/acs" NotOnOrAfter="1970-01-01T00:16:40.900Z"/>
                </saml:SubjectConfirmation>
            </saml:Subject>
            <saml:Conditions NotBefore="1970-01-01T00:16:40.900Z" NotOnOrAfter="1970-01-01T00:16:41.900Z"/>
        </saml:Assertion>"#;
        let assertion = SamlParser::parse_unverified(xml).expect("parses fractional seconds");
        // At 1000.000s, NotBefore=1000.900s is still in the future.
        assert!(matches!(
            SamlParser::validate_with_config(&assertion, 1000, &SamlValidationConfig::default()),
            Err(SamlError::NotYetValid { .. })
        ));
        // At 1001.000s, SCD NotOnOrAfter=1000.900s has passed.
        assert!(matches!(
            SamlParser::validate_with_config(&assertion, 1001, &SamlValidationConfig::default()),
            Err(SamlError::Expired { .. })
        ));

        // Conditions NotOnOrAfter=1000.900s must still be valid at 1000.000s (not expired early).
        let xml2 = r#"<saml:Assertion xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="_a_frac2">
            <saml:Issuer>https://idp.example.com</saml:Issuer>
            <saml:Subject>
                <saml:NameID>alice@example.org</saml:NameID>
                <saml:SubjectConfirmation Method="urn:oasis:names:tc:SAML:2.0:cm:bearer">
                    <saml:SubjectConfirmationData Recipient="https://sp.example/acs" NotOnOrAfter="1970-01-01T00:16:40.900Z"/>
                </saml:SubjectConfirmation>
            </saml:Subject>
            <saml:Conditions NotBefore="1970-01-01T00:00:01Z" NotOnOrAfter="1970-01-01T00:16:40.900Z"/>
        </saml:Assertion>"#;
        let assertion2 = SamlParser::parse_unverified(xml2).expect("parses");
        SamlParser::validate_with_config(&assertion2, 1000, &SamlValidationConfig::default())
            .expect("1000.000s is before 1000.900s");
        assert!(matches!(
            SamlParser::validate_with_config(&assertion2, 1001, &SamlValidationConfig::default()),
            Err(SamlError::Expired { .. })
        ));
    }

    #[test]
    fn qualified_foreign_attributes_and_nested_response_are_ignored_and_truncated_eof_rejected() {
        // Qualified `ext:Method` must not trigger bearer confirmation.
        let xml_ext_method = r#"<saml:Assertion xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" xmlns:ext="urn:example:ext" ID="_a_ext">
            <saml:Issuer>https://idp.example.com</saml:Issuer>
            <saml:Subject>
                <saml:NameID>alice@example.org</saml:NameID>
                <saml:SubjectConfirmation ext:Method="urn:oasis:names:tc:SAML:2.0:cm:bearer">
                    <saml:SubjectConfirmationData Recipient="https://sp.example/acs" NotOnOrAfter="2030-01-01T00:00:00Z"/>
                </saml:SubjectConfirmation>
            </saml:Subject>
            <saml:Conditions NotBefore="2020-01-01T00:00:00Z" NotOnOrAfter="2030-01-01T00:00:00Z"/>
        </saml:Assertion>"#;
        let a_ext = SamlParser::parse_unverified(xml_ext_method).expect("parses");
        assert!(a_ext.subject_confirmations.is_empty());

        // Nested `<samlp:Response>` inside Advice must not overwrite root Response/@Destination.
        let xml_nested_resp = r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="_r_root" Destination="https://sp.example/acs">
            <saml:Assertion ID="_a_nr">
                <saml:Issuer>https://idp.example.com</saml:Issuer>
                <saml:Subject><saml:NameID>alice@example.org</saml:NameID></saml:Subject>
                <saml:Advice>
                    <samlp:Response ID="_r_nested" Destination="https://evil.example/acs"/>
                </saml:Advice>
            </saml:Assertion>
        </samlp:Response>"#;
        let a_nr = SamlParser::parse_unverified(xml_nested_resp).expect("parses");
        assert_eq!(a_nr.destination.as_deref(), Some("https://sp.example/acs"));

        // Truncated Assertion omitting `</saml:Assertion>` must fail closed at EOF.
        let xml_truncated = r#"<saml:Assertion xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="_a_trunc">
            <saml:Issuer>https://idp.example.com</saml:Issuer>
            <saml:Subject><saml:NameID>alice@example.org</saml:NameID></saml:Subject>"#;
        assert!(matches!(
            SamlParser::parse_unverified(xml_truncated),
            Err(SamlError::Malformed(_))
        ));
    }

    #[test]
    fn parse_entity_id_requires_metadata_ns_unqualified_attr_and_decodes_entities() {
        let valid = r#"<md:EntityDescriptor xmlns:md="urn:oasis:names:tc:SAML:2.0:metadata" xmlns:ext="urn:example:ext" ext:entityID="https://wrong.example/" entityID="https://idp.example/?a=1&amp;b=2"/>"#;
        assert_eq!(
            SamlParser::parse_entity_id(valid).expect("valid metadata"),
            "https://idp.example/?a=1&b=2"
        );

        let foreign =
            r#"<EntityDescriptor xmlns="urn:example:foreign" entityID="https://idp.example/"/>"#;
        assert!(matches!(
            SamlParser::parse_entity_id(foreign),
            Err(SamlError::Malformed(_))
        ));
    }

    #[test]
    fn leap_second_timestamps_are_rejected_fail_closed() {
        assert!(matches!(
            parse_iso8601_strict("2016-12-31T23:59:60.500Z"),
            Err(SamlError::Malformed(_))
        ));
    }

    #[test]
    fn validate_browser_sso_enforces_signature_authn_context_audience_recipient_and_shared_skew_replay_cache()
     {
        struct AcceptVerifier;
        impl SamlSignatureVerifier for AcceptVerifier {
            fn verify_assertion_signature(
                &self,
                _: &str,
                _: &str,
                _: &str,
            ) -> Result<(), SamlError> {
                Ok(())
            }
        }

        let cache = InMemorySamlReplayCache::with_max_clock_skew(30);

        // 1. AuthnStatement missing `<AuthnContext>` is rejected (SAML Core §2.7.2).
        let xml_no_context = r##"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" xmlns:ds="http://www.w3.org/2000/09/xmldsig#" ID="_r_sso_nc">
            <saml:Assertion ID="_a_sso_nc">
                <saml:Issuer>https://idp.example.com</saml:Issuer>
                <ds:Signature><ds:SignedInfo><ds:Reference URI="#_a_sso_nc"/></ds:SignedInfo></ds:Signature>
                <saml:Subject>
                    <saml:NameID>alice@example.org</saml:NameID>
                    <saml:SubjectConfirmation Method="urn:oasis:names:tc:SAML:2.0:cm:bearer">
                        <saml:SubjectConfirmationData Recipient="https://sp.example/acs" NotOnOrAfter="1970-01-01T00:16:40Z"/>
                    </saml:SubjectConfirmation>
                </saml:Subject>
                <saml:Conditions NotBefore="1970-01-01T00:00:01Z" NotOnOrAfter="1970-01-01T00:16:40Z">
                    <saml:AudienceRestriction><saml:Audience>https://sp.example</saml:Audience></saml:AudienceRestriction>
                </saml:Conditions>
                <saml:AuthnStatement AuthnInstant="1970-01-01T00:00:05Z"/>
            </saml:Assertion>
        </samlp:Response>"##;
        let no_ctx = SamlParser::parse_signed(xml_no_context, &AcceptVerifier).expect("parses");
        assert!(!no_ctx.has_authn_statement);
        assert!(matches!(
            SamlParser::validate_browser_sso(
                &no_ctx,
                500,
                &SamlValidationConfig {
                    expected_audience: Some("https://sp.example"),
                    expected_recipient: Some("https://sp.example/acs"),
                    replay_cache: Some(&cache),
                    ..Default::default()
                },
            ),
            Err(SamlError::Malformed(_))
        ));

        let xml_full = r##"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" xmlns:ds="http://www.w3.org/2000/09/xmldsig#" ID="_r_sso2">
            <saml:Assertion ID="_a_sso2">
                <saml:Issuer>https://idp.example.com</saml:Issuer>
                <ds:Signature><ds:SignedInfo><ds:Reference URI="#_a_sso2"/></ds:SignedInfo></ds:Signature>
                <saml:Subject>
                    <saml:NameID>alice@example.org</saml:NameID>
                    <saml:SubjectConfirmation Method="urn:oasis:names:tc:SAML:2.0:cm:bearer">
                        <saml:SubjectConfirmationData Recipient="https://sp.example/acs" NotOnOrAfter="1970-01-01T00:16:40Z"/>
                    </saml:SubjectConfirmation>
                </saml:Subject>
                <saml:Conditions NotBefore="1970-01-01T00:00:01Z" NotOnOrAfter="1970-01-01T00:16:40Z">
                    <saml:AudienceRestriction><saml:Audience>https://sp.example</saml:Audience></saml:AudienceRestriction>
                </saml:Conditions>
                <saml:AuthnStatement AuthnInstant="1970-01-01T00:00:05Z">
                    <saml:AuthnContext>
                        <saml:AuthnContextClassRef>urn:oasis:names:tc:SAML:2.0:ac:classes:PasswordProtectedTransport</saml:AuthnContextClassRef>
                    </saml:AuthnContext>
                </saml:AuthnStatement>
            </saml:Assertion>
        </samlp:Response>"##;

        // 2. Unverified assertion must be rejected by `validate_browser_sso`.
        let unverified = SamlParser::parse_unverified(xml_full).expect("parses unverified");
        assert!(unverified.has_authn_statement);
        assert!(matches!(
            SamlParser::validate_browser_sso(
                &unverified,
                500,
                &SamlValidationConfig {
                    expected_audience: Some("https://sp.example"),
                    expected_recipient: Some("https://sp.example/acs"),
                    replay_cache: Some(&cache),
                    ..Default::default()
                },
            ),
            Err(SamlError::MissingSignatureVerifier)
        ));

        let full = SamlParser::parse_signed(xml_full, &AcceptVerifier).expect("parses signed");
        assert!(full.has_authn_statement);

        // 3. Missing audience fails closed.
        assert!(matches!(
            SamlParser::validate_browser_sso(
                &full,
                500,
                &SamlValidationConfig {
                    expected_recipient: Some("https://sp.example/acs"),
                    replay_cache: Some(&cache),
                    ..Default::default()
                },
            ),
            Err(SamlError::Malformed(_))
        ));
        // 4. Missing recipient fails closed.
        assert!(matches!(
            SamlParser::validate_browser_sso(
                &full,
                500,
                &SamlValidationConfig {
                    expected_audience: Some("https://sp.example"),
                    replay_cache: Some(&cache),
                    ..Default::default()
                },
            ),
            Err(SamlError::Malformed(_))
        ));
        // 5. Missing replay cache fails closed.
        assert!(matches!(
            SamlParser::validate_browser_sso(
                &full,
                500,
                &SamlValidationConfig {
                    expected_audience: Some("https://sp.example"),
                    expected_recipient: Some("https://sp.example/acs"),
                    ..Default::default()
                },
            ),
            Err(SamlError::Malformed(_))
        ));

        // 6. Mixed-skew shared replay cache: Validator A (skew 0) validates at 999,
        // `prune_expired(1000)` runs, then Validator B (skew 30) attempts replay at 1001
        // and must still be rejected as `AssertionReplayed`.
        let cfg_skew_0 = SamlValidationConfig {
            expected_audience: Some("https://sp.example"),
            expected_recipient: Some("https://sp.example/acs"),
            clock_skew_secs: 0,
            replay_cache: Some(&cache),
            ..Default::default()
        };
        let cfg_skew_30 = SamlValidationConfig {
            expected_audience: Some("https://sp.example"),
            expected_recipient: Some("https://sp.example/acs"),
            clock_skew_secs: 30,
            replay_cache: Some(&cache),
            ..Default::default()
        };
        SamlParser::validate_browser_sso(&full, 999, &cfg_skew_0).expect("validator A succeeds");
        cache.prune_expired(1000);
        assert!(matches!(
            SamlParser::validate_browser_sso(&full, 1001, &cfg_skew_30),
            Err(SamlError::AssertionReplayed(_))
        ));

        // 7. Validator skew exceeding cache `max_clock_skew_secs` fails closed.
        assert!(matches!(
            SamlParser::validate_browser_sso(
                &full,
                999,
                &SamlValidationConfig {
                    expected_audience: Some("https://sp.example"),
                    expected_recipient: Some("https://sp.example/acs"),
                    clock_skew_secs: 60,
                    replay_cache: Some(&cache),
                    ..Default::default()
                },
            ),
            Err(SamlError::Malformed(_))
        ));

        // 8. Atomic expiry-vs-pruning check: after pruning at 1030, a late validation at 1029
        // must be rejected as Expired instead of re-inserting `_a_sso2`.
        cache.prune_expired(1030);
        assert!(matches!(
            SamlParser::validate_browser_sso(&full, 1029, &cfg_skew_30),
            Err(SamlError::Expired { .. })
        ));
    }
}
