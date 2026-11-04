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
        i64::from_db_value(val).map(|v| v as i32)
    }
}

impl FromDbValue for i16 {
    fn from_db_value(val: &DbValue) -> Result<Self, PersistenceError> {
        i64::from_db_value(val).map(|v| v as i16)
    }
}

impl FromDbValue for i8 {
    fn from_db_value(val: &DbValue) -> Result<Self, PersistenceError> {
        i64::from_db_value(val).map(|v| v as i8)
    }
}

impl FromDbValue for u64 {
    fn from_db_value(val: &DbValue) -> Result<Self, PersistenceError> {
        i64::from_db_value(val).map(|v| v as u64)
    }
}

impl FromDbValue for u32 {
    fn from_db_value(val: &DbValue) -> Result<Self, PersistenceError> {
        i64::from_db_value(val).map(|v| v as u32)
    }
}

impl FromDbValue for u16 {
    fn from_db_value(val: &DbValue) -> Result<Self, PersistenceError> {
        i64::from_db_value(val).map(|v| v as u16)
    }
}

impl FromDbValue for u8 {
    fn from_db_value(val: &DbValue) -> Result<Self, PersistenceError> {
        i64::from_db_value(val).map(|v| v as u8)
    }
}

impl FromDbValue for usize {
    fn from_db_value(val: &DbValue) -> Result<Self, PersistenceError> {
        i64::from_db_value(val).map(|v| v as usize)
    }
}

impl FromDbValue for isize {
    fn from_db_value(val: &DbValue) -> Result<Self, PersistenceError> {
        i64::from_db_value(val).map(|v| v as isize)
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
        f64::from_db_value(val).map(|f| f as f32)
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
            .ok_or_else(|| {
                PersistenceError::QueryFailed(format!("column '{column}' not found in result row"))
            })?;
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
        T::from_db_value(val)
    }

    /// Extracts and converts a column value by zero-based index.
    pub fn get_by_index<T: FromDbValue>(&self, index: usize) -> Result<T, PersistenceError> {
        let val = self.get_value_by_index(index)?;
        T::from_db_value(val)
    }

    /// Extracts an optional column value, treating missing or NULL values as `None`.
    pub fn get_opt<T: FromDbValue>(&self, column: &str) -> Result<Option<T>, PersistenceError> {
        match self.get_value(column) {
            Ok(DbValue::Null) => Ok(None),
            Ok(other) => T::from_db_value(other).map(Some),
            Err(_) => Ok(None),
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
