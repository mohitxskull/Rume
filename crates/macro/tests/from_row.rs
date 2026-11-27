use rume_data::persistence::{DbRow, DbValue, FromRow, PersistenceError};
use rume_macros::FromRow;
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, FromRow)]
#[from_row(crate = "rume_data::persistence")]
struct UserRecord {
    id: i64,
    #[from_row(rename = "full_name")]
    name: String,
    r#type: String,
}

#[derive(Debug, PartialEq, FromRow)]
#[from_row(crate = "rume_data::persistence")]
struct DefaultRecord {
    id: i64,
    #[from_row(default)]
    retries: i32,
    #[from_row(default)]
    label: String,
}

#[derive(Debug, PartialEq, FromRow)]
#[from_row(crate = "rume_data::persistence")]
struct SkippedRecord {
    id: i64,
    #[from_row(skip)]
    cached_tag: String,
}

#[derive(Debug, PartialEq, FromRow)]
#[from_row(crate = "rume_data::persistence")]
struct AuditMeta {
    created_by: String,
    version: i64,
}

#[derive(Debug, PartialEq, FromRow)]
#[from_row(crate = "rume_data::persistence")]
struct DocumentRecord {
    id: i64,
    #[from_row(flatten)]
    audit: AuditMeta,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Payload {
    enabled: bool,
    tags: Vec<String>,
}

#[derive(Debug, PartialEq, FromRow)]
#[from_row(crate = "rume_data::persistence")]
struct JsonRecord {
    #[from_row(json)]
    payload: Payload,
    #[from_row(json)]
    extra: Option<Payload>,
}

#[derive(Debug, PartialEq, FromRow)]
#[from_row(crate = "rume_data::persistence")]
struct Wrapper<T, M> {
    value: T,
    #[from_row(default)]
    fallback: T,
    #[from_row(flatten)]
    meta: M,
}

#[test]
fn from_row_decodes_named_struct_and_rename() {
    let row = DbRow::new(
        vec!["id".into(), "full_name".into(), "type".into()],
        vec![
            DbValue::Int(42),
            DbValue::Text("Ada Lovelace".into()),
            DbValue::Text("admin".into()),
        ],
    );

    let record = UserRecord::from_row(&row).expect("decode UserRecord");
    assert_eq!(
        record,
        UserRecord {
            id: 42,
            name: "Ada Lovelace".into(),
            r#type: "admin".into(),
        }
    );
}

#[test]
fn from_row_default_handles_missing_column_and_null_but_propagates_decode_error() {
    // Missing `retries` and NULL `label` both fall back to Default::default()
    let row_missing_and_null = DbRow::new(
        vec!["id".into(), "label".into()],
        vec![DbValue::Int(7), DbValue::Null],
    );
    let record = DefaultRecord::from_row(&row_missing_and_null).expect("decode with defaults");
    assert_eq!(
        record,
        DefaultRecord {
            id: 7,
            retries: 0,
            label: String::new(),
        }
    );

    // Corrupt non-null value for `retries` must still propagate DecodeError
    let row_corrupt = DbRow::new(
        vec!["id".into(), "retries".into(), "label".into()],
        vec![
            DbValue::Int(7),
            DbValue::Text("not_an_int".into()),
            DbValue::Text("ok".into()),
        ],
    );
    let err = DefaultRecord::from_row(&row_corrupt).unwrap_err();
    match err {
        PersistenceError::DecodeError { column, .. } => assert_eq!(column, "retries"),
        other => panic!("expected DecodeError, got {other:?}"),
    }
}

#[test]
fn from_row_skip_uses_default_without_touching_row() {
    let row = DbRow::new(vec!["id".into()], vec![DbValue::Int(99)]);
    let record = SkippedRecord::from_row(&row).expect("decode SkippedRecord");
    assert_eq!(
        record,
        SkippedRecord {
            id: 99,
            cached_tag: String::new(),
        }
    );
}

#[test]
fn from_row_flatten_decodes_nested_struct() {
    let row = DbRow::new(
        vec!["id".into(), "created_by".into(), "version".into()],
        vec![
            DbValue::Int(10),
            DbValue::Text("system".into()),
            DbValue::Int(3),
        ],
    );
    let record = DocumentRecord::from_row(&row).expect("decode DocumentRecord");
    assert_eq!(
        record,
        DocumentRecord {
            id: 10,
            audit: AuditMeta {
                created_by: "system".into(),
                version: 3,
            },
        }
    );
}

#[test]
fn from_row_json_deserializes_text_bytes_and_optional_null() {
    let payload_json = r#"{"enabled":true,"tags":["a","b"]}"#;
    let row_text_and_null = DbRow::new(
        vec!["payload".into(), "extra".into()],
        vec![DbValue::Text(payload_json.into()), DbValue::Null],
    );
    let decoded = JsonRecord::from_row(&row_text_and_null).expect("decode text json + null");
    assert_eq!(
        decoded,
        JsonRecord {
            payload: Payload {
                enabled: true,
                tags: vec!["a".into(), "b".into()],
            },
            extra: None,
        }
    );

    let extra_bytes = br#"{"enabled":false,"tags":["x"]}"#.to_vec();
    let row_bytes = DbRow::new(
        vec!["payload".into(), "extra".into()],
        vec![
            DbValue::Bytes(payload_json.as_bytes().to_vec()),
            DbValue::Bytes(extra_bytes),
        ],
    );
    let decoded_bytes = JsonRecord::from_row(&row_bytes).expect("decode bytes json");
    assert_eq!(
        decoded_bytes,
        JsonRecord {
            payload: Payload {
                enabled: true,
                tags: vec!["a".into(), "b".into()],
            },
            extra: Some(Payload {
                enabled: false,
                tags: vec!["x".into()],
            }),
        }
    );

    let row_malformed = DbRow::new(
        vec!["payload".into(), "extra".into()],
        vec![DbValue::Text("{not-json".into()), DbValue::Null],
    );
    let err = JsonRecord::from_row(&row_malformed).unwrap_err();
    match err {
        PersistenceError::DecodeError { column, .. } => assert_eq!(column, "payload"),
        other => panic!("expected DecodeError, got {other:?}"),
    }
}

#[test]
fn from_row_generic_struct_with_bounds() {
    let row = DbRow::new(
        vec!["value".into(), "created_by".into(), "version".into()],
        vec![
            DbValue::Int(123),
            DbValue::Text("alice".into()),
            DbValue::Int(2),
        ],
    );
    let wrapped = Wrapper::<i64, AuditMeta>::from_row(&row).expect("decode generic Wrapper");
    assert_eq!(
        wrapped,
        Wrapper {
            value: 123,
            fallback: 0,
            meta: AuditMeta {
                created_by: "alice".into(),
                version: 2,
            },
        }
    );
}
