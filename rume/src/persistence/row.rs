//! Generic database row representation and typed column extraction.

use std::fmt::Debug;
use crate::persistence::backend::DbValue;
use crate::persistence::error::PersistenceError;

/// Conversion trait from a generic [`DbValue`] into a typed application value.
pub trait FromDbValue: Sized {
    /// Attempts to convert a database value reference into this type.
    fn from_db_value(val: &DbValue) -> Result<Self, PersistenceError>;
}

impl FromDbValue for String {
    fn from_db_value(val: &DbValue) -> Result<Self, PersistenceError> {
        match val {
            DbValue::Text(s) => Ok(s.clone()),
            DbValue::Int(i) => Ok(i.to_string()),
            DbValue::Float(f) => Ok(f.to_string()),
            DbValue::Bool(b) => Ok(b.to_string()),
            DbValue::Bytes(b) => String::from_utf8(b.clone())
                .map_err(|e| PersistenceError::QueryFailed(format!("invalid utf-8 string: {e}"))),
            DbValue::Null => Err(PersistenceError::QueryFailed(
                "cannot convert NULL database value to non-nullable String".into(),
            )),
        }
    }
}

impl FromDbValue for i64 {
    fn from_db_value(val: &DbValue) -> Result<Self, PersistenceError> {
        match val {
            DbValue::Int(i) => Ok(*i),
            DbValue::Text(s) => s.parse::<i64>().map_err(|e| {
                PersistenceError::QueryFailed(format!("failed to parse '{s}' as i64: {e}"))
            }),
            DbValue::Float(f) => Ok(*f as i64),
            DbValue::Bool(b) => Ok(if *b { 1 } else { 0 }),
            DbValue::Null => Err(PersistenceError::QueryFailed(
                "cannot convert NULL database value to non-nullable i64".into(),
            )),
            DbValue::Bytes(_) => Err(PersistenceError::QueryFailed(
                "cannot convert bytes to i64".into(),
            )),
        }
    }
}

impl FromDbValue for i32 {
    fn from_db_value(val: &DbValue) -> Result<Self, PersistenceError> {
        let n = i64::from_db_value(val)?;
        i32::try_from(n).map_err(|e| PersistenceError::DecodeError {
            column: "<numeric>".into(),
            message: format!("integer value {n} out of range for i32: {e}"),
        })
    }
}

impl FromDbValue for i16 {
    fn from_db_value(val: &DbValue) -> Result<Self, PersistenceError> {
        let n = i64::from_db_value(val)?;
        i16::try_from(n).map_err(|e| PersistenceError::DecodeError {
            column: "<numeric>".into(),
            message: format!("integer value {n} out of range for i16: {e}"),
        })
    }
}

impl FromDbValue for i8 {
    fn from_db_value(val: &DbValue) -> Result<Self, PersistenceError> {
        let n = i64::from_db_value(val)?;
        i8::try_from(n).map_err(|e| PersistenceError::DecodeError {
            column: "<numeric>".into(),
            message: format!("integer value {n} out of range for i8: {e}"),
        })
    }
}

impl FromDbValue for u64 {
    fn from_db_value(val: &DbValue) -> Result<Self, PersistenceError> {
        match val {
            DbValue::Int(i) => u64::try_from(*i).map_err(|e| PersistenceError::DecodeError {
                column: "<numeric>".into(),
                message: format!("negative integer value {i} cannot be converted to u64: {e}"),
            }),
            DbValue::Text(s) => s.parse::<u64>().map_err(|e| PersistenceError::DecodeError {
                column: "<numeric>".into(),
                message: format!("failed to parse '{s}' as u64: {e}"),
            }),
            DbValue::Float(f) => {
                if *f < 0.0 || *f > u64::MAX as f64 || f.is_nan() {
                    return Err(PersistenceError::DecodeError {
                        column: "<numeric>".into(),
                        message: format!("float value {f} out of range for u64"),
                    });
                }
                Ok(*f as u64)
            }
            DbValue::Bool(b) => Ok(if *b { 1 } else { 0 }),
            DbValue::Null => Err(PersistenceError::QueryFailed(
                "cannot convert NULL database value to non-nullable u64".into(),
            )),
            DbValue::Bytes(_) => Err(PersistenceError::QueryFailed(
                "cannot convert bytes to u64".into(),
            )),
        }
    }
}

impl FromDbValue for u32 {
    fn from_db_value(val: &DbValue) -> Result<Self, PersistenceError> {
        let n = u64::from_db_value(val)?;
        u32::try_from(n).map_err(|e| PersistenceError::DecodeError {
            column: "<numeric>".into(),
            message: format!("value {n} out of range for u32: {e}"),
        })
    }
}

impl FromDbValue for u16 {
    fn from_db_value(val: &DbValue) -> Result<Self, PersistenceError> {
        let n = u64::from_db_value(val)?;
        u16::try_from(n).map_err(|e| PersistenceError::DecodeError {
            column: "<numeric>".into(),
            message: format!("value {n} out of range for u16: {e}"),
        })
    }
}

impl FromDbValue for u8 {
    fn from_db_value(val: &DbValue) -> Result<Self, PersistenceError> {
        let n = u64::from_db_value(val)?;
        u8::try_from(n).map_err(|e| PersistenceError::DecodeError {
            column: "<numeric>".into(),
            message: format!("value {n} out of range for u8: {e}"),
        })
    }
}

impl FromDbValue for usize {
    fn from_db_value(val: &DbValue) -> Result<Self, PersistenceError> {
        let n = u64::from_db_value(val)?;
        usize::try_from(n).map_err(|e| PersistenceError::DecodeError {
            column: "<numeric>".into(),
            message: format!("value {n} out of range for usize: {e}"),
        })
    }
}

impl FromDbValue for isize {
    fn from_db_value(val: &DbValue) -> Result<Self, PersistenceError> {
        let n = i64::from_db_value(val)?;
        isize::try_from(n).map_err(|e| PersistenceError::DecodeError {
            column: "<numeric>".into(),
            message: format!("value {n} out of range for isize: {e}"),
        })
    }
}

impl FromDbValue for f64 {
    fn from_db_value(val: &DbValue) -> Result<Self, PersistenceError> {
        match val {
            DbValue::Float(f) => Ok(*f),
            DbValue::Int(i) => Ok(*i as f64),
            DbValue::Text(s) => s.parse::<f64>().map_err(|e| {
                PersistenceError::QueryFailed(format!("failed to parse '{s}' as f64: {e}"))
            }),
            DbValue::Null => Err(PersistenceError::QueryFailed(
                "cannot convert NULL database value to non-nullable f64".into(),
            )),
            _ => Err(PersistenceError::QueryFailed(
                "cannot convert database value to f64".into(),
            )),
        }
    }
}

impl FromDbValue for f32 {
    fn from_db_value(val: &DbValue) -> Result<Self, PersistenceError> {
        let f = f64::from_db_value(val)?;
        if f.is_finite() && (f < f32::MIN as f64 || f > f32::MAX as f64) {
            return Err(PersistenceError::DecodeError {
                column: "<numeric>".into(),
                message: format!("float value {f} out of range for f32"),
            });
        }
        Ok(f as f32)
    }
}

impl FromDbValue for bool {
    fn from_db_value(val: &DbValue) -> Result<Self, PersistenceError> {
        match val {
            DbValue::Bool(b) => Ok(*b),
            DbValue::Int(i) => Ok(*i != 0),
            DbValue::Text(s) => match s.to_lowercase().as_str() {
                "true" | "t" | "1" | "yes" => Ok(true),
                "false" | "f" | "0" | "no" => Ok(false),
                _ => Err(PersistenceError::QueryFailed(format!(
                    "cannot parse '{s}' as boolean"
                ))),
            },
            DbValue::Null => Err(PersistenceError::QueryFailed(
                "cannot convert NULL database value to non-nullable bool".into(),
            )),
            _ => Err(PersistenceError::QueryFailed(
                "cannot convert database value to bool".into(),
            )),
        }
    }
}

impl FromDbValue for Vec<u8> {
    fn from_db_value(val: &DbValue) -> Result<Self, PersistenceError> {
        match val {
            DbValue::Bytes(b) => Ok(b.clone()),
            DbValue::Text(s) => Ok(s.as_bytes().to_vec()),
            DbValue::Null => Err(PersistenceError::QueryFailed(
                "cannot convert NULL database value to non-nullable Vec<u8>".into(),
            )),
            _ => Err(PersistenceError::QueryFailed(
                "cannot convert database value to bytes".into(),
            )),
        }
    }
}

impl<T: FromDbValue> FromDbValue for Option<T> {
    fn from_db_value(val: &DbValue) -> Result<Self, PersistenceError> {
        match val {
            DbValue::Null => Ok(None),
            other => T::from_db_value(other).map(Some),
        }
    }
}

/// A generic database row containing ordered column names and corresponding [`DbValue`] entries.
#[derive(Debug, Clone, PartialEq)]
pub struct DbRow {
    columns: Vec<String>,
    values: Vec<DbValue>,
}

impl DbRow {
    /// Creates a new row from column names and values.
    pub fn new(columns: Vec<String>, values: Vec<DbValue>) -> Self {
        Self { columns, values }
    }

    /// Returns the number of columns in this row.
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Returns whether this row contains zero columns.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Returns the column names in index order.
    pub fn column_names(&self) -> &[String] {
        &self.columns
    }

    /// Retrieves a reference to the raw [`DbValue`] by column name (case-insensitive).
    pub fn get_value(&self, column: &str) -> Result<&DbValue, PersistenceError> {
        let idx = self
            .columns
            .iter()
            .position(|c| c.eq_ignore_ascii_case(column))
            .ok_or_else(|| PersistenceError::ColumnNotFound(column.to_string()))?;
        Ok(&self.values[idx])
    }

    /// Retrieves a reference to the raw [`DbValue`] by zero-based index.
    pub fn get_value_by_index(&self, index: usize) -> Result<&DbValue, PersistenceError> {
        self.values.get(index).ok_or_else(|| {
            PersistenceError::QueryFailed(format!(
                "column index {index} out of bounds (row has {} columns)",
                self.values.len()
            ))
        })
    }

    /// Extracts and converts a column value into type `T`.
    pub fn get<T: FromDbValue>(&self, column: &str) -> Result<T, PersistenceError> {
        let val = self.get_value(column)?;
        T::from_db_value(val).map_err(|e| match e {
            PersistenceError::DecodeError { message, .. } => PersistenceError::DecodeError {
                column: column.to_string(),
                message,
            },
            other => other,
        })
    }

    /// Extracts and converts a column value by zero-based index.
    pub fn get_by_index<T: FromDbValue>(&self, index: usize) -> Result<T, PersistenceError> {
        let col_name = self.columns.get(index).cloned().unwrap_or_else(|| index.to_string());
        let val = self.get_value_by_index(index)?;
        T::from_db_value(val).map_err(|e| match e {
            PersistenceError::DecodeError { message, .. } => PersistenceError::DecodeError {
                column: col_name,
                message,
            },
            other => other,
        })
    }

    /// Extracts an optional column value.
    ///
    /// Returns `Ok(None)` if the column exists in the row and has a SQL NULL value.
    /// Returns `Err(PersistenceError::ColumnNotFound)` if the column does not exist in the row schema.
    pub fn get_opt<T: FromDbValue>(&self, column: &str) -> Result<Option<T>, PersistenceError> {
        match self.get_value(column)? {
            DbValue::Null => Ok(None),
            other => T::from_db_value(other).map(Some).map_err(|e| match e {
                PersistenceError::DecodeError { message, .. } => PersistenceError::DecodeError {
                    column: column.to_string(),
                    message,
                },
                other => other,
            }),
        }
    }
}

/// Trait implemented by application models that can be decoded from a generic [`DbRow`].
pub trait FromRow: Sized {
    /// Decodes an instance of `Self` from a database row.
    fn from_row(row: &DbRow) -> Result<Self, PersistenceError>;
}

impl FromRow for DbRow {
    fn from_row(row: &DbRow) -> Result<Self, PersistenceError> {
        Ok(row.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_column_not_found_in_get_and_get_opt() {
        let row = DbRow::new(
            vec!["id".into(), "name".into(), "description".into()],
            vec![
                DbValue::Int(42),
                DbValue::Text("test".into()),
                DbValue::Null,
            ],
        );

        // Missing column returns ColumnNotFound
        let missing_err = row.get::<String>("non_existent").unwrap_err();
        match missing_err {
            PersistenceError::ColumnNotFound(col) => assert_eq!(col, "non_existent"),
            other => panic!("expected ColumnNotFound, got {other:?}"),
        }

        // Missing column in get_opt also returns ColumnNotFound
        let missing_opt_err = row.get_opt::<String>("non_existent").unwrap_err();
        match missing_opt_err {
            PersistenceError::ColumnNotFound(col) => assert_eq!(col, "non_existent"),
            other => panic!("expected ColumnNotFound, got {other:?}"),
        }

        // Present null column returns Ok(None)
        let null_val: Option<String> = row.get_opt("description").unwrap();
        assert_eq!(null_val, None);

        // Present value returns Ok(Some(val))
        let id_val: Option<i64> = row.get_opt("id").unwrap();
        assert_eq!(id_val, Some(42));
    }

    #[test]
    fn test_lossless_numeric_bounds_checking() {
        // i64::MAX out of range for i32
        let overflow_row = DbRow::new(vec!["big".into()], vec![DbValue::Int(i64::MAX)]);
        let err = overflow_row.get::<i32>("big").unwrap_err();
        match err {
            PersistenceError::DecodeError { column, message } => {
                assert_eq!(column, "big");
                assert!(message.contains("out of range for i32"));
            }
            other => panic!("expected DecodeError, got {other:?}"),
        }

        // Negative i64 cannot be converted to unsigned
        let neg_row = DbRow::new(vec!["neg".into()], vec![DbValue::Int(-5)]);
        let err = neg_row.get::<u32>("neg").unwrap_err();
        match err {
            PersistenceError::DecodeError { column, message } => {
                assert_eq!(column, "neg");
                assert!(message.contains("cannot be converted to u64"));
            }
            other => panic!("expected DecodeError, got {other:?}"),
        }

        // Value exceeding u32 range
        let big_u32_row = DbRow::new(vec!["big_u32".into()], vec![DbValue::Text(format!("{}", u64::MAX))]);
        let err = big_u32_row.get::<u32>("big_u32").unwrap_err();
        match err {
            PersistenceError::DecodeError { column, message } => {
                assert_eq!(column, "big_u32");
                assert!(message.contains("out of range for u32"));
            }
            other => panic!("expected DecodeError, got {other:?}"),
        }

        // Valid conversion succeeds
        let valid_row = DbRow::new(vec!["num".into()], vec![DbValue::Int(120)]);
        let val: u8 = valid_row.get("num").unwrap();
        assert_eq!(val, 120);
    }
}
