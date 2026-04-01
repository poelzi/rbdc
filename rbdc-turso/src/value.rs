//! Value type for the Turso adapter.
//!
//! Wraps `turso::Value` with type metadata and provides conversions
//! to/from `rbs::Value` matching the SQLite adapter's public behavior.

use rbs::Value;

/// Data type classification for Turso values.
///
/// Thin wrapper around `turso::value::ValueType` that adds the display/name
/// methods needed by `MetaData::column_type()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TursoDataType {
    Null,
    Integer,
    Real,
    Text,
    Blob,
}

impl TursoDataType {
    /// Canonical SQL type name, matching SQLite adapter conventions.
    pub fn name(&self) -> &'static str {
        match self {
            TursoDataType::Null => "NULL",
            TursoDataType::Integer => "INTEGER",
            TursoDataType::Real => "REAL",
            TursoDataType::Text => "TEXT",
            TursoDataType::Blob => "BLOB",
        }
    }
}

impl From<turso::value::ValueType> for TursoDataType {
    fn from(vt: turso::value::ValueType) -> Self {
        match vt {
            turso::value::ValueType::Null => TursoDataType::Null,
            turso::value::ValueType::Integer => TursoDataType::Integer,
            turso::value::ValueType::Real => TursoDataType::Real,
            turso::value::ValueType::Text => TursoDataType::Text,
            turso::value::ValueType::Blob => TursoDataType::Blob,
        }
    }
}

impl From<&turso::Value> for TursoDataType {
    fn from(v: &turso::Value) -> Self {
        match v {
            turso::Value::Null => TursoDataType::Null,
            turso::Value::Integer(_) => TursoDataType::Integer,
            turso::Value::Real(_) => TursoDataType::Real,
            turso::Value::Text(_) => TursoDataType::Text,
            turso::Value::Blob(_) => TursoDataType::Blob,
        }
    }
}

impl std::fmt::Display for TursoDataType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// A Turso value with associated type metadata.
#[derive(Debug, Clone)]
pub struct TursoValue {
    pub(crate) inner: turso::Value,
    pub(crate) data_type: TursoDataType,
}

impl TursoValue {
    /// Create from a `turso::Value`, inferring type from the value itself.
    pub fn new(value: turso::Value) -> Self {
        let data_type = TursoDataType::from(&value);
        Self {
            inner: value,
            data_type,
        }
    }

    /// Create with an explicit data type (e.g. from column metadata).
    pub fn with_type(value: turso::Value, data_type: TursoDataType) -> Self {
        Self {
            inner: value,
            data_type,
        }
    }

    pub fn data_type(&self) -> TursoDataType {
        self.data_type
    }

    pub fn is_null(&self) -> bool {
        matches!(self.inner, turso::Value::Null)
    }

    pub fn as_integer(&self) -> Option<i64> {
        match &self.inner {
            turso::Value::Integer(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_real(&self) -> Option<f64> {
        match &self.inner {
            turso::Value::Real(f) => Some(*f),
            _ => None,
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match &self.inner {
            turso::Value::Text(s) => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn as_blob(&self) -> Option<&[u8]> {
        match &self.inner {
            turso::Value::Blob(b) => Some(b.as_slice()),
            _ => None,
        }
    }
}

/// Convert a `TursoValue` to `rbs::Value`.
///
/// - Null → `Value::Null`
/// - Integer → `Value::I64`
/// - Real → `Value::F64`
/// - Text → `Value::String` (or deserialized JSON when `json_detect` is true)
/// - Blob → `Value::Binary`
///
/// When `json_detect` is `false` (default), all TEXT values become
/// `Value::String` unconditionally. When `true`, TEXT values that look
/// like JSON objects, arrays, or the literal `"null"` are parsed.
pub fn turso_value_to_rbs(tv: &TursoValue, json_detect: bool) -> Value {
    match &tv.inner {
        turso::Value::Null => Value::Null,
        turso::Value::Integer(n) => Value::I64(*n),
        turso::Value::Real(f) => Value::F64(*f),
        turso::Value::Text(s) => {
            if json_detect && is_json_string(s) {
                if let Ok(v) = serde_json::from_str::<Value>(s) {
                    v
                } else {
                    Value::String(s.clone())
                }
            } else {
                Value::String(s.clone())
            }
        }
        turso::Value::Blob(b) => Value::Binary(b.clone()),
    }
}

/// Convert `turso::Value` directly to `rbs::Value` (convenience wrapper).
///
/// JSON detection is disabled — TEXT values always become `Value::String`.
pub fn turso_to_value(v: turso::Value) -> Value {
    turso_value_to_rbs(&TursoValue::new(v), false)
}

/// Convert `rbs::Value` to `turso::Value` for parameter binding.
///
/// Matches the SQLite adapter's `Encode for Value` behavior.
pub fn value_to_turso(v: &Value) -> Result<turso::Value, rbdc::Error> {
    match v {
        Value::Null => Ok(turso::Value::Null),
        Value::Bool(b) => Ok(turso::Value::Integer(if *b { 1 } else { 0 })),
        Value::I32(n) => Ok(turso::Value::Integer(*n as i64)),
        Value::I64(n) => Ok(turso::Value::Integer(*n)),
        Value::U32(n) => Ok(turso::Value::Integer(*n as i64)),
        Value::U64(n) => Ok(turso::Value::Integer(*n as i64)),
        Value::F32(f) => Ok(turso::Value::Real(*f as f64)),
        Value::F64(f) => Ok(turso::Value::Real(*f)),
        Value::String(s) => Ok(turso::Value::Text(s.clone())),
        Value::Binary(b) => Ok(turso::Value::Blob(b.clone())),
        Value::Ext(type_tag, val) => match &**type_tag {
            "Date" | "DateTime" | "Time" | "Decimal" | "Uuid" => Ok(turso::Value::Text(
                val.as_str()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| val.to_string()),
            )),
            "Timestamp" => Ok(turso::Value::Integer(val.as_i64().unwrap_or_default())),
            "Json" => match val.as_ref() {
                Value::Binary(b) => Ok(turso::Value::Blob(b.clone())),
                _ => Ok(turso::Value::Blob(val.to_string().into_bytes())),
            },
            _ => match val.as_ref() {
                Value::String(s) => Ok(turso::Value::Text(s.clone())),
                Value::I64(n) => Ok(turso::Value::Integer(*n)),
                Value::U64(n) => Ok(turso::Value::Integer(*n as i64)),
                Value::F64(f) => Ok(turso::Value::Real(*f)),
                _ => Ok(turso::Value::Text(val.to_string())),
            },
        },
        Value::Array(_) | Value::Map(_) => Ok(turso::Value::Text(
            serde_json::to_string(v).unwrap_or_default(),
        )),
    }
}

/// Check if a string looks like JSON (null, object, or array).
///
/// Same heuristic as the SQLite adapter's `is_json_string`.
pub fn is_json_string(s: &str) -> bool {
    s == "null"
        || (s.starts_with('{') && s.ends_with('}'))
        || (s.starts_with('[') && s.ends_with(']'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_null_roundtrip() {
        let tv = TursoValue::new(turso::Value::Null);
        assert!(tv.is_null());
        assert_eq!(tv.data_type(), TursoDataType::Null);
        assert_eq!(turso_value_to_rbs(&tv, false), Value::Null);
    }

    #[test]
    fn test_integer_roundtrip() {
        let tv = TursoValue::new(turso::Value::Integer(42));
        assert_eq!(tv.data_type(), TursoDataType::Integer);
        assert_eq!(turso_value_to_rbs(&tv, false), Value::I64(42));
    }

    #[test]
    fn test_i64_extremes() {
        assert_eq!(
            turso_value_to_rbs(&TursoValue::new(turso::Value::Integer(i64::MAX)), false),
            Value::I64(i64::MAX)
        );
        assert_eq!(
            turso_value_to_rbs(&TursoValue::new(turso::Value::Integer(i64::MIN)), false),
            Value::I64(i64::MIN)
        );
    }

    #[test]
    fn test_real_roundtrip() {
        let tv = TursoValue::new(turso::Value::Real(3.14));
        assert_eq!(tv.data_type(), TursoDataType::Real);
        assert_eq!(turso_value_to_rbs(&tv, false), Value::F64(3.14));
    }

    #[test]
    fn test_text_plain() {
        let tv = TursoValue::new(turso::Value::Text("hello".into()));
        assert_eq!(
            turso_value_to_rbs(&tv, false),
            Value::String("hello".into())
        );
    }

    #[test]
    fn test_text_json_disabled_by_default() {
        // With json_detect=false, JSON-shaped text stays as String
        let tv = TursoValue::new(turso::Value::Text(r#"{"key":"value"}"#.into()));
        assert_eq!(
            turso_value_to_rbs(&tv, false),
            Value::String(r#"{"key":"value"}"#.into())
        );

        let tv = TursoValue::new(turso::Value::Text("[1,2,3]".into()));
        assert_eq!(
            turso_value_to_rbs(&tv, false),
            Value::String("[1,2,3]".into())
        );

        let tv = TursoValue::new(turso::Value::Text("null".into()));
        assert_eq!(turso_value_to_rbs(&tv, false), Value::String("null".into()));
    }

    #[test]
    fn test_text_json_object_when_enabled() {
        let tv = TursoValue::new(turso::Value::Text(r#"{"key":"value"}"#.into()));
        assert!(matches!(turso_value_to_rbs(&tv, true), Value::Map(_)));
    }

    #[test]
    fn test_text_json_array_when_enabled() {
        let tv = TursoValue::new(turso::Value::Text("[1,2,3]".into()));
        assert!(matches!(turso_value_to_rbs(&tv, true), Value::Array(_)));
    }

    #[test]
    fn test_text_json_null_when_enabled() {
        let tv = TursoValue::new(turso::Value::Text("null".into()));
        assert_eq!(turso_value_to_rbs(&tv, true), Value::Null);
    }

    #[test]
    fn test_blob_roundtrip() {
        let data = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let tv = TursoValue::new(turso::Value::Blob(data.clone()));
        assert_eq!(turso_value_to_rbs(&tv, false), Value::Binary(data));
    }

    #[test]
    fn test_data_type_from_value_type() {
        assert_eq!(
            TursoDataType::from(turso::value::ValueType::Integer),
            TursoDataType::Integer
        );
        assert_eq!(
            TursoDataType::from(turso::value::ValueType::Real),
            TursoDataType::Real
        );
        assert_eq!(
            TursoDataType::from(turso::value::ValueType::Text),
            TursoDataType::Text
        );
        assert_eq!(
            TursoDataType::from(turso::value::ValueType::Blob),
            TursoDataType::Blob
        );
        assert_eq!(
            TursoDataType::from(turso::value::ValueType::Null),
            TursoDataType::Null
        );
    }

    #[test]
    fn test_value_to_turso_basics() {
        assert!(matches!(
            value_to_turso(&Value::Null).unwrap(),
            turso::Value::Null
        ));
        assert!(matches!(
            value_to_turso(&Value::Bool(true)).unwrap(),
            turso::Value::Integer(1)
        ));
        assert!(matches!(
            value_to_turso(&Value::Bool(false)).unwrap(),
            turso::Value::Integer(0)
        ));
        assert!(matches!(
            value_to_turso(&Value::I64(100)).unwrap(),
            turso::Value::Integer(100)
        ));
    }

    #[test]
    fn test_is_json_string_checks() {
        assert!(is_json_string("null"));
        assert!(is_json_string(r#"{"a":1}"#));
        assert!(is_json_string("[1,2]"));
        assert!(!is_json_string("hello"));
        assert!(!is_json_string(""));
    }

    #[test]
    fn test_data_type_display_and_accessors() {
        assert_eq!(format!("{}", TursoDataType::Null), "NULL");

        let int_v = TursoValue::new(turso::Value::Integer(42));
        assert_eq!(int_v.as_integer(), Some(42));
        assert_eq!(int_v.as_real(), None);

        let real_v = TursoValue::new(turso::Value::Real(1.5));
        assert_eq!(real_v.as_real(), Some(1.5));
        assert_eq!(real_v.as_text(), None);

        let text_v = TursoValue::new(turso::Value::Text("abc".into()));
        assert_eq!(text_v.as_text(), Some("abc"));

        let blob_v = TursoValue::new(turso::Value::Blob(vec![1, 2, 3]));
        assert_eq!(blob_v.as_blob(), Some(&[1, 2, 3][..]));
    }

    #[test]
    fn test_turso_to_value_wrapper() {
        assert_eq!(turso_to_value(turso::Value::Integer(7)), Value::I64(7));
    }

    #[test]
    fn test_value_to_turso_ext_date_like_tags() {
        for tag in ["Date", "DateTime", "Time", "Decimal", "Uuid"] {
            let out = value_to_turso(&Value::Ext(
                tag,
                Box::new(Value::String("2026-01-01".to_string())),
            ))
            .unwrap();
            assert!(matches!(out, turso::Value::Text(ref s) if s == "2026-01-01"));
        }
    }

    #[test]
    fn test_value_to_turso_ext_timestamp_and_json() {
        let ts = value_to_turso(&Value::Ext("Timestamp", Box::new(Value::I64(123)))).unwrap();
        assert!(matches!(ts, turso::Value::Integer(123)));

        let ts_default = value_to_turso(&Value::Ext(
            "Timestamp",
            Box::new(Value::String("x".into())),
        ))
        .unwrap();
        assert!(matches!(ts_default, turso::Value::Integer(0)));

        let json_binary = value_to_turso(&Value::Ext(
            "Json",
            Box::new(Value::Binary(vec![0xCA, 0xFE])),
        ))
        .unwrap();
        assert!(matches!(json_binary, turso::Value::Blob(ref b) if b == &vec![0xCA, 0xFE]));

        let json_text = value_to_turso(&Value::Ext(
            "Json",
            Box::new(Value::String("{\"a\":1}".into())),
        ))
        .unwrap();
        assert!(matches!(json_text, turso::Value::Blob(_)));
    }

    #[test]
    fn test_value_to_turso_ext_unknown_and_collections() {
        let e1 = value_to_turso(&Value::Ext("Other", Box::new(Value::String("s".into())))).unwrap();
        assert!(matches!(e1, turso::Value::Text(ref s) if s == "s"));

        let e2 = value_to_turso(&Value::Ext("Other", Box::new(Value::I64(-7)))).unwrap();
        assert!(matches!(e2, turso::Value::Integer(-7)));

        let e3 = value_to_turso(&Value::Ext("Other", Box::new(Value::U64(9)))).unwrap();
        assert!(matches!(e3, turso::Value::Integer(9)));

        let e4 = value_to_turso(&Value::Ext("Other", Box::new(Value::F64(2.5)))).unwrap();
        assert!(matches!(e4, turso::Value::Real(f) if (f - 2.5).abs() < f64::EPSILON));

        let e5 = value_to_turso(&Value::Ext("Other", Box::new(Value::Null))).unwrap();
        assert!(matches!(e5, turso::Value::Text(_)));

        let arr = value_to_turso(&Value::Array(vec![Value::I64(1)])).unwrap();
        assert!(matches!(arr, turso::Value::Text(ref s) if s == "[1]"));

        let map: Value = serde_json::from_str(r#"{"k":1}"#).unwrap();
        let map_out = value_to_turso(&map).unwrap();
        assert!(matches!(map_out, turso::Value::Text(ref s) if s.contains("\"k\"")));
    }
}
