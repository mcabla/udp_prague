//! Simple JSON lines file writer.
//!
//! The public API stays close to `json_writer.h` from the reference C++ code,
//! but the serialization backend uses `serde_json` so strings are escaped
//! correctly while preserving the existing JSON-lines schema.

use serde::{ser::SerializeMap, Serializer as _};
use serde_json::{Serializer as JsonSerializer, Value};
use std::fs::{File, OpenOptions};
use std::io::{self, Write};

/// Schema-preserving JSON-lines writer.
///
/// The original project serializes every value as a JSON string. We preserve
/// that externally visible schema for compatibility, while delegating escaping
/// and encoding to `serde_json`.
#[allow(non_camel_case_types)]
#[derive(Debug)]
pub struct json_writer {
    /// Buffer holding the finalized JSON object (without trailing newline).
    pub buf: String,
    /// Output file path.
    pub file: String,
    /// Whether the next field is the first field in the object.
    pub first: bool,
    fields: Vec<(String, Value)>,
}

impl json_writer {
    /// Create a new writer (uninitialized).
    pub fn new() -> Self {
        Self {
            buf: String::new(),
            file: String::new(),
            first: true,
            fields: Vec::new(),
        }
    }

    /// Initialize the output file path.
    ///
    /// If `append` is `false`, the file is truncated.
    pub fn init(&mut self, filename: &str, append: bool) -> io::Result<()> {
        if filename.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "empty filename",
            ));
        }
        self.file.clear();
        self.file.push_str(filename);

        if !append {
            // Truncate/overwrite.
            let _ = File::create(&self.file)?;
        }
        Ok(())
    }

    /// Reset the JSON buffer to begin a new object.
    pub fn reset(&mut self) {
        self.buf.clear();
        self.first = true;
        self.fields.clear();
    }

    #[inline]
    fn sep(&mut self) {
        self.first = false;
    }

    fn push_field(&mut self, key: &str, value: Value) {
        self.sep();
        self.fields.push((key.to_string(), value));
        self.buf.clear();
    }

    fn render(&self) -> io::Result<String> {
        let mut out = Vec::new();
        {
            let mut serializer = JsonSerializer::new(&mut out);
            let mut map = serializer
                .serialize_map(Some(self.fields.len()))
                .map_err(io::Error::other)?;
            for (key, value) in &self.fields {
                map.serialize_entry(key, value).map_err(io::Error::other)?;
            }
            map.end().map_err(io::Error::other)?;
        }
        String::from_utf8(out).map_err(io::Error::other)
    }

    /// Add a string field `k: v`.
    pub fn field_str(&mut self, k: &str, v: &str) {
        self.push_field(k, Value::String(v.to_string()));
    }

    /// Add an unsigned integer field `k: v` serialized as a string.
    pub fn field_u64(&mut self, k: &str, v: u64) {
        self.push_field(k, Value::String(v.to_string()));
    }

    /// Add a signed integer field `k: v` serialized as a string.
    pub fn field_i32(&mut self, k: &str, v: i32) {
        self.push_field(k, Value::String(v.to_string()));
    }

    /// Add a float field `k: v` serialized as a string.
    pub fn field_f32(&mut self, k: &str, v: f32) {
        self.push_field(k, Value::String(format!("{v:.6}")));
    }

    /// Finalize the current object into `buf`.
    pub fn finalize(&mut self) {
        self.buf = self.render().unwrap_or_else(|_| "{}".to_string());
    }

    /// Append the current buffer as a single line to the output file.
    pub fn dump(&mut self) -> io::Result<()> {
        if self.file.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "json_writer not initialized",
            ));
        }
        if self.buf.is_empty() {
            self.buf = self.render()?;
        }
        let mut out = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.file)?;
        out.write_all(self.buf.as_bytes())?;
        out.write_all(b"\n")?;
        Ok(())
    }
}

impl Default for json_writer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::json_writer;

    #[test]
    fn serde_backend_preserves_field_order_and_string_schema() {
        let mut writer = json_writer::new();

        writer.reset();
        writer.field_str("name", "sender");
        writer.field_str("quoted", "line\n\"two\"");
        writer.field_u64("time", 42);
        writer.field_i32("delta", -7);
        writer.field_f32("rate", 1.5);
        writer.finalize();

        assert_eq!(
            writer.buf,
            "{\"name\":\"sender\",\"quoted\":\"line\\n\\\"two\\\"\",\"time\":\"42\",\"delta\":\"-7\",\"rate\":\"1.500000\"}"
        );
    }
}
