use anyhow::Result;
use serde::Serialize;
use std::io::{self, Write};

fn checked_length(current: usize, additional: usize, limit: usize) -> io::Result<usize> {
    current
        .checked_add(additional)
        .filter(|length| *length <= limit)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Project reuse payload exceeds its byte limit ({limit} bytes)"),
            )
        })
}

struct CountingWriter {
    length: usize,
    limit: usize,
}

impl Write for CountingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.length = checked_length(self.length, bytes.len(), self.limit)?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct BoundedWriter {
    bytes: Vec<u8>,
    limit: usize,
}

impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        checked_length(self.bytes.len(), bytes.len(), self.limit)?;
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn serialized_len(value: &impl Serialize, limit: usize) -> Result<usize> {
    let mut writer = CountingWriter { length: 0, limit };
    serde_json::to_writer(&mut writer, value)?;
    Ok(writer.length)
}

pub(super) fn bounded_json(value: &impl Serialize, limit: usize) -> Result<String> {
    let mut writer = BoundedWriter {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut writer, value)?;
    Ok(String::from_utf8(writer.bytes)?)
}

pub(super) fn canonical_json(value: &impl Serialize, limit: usize) -> Result<String> {
    serialized_len(value, limit)?;
    let mut canonical = serde_json::to_value(value)?;
    canonical.sort_all_objects();
    bounded_json(&canonical, limit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::ser::SerializeSeq;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn exact_encoded_bound_accepts_unicode_and_escaped_nested_json() {
        let nested =
            serde_json::to_string(&json!({"message": "中文 🎵 \"quoted\" \\ path\n"})).unwrap();
        let value = json!({"snapshotJson": nested, "label": "音频"});
        let expected = serde_json::to_string(&value).unwrap();
        let limit = expected.len();

        assert_eq!(serialized_len(&value, limit).unwrap(), limit);
        assert_eq!(bounded_json(&value, limit).unwrap(), expected);
        let mut canonical = value.clone();
        canonical.sort_all_objects();
        assert_eq!(
            canonical_json(&value, limit).unwrap(),
            serde_json::to_string(&canonical).unwrap()
        );
        assert!(serialized_len(&value, limit - 1).is_err());
        assert!(bounded_json(&value, limit - 1).is_err());
        assert!(canonical_json(&value, limit - 1).is_err());
    }

    #[test]
    fn canonical_json_sorts_nested_objects_without_changing_array_order() {
        #[derive(Serialize)]
        struct Fields {
            z: u8,
            a: u8,
        }
        #[derive(Serialize)]
        struct Payload {
            z: Vec<Fields>,
            a: Fields,
        }
        let value = Payload {
            z: vec![Fields { z: 2, a: 1 }, Fields { z: 4, a: 3 }],
            a: Fields { z: 6, a: 5 },
        };
        let expected = r#"{"a":{"a":5,"z":6},"z":[{"a":1,"z":2},{"a":3,"z":4}]}"#;
        assert_eq!(canonical_json(&value, expected.len()).unwrap(), expected);
    }

    #[test]
    fn oversized_custom_serializer_stops_in_preflight_before_value_allocation() {
        struct Oversized<'a> {
            serializations: &'a AtomicUsize,
            elements: &'a AtomicUsize,
        }
        impl Serialize for Oversized<'_> {
            fn serialize<S: serde::Serializer>(
                &self,
                serializer: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                self.serializations.fetch_add(1, Ordering::Relaxed);
                let mut sequence = serializer.serialize_seq(None)?;
                for _ in 0..1000 {
                    self.elements.fetch_add(1, Ordering::Relaxed);
                    sequence.serialize_element("oversized element")?;
                }
                sequence.end()
            }
        }

        let serializations = AtomicUsize::new(0);
        let elements = AtomicUsize::new(0);
        let value = Oversized {
            serializations: &serializations,
            elements: &elements,
        };
        let error = canonical_json(&value, 8).unwrap_err();
        assert!(error
            .to_string()
            .contains("Project reuse payload exceeds its byte limit"));
        assert_eq!(serializations.load(Ordering::Relaxed), 1);
        assert_eq!(elements.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn rejected_writes_leave_count_and_buffer_unchanged() {
        let mut counter = CountingWriter {
            length: usize::MAX - 1,
            limit: usize::MAX,
        };
        assert!(counter.write(b"ab").is_err());
        assert_eq!(counter.length, usize::MAX - 1);

        let mut writer = BoundedWriter {
            bytes: vec![b'a'],
            limit: 2,
        };
        assert!(writer.write(b"bc").is_err());
        assert_eq!(writer.bytes, b"a");
    }
}
