use std::collections::BTreeMap;
use std::marker::PhantomData;

use rume_macros::HasOpenApiSchema;

mod mock_openapi {
    use std::collections::BTreeMap;

    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    pub struct OpenApiSchema {
        pub r#type: String,
        pub format: Option<String>,
        pub pattern: Option<String>,
        pub min_length: Option<usize>,
        pub max_length: Option<usize>,
        pub minimum: Option<i64>,
        pub maximum: Option<i64>,
        pub properties: Option<BTreeMap<String, OpenApiSchema>>,
        pub required: Option<Vec<String>>,
    }

    impl OpenApiSchema {
        pub fn string() -> Self {
            Self {
                r#type: "string".into(),
                ..Default::default()
            }
        }

        pub fn integer() -> Self {
            Self {
                r#type: "integer".into(),
                format: Some("int64".into()),
                ..Default::default()
            }
        }

        pub fn number() -> Self {
            Self {
                r#type: "number".into(),
                ..Default::default()
            }
        }

        pub fn object() -> Self {
            Self {
                r#type: "object".into(),
                ..Default::default()
            }
        }

        pub fn with_min_length(mut self, min: usize) -> Self {
            self.min_length = Some(min);
            self
        }

        pub fn with_max_length(mut self, max: usize) -> Self {
            self.max_length = Some(max);
            self
        }

        pub fn with_minimum(mut self, min: i64) -> Self {
            self.minimum = Some(min);
            self
        }

        pub fn with_maximum(mut self, max: i64) -> Self {
            self.maximum = Some(max);
            self
        }

        pub fn with_format(mut self, format: impl Into<String>) -> Self {
            self.format = Some(format.into());
            self
        }

        pub fn with_pattern(mut self, pattern: impl Into<String>) -> Self {
            self.pattern = Some(pattern.into());
            self
        }

        pub fn with_property(mut self, name: impl Into<String>, schema: OpenApiSchema) -> Self {
            let mut props = self.properties.unwrap_or_default();
            props.insert(name.into(), schema);
            self.properties = Some(props);
            self
        }

        pub fn with_required_property(
            mut self,
            name: impl Into<String>,
            schema: OpenApiSchema,
        ) -> Self {
            let name_str = name.into();
            let mut req = self.required.unwrap_or_default();
            if !req.contains(&name_str) {
                req.push(name_str.clone());
            }
            self.required = Some(req);
            self.with_property(name_str, schema)
        }
    }

    pub trait HasOpenApiSchema {
        fn schema() -> OpenApiSchema;
    }

    impl HasOpenApiSchema for String {
        fn schema() -> OpenApiSchema {
            OpenApiSchema::string()
        }
    }

    impl HasOpenApiSchema for i64 {
        fn schema() -> OpenApiSchema {
            OpenApiSchema::integer()
        }
    }

    impl HasOpenApiSchema for f64 {
        fn schema() -> OpenApiSchema {
            OpenApiSchema::number()
        }
    }

    impl<T: HasOpenApiSchema> HasOpenApiSchema for Option<T> {
        fn schema() -> OpenApiSchema {
            T::schema()
        }
    }
}

use mock_openapi::{HasOpenApiSchema as MockHasOpenApiSchema, OpenApiSchema};

#[derive(HasOpenApiSchema)]
#[openapi(crate = "mock_openapi")]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct CreateUserPayload {
    #[garde(length(min = 3, max = 50))]
    user_name: String,
    #[garde(email)]
    contact_email: String,
    #[garde(url)]
    homepage_url: Option<String>,
    #[garde(range(min = 18, max = 120))]
    age_years: i64,
    #[garde(range(min = 0.5, max = 99.5))]
    score: f64,
    #[garde(pattern("^[a-z0-9-]+$"))]
    #[serde(rename = "handle")]
    slug: String,
    #[serde(default)]
    bio: String,
    #[serde(skip)]
    internal_secret: String,
    #[serde(skip_deserializing)]
    computed_tag: String,
    #[garde(dive, custom(my_validator))]
    r#type: String,
}

#[derive(HasOpenApiSchema)]
#[openapi(crate = "mock_openapi")]
struct EmptyUnit;

#[derive(HasOpenApiSchema)]
#[openapi(crate = "mock_openapi")]
#[allow(dead_code)]
struct GenericEnvelope<T, Unused> {
    data: T,
    fallback: Option<T>,
    #[serde(skip)]
    _marker: PhantomData<Unused>,
}

struct NotASchema;

#[test]
fn derive_has_openapi_schema_generates_expected_struct_schema() {
    let schema = CreateUserPayload::schema();
    assert_eq!(schema.r#type, "object");

    let props: &BTreeMap<String, OpenApiSchema> = schema.properties.as_ref().expect("properties");
    assert_eq!(props.len(), 8);
    assert!(!props.contains_key("internalSecret"));
    assert!(!props.contains_key("computedTag"));

    let user_name = props.get("userName").expect("userName");
    assert_eq!(user_name.r#type, "string");
    assert_eq!(user_name.min_length, Some(3));
    assert_eq!(user_name.max_length, Some(50));

    let email = props.get("contactEmail").expect("contactEmail");
    assert_eq!(email.format.as_deref(), Some("email"));

    let homepage = props.get("homepageUrl").expect("homepageUrl");
    assert_eq!(homepage.format.as_deref(), Some("uri"));

    let age = props.get("ageYears").expect("ageYears");
    assert_eq!(age.minimum, Some(18));
    assert_eq!(age.maximum, Some(120));

    let score = props.get("score").expect("score");
    assert_eq!(score.r#type, "number");
    assert_eq!(score.minimum, None);
    assert_eq!(score.maximum, None);

    let handle = props.get("handle").expect("handle");
    assert_eq!(handle.pattern.as_deref(), Some("^[a-z0-9-]+$"));

    assert!(props.contains_key("bio"));
    assert!(props.contains_key("type"));

    let required = schema.required.as_ref().expect("required");
    assert_eq!(
        required,
        &vec![
            "userName".to_string(),
            "contactEmail".to_string(),
            "ageYears".to_string(),
            "score".to_string(),
            "handle".to_string(),
            "type".to_string(),
        ]
    );
}

#[test]
fn derive_has_openapi_schema_supports_unit_and_generic_structs() {
    let unit = EmptyUnit::schema();
    assert_eq!(unit, OpenApiSchema::object());

    let env = GenericEnvelope::<String, NotASchema>::schema();
    let props = env.properties.as_ref().expect("properties");
    assert_eq!(props.len(), 2);
    assert_eq!(props["data"].r#type, "string");
    assert_eq!(props["fallback"].r#type, "string");
    assert_eq!(env.required, Some(vec!["data".to_string()]));
}

#[test]
fn derive_has_openapi_schema_compile_fail_diagnostics() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/openapi_union_rejected.rs");
    t.compile_fail("tests/ui/openapi_tuple_struct_rejected.rs");
}
