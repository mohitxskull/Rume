use super::assertion::SamlParser;
use super::verify::SamlError;
use quick_xml::events::Event;
use quick_xml::name::ResolveResult;
use quick_xml::reader::NsReader;

const SAML_METADATA_NS: &str = "urn:oasis:names:tc:SAML:2.0:metadata";

impl SamlParser {
    /// Extracts the `entityID` from a SAML 2.0 metadata `<EntityDescriptor>` XML document.
    pub fn parse_entity_id(xml: &str) -> Result<String, SamlError> {
        let mut reader = NsReader::from_str(xml);
        reader.config_mut().trim_text(true);
        let mut entity_id: Option<String> = None;
        let mut depth: usize = 0;

        // Why: parse the entire document to `Event::Eof` and verify balanced tags rather than
        // returning immediately on the opening `<EntityDescriptor>`, so malformed or truncated
        // XML after the opening tag still fails closed.
        loop {
            match reader.read_resolved_event() {
                Ok((res, Event::Start(ref e))) => {
                    depth += 1;
                    let is_metadata_ns = matches!(
                        res,
                        ResolveResult::Bound(ns) if ns.as_ref() == SAML_METADATA_NS
                    );
                    if entity_id.is_none()
                        && depth == 1
                        && is_metadata_ns
                        && e.local_name().as_ref() == "EntityDescriptor"
                    {
                        let mut found = None;
                        for attr_res in e.attributes() {
                            let attr = attr_res.map_err(|err| {
                                SamlError::Malformed(format!("malformed XML attribute: {err}"))
                            })?;
                            let (attr_ns, attr_local) =
                                reader.resolver().resolve_attribute(attr.key);
                            if matches!(attr_ns, ResolveResult::Unbound)
                                && attr_local.as_ref() == "entityID"
                            {
                                found = Some(
                                    attr.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                                        .map(|v| v.into_owned())
                                        .map_err(|err| {
                                            SamlError::Malformed(format!(
                                                "malformed XML attribute value: {err}"
                                            ))
                                        })?,
                                );
                                break;
                            }
                        }
                        let id = found
                            .map(|v| v.trim().to_string())
                            .filter(|v| !v.is_empty())
                            .ok_or_else(|| {
                                SamlError::Malformed(
                                    "missing or empty entityID attribute on EntityDescriptor"
                                        .into(),
                                )
                            })?;
                        entity_id = Some(id);
                    }
                }
                Ok((res, Event::Empty(ref e))) => {
                    let is_metadata_ns = matches!(
                        res,
                        ResolveResult::Bound(ns) if ns.as_ref() == SAML_METADATA_NS
                    );
                    if entity_id.is_none()
                        && depth == 0
                        && is_metadata_ns
                        && e.local_name().as_ref() == "EntityDescriptor"
                    {
                        let mut found = None;
                        for attr_res in e.attributes() {
                            let attr = attr_res.map_err(|err| {
                                SamlError::Malformed(format!("malformed XML attribute: {err}"))
                            })?;
                            let (attr_ns, attr_local) =
                                reader.resolver().resolve_attribute(attr.key);
                            if matches!(attr_ns, ResolveResult::Unbound)
                                && attr_local.as_ref() == "entityID"
                            {
                                found = Some(
                                    attr.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                                        .map(|v| v.into_owned())
                                        .map_err(|err| {
                                            SamlError::Malformed(format!(
                                                "malformed XML attribute value: {err}"
                                            ))
                                        })?,
                                );
                                break;
                            }
                        }
                        let id = found
                            .map(|v| v.trim().to_string())
                            .filter(|v| !v.is_empty())
                            .ok_or_else(|| {
                                SamlError::Malformed(
                                    "missing or empty entityID attribute on EntityDescriptor"
                                        .into(),
                                )
                            })?;
                        entity_id = Some(id);
                    }
                }
                Ok((_, Event::End(_))) => {
                    depth = depth.saturating_sub(1);
                }
                Ok((_, Event::Eof)) => {
                    if depth != 0 {
                        return Err(SamlError::Malformed(
                            "unexpected EOF with unclosed XML elements in metadata".into(),
                        ));
                    }
                    break;
                }
                Err(err) => return Err(SamlError::Malformed(err.to_string())),
                _ => {}
            }
        }
        entity_id.ok_or_else(|| SamlError::Malformed("EntityDescriptor element not found".into()))
    }
}
