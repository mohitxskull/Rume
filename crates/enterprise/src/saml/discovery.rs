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
        loop {
            match reader.read_resolved_event() {
                Ok((res, Event::Start(ref e))) | Ok((res, Event::Empty(ref e))) => {
                    let is_metadata_ns = matches!(
                        res,
                        ResolveResult::Bound(ns) if ns.as_ref() == SAML_METADATA_NS
                    );
                    if is_metadata_ns && e.local_name().as_ref() == "EntityDescriptor" {
                        for attr_res in e.attributes() {
                            let attr = attr_res.map_err(|err| {
                                SamlError::Malformed(format!("malformed XML attribute: {err}"))
                            })?;
                            let (attr_ns, attr_local) =
                                reader.resolver().resolve_attribute(attr.key);
                            if matches!(attr_ns, ResolveResult::Unbound)
                                && attr_local.as_ref() == "entityID"
                            {
                                return attr
                                    .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                                    .map(|v| v.into_owned())
                                    .map_err(|err| {
                                        SamlError::Malformed(format!(
                                            "malformed XML attribute value: {err}"
                                        ))
                                    });
                            }
                        }
                        return Err(SamlError::Malformed(
                            "missing entityID attribute on EntityDescriptor".into(),
                        ));
                    }
                }
                Ok((_, Event::Eof)) => break,
                Err(err) => return Err(SamlError::Malformed(err.to_string())),
                _ => {}
            }
        }
        Err(SamlError::Malformed(
            "EntityDescriptor element not found".into(),
        ))
    }
}
