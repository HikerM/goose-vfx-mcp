use std::collections::BTreeSet;

use crate::error::{ErrorCode, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonValue {
    Object(Vec<(String, JsonValue)>),
    Array(Vec<JsonValue>),
    String(String),
    Number(u64),
    Null,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParserLimits {
    pub max_bytes: usize,
    pub max_depth: usize,
    pub max_fields: usize,
    pub max_array_items: usize,
    pub max_string_bytes: usize,
}

impl Default for ParserLimits {
    fn default() -> Self {
        Self {
            max_bytes: 256 * 1024,
            max_depth: 24,
            max_fields: 4096,
            max_array_items: 2048,
            max_string_bytes: 8192,
        }
    }
}

pub fn parse_json_bytes(raw: &[u8]) -> Result<JsonValue> {
    parse_json_bytes_with_limits(raw, ParserLimits::default())
}

pub fn parse_json_bytes_with_limits(raw: &[u8], limits: ParserLimits) -> Result<JsonValue> {
    if raw.len() > limits.max_bytes {
        return Err(ErrorCode::MaxBytesExceeded);
    }
    if raw.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return Err(ErrorCode::Utf8BomNotAllowed);
    }
    let text = std::str::from_utf8(raw).map_err(|_| ErrorCode::InvalidUtf8)?;
    let mut parser = Parser {
        text,
        index: 0,
        limits,
        total_fields: 0,
    };
    parser.skip_whitespace();
    let value = parser.parse_value(0)?;
    parser.skip_whitespace();
    if !parser.is_eof() {
        return Err(ErrorCode::TrailingBytes);
    }
    Ok(value)
}

pub fn write_json_string(out: &mut Vec<u8>, value: &str) {
    out.push(b'"');
    for ch in value.chars() {
        match ch {
            '"' => out.extend_from_slice(br#"\""#),
            '\\' => out.extend_from_slice(br#"\\"#),
            '\u{08}' => out.extend_from_slice(br#"\b"#),
            '\u{0c}' => out.extend_from_slice(br#"\f"#),
            '\n' => out.extend_from_slice(br#"\n"#),
            '\r' => out.extend_from_slice(br#"\r"#),
            '\t' => out.extend_from_slice(br#"\t"#),
            c if c <= '\u{1f}' => {
                let escaped = format!("\\u{:04x}", c as u32);
                out.extend_from_slice(escaped.as_bytes());
            }
            other => {
                let mut buf = [0u8; 4];
                out.extend_from_slice(other.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    out.push(b'"');
}

struct Parser<'a> {
    text: &'a str,
    index: usize,
    limits: ParserLimits,
    total_fields: usize,
}

impl<'a> Parser<'a> {
    fn ensure_depth(&self, depth: usize) -> Result<()> {
        if depth > self.limits.max_depth {
            return Err(ErrorCode::MaxDepthExceeded);
        }
        Ok(())
    }

    fn parse_value(&mut self, depth: usize) -> Result<JsonValue> {
        self.ensure_depth(depth)?;
        self.skip_whitespace();
        match self.peek_char().ok_or(ErrorCode::UnexpectedEnd)? {
            '{' => self.parse_object(depth + 1),
            '[' => self.parse_array(depth + 1),
            '"' => Ok(JsonValue::String(self.parse_string()?)),
            'n' => {
                self.consume_literal("null")?;
                Ok(JsonValue::Null)
            }
            't' | 'f' => Err(ErrorCode::BooleanNotAllowed),
            '-' | '+' | '0'..='9' => Ok(JsonValue::Number(self.parse_u64()?)),
            _ => Err(ErrorCode::UnexpectedToken),
        }
    }

    fn parse_object(&mut self, depth: usize) -> Result<JsonValue> {
        self.ensure_depth(depth)?;
        self.expect_char('{')?;
        self.skip_whitespace();
        let mut seen = BTreeSet::new();
        let mut entries = Vec::new();
        if self.consume_if('}') {
            return Ok(JsonValue::Object(entries));
        }
        loop {
            let key = self.parse_string()?;
            self.total_fields += 1;
            if self.total_fields > self.limits.max_fields {
                return Err(ErrorCode::MaxFieldsExceeded);
            }
            if !seen.insert(key.clone()) {
                return Err(ErrorCode::DuplicateKey);
            }
            self.skip_whitespace();
            self.expect_char(':')?;
            let value = self.parse_value(depth)?;
            entries.push((key, value));
            self.skip_whitespace();
            if self.consume_if('}') {
                return Ok(JsonValue::Object(entries));
            }
            self.expect_char(',')?;
            self.skip_whitespace();
        }
    }

    fn parse_array(&mut self, depth: usize) -> Result<JsonValue> {
        self.ensure_depth(depth)?;
        self.expect_char('[')?;
        self.skip_whitespace();
        let mut items = Vec::new();
        if self.consume_if(']') {
            return Ok(JsonValue::Array(items));
        }
        loop {
            if items.len() >= self.limits.max_array_items {
                return Err(ErrorCode::MaxArrayItemsExceeded);
            }
            items.push(self.parse_value(depth)?);
            self.skip_whitespace();
            if self.consume_if(']') {
                return Ok(JsonValue::Array(items));
            }
            self.expect_char(',')?;
            self.skip_whitespace();
        }
    }

    fn parse_string(&mut self) -> Result<String> {
        self.expect_char('"')?;
        let mut out = String::new();
        loop {
            let ch = self.next_char().ok_or(ErrorCode::UnexpectedEnd)?;
            match ch {
                '"' => {
                    if out.len() > self.limits.max_string_bytes {
                        return Err(ErrorCode::MaxStringBytesExceeded);
                    }
                    return Ok(out);
                }
                '\\' => out.push(self.parse_escape()?),
                c if c <= '\u{1f}' => return Err(ErrorCode::UnexpectedToken),
                c => out.push(c),
            }
            if out.len() > self.limits.max_string_bytes {
                return Err(ErrorCode::MaxStringBytesExceeded);
            }
        }
    }

    fn parse_escape(&mut self) -> Result<char> {
        match self.next_char().ok_or(ErrorCode::UnexpectedEnd)? {
            '"' => Ok('"'),
            '\\' => Ok('\\'),
            '/' => Ok('/'),
            'b' => Ok('\u{08}'),
            'f' => Ok('\u{0c}'),
            'n' => Ok('\n'),
            'r' => Ok('\r'),
            't' => Ok('\t'),
            'u' => self.parse_unicode_escape(),
            _ => Err(ErrorCode::UnexpectedToken),
        }
    }

    fn parse_unicode_escape(&mut self) -> Result<char> {
        let first = self.parse_hex_quad()?;
        if !(0xD800..=0xDFFF).contains(&first) {
            return char::from_u32(first).ok_or(ErrorCode::UnexpectedToken);
        }
        if !(0xD800..=0xDBFF).contains(&first) {
            return Err(ErrorCode::UnexpectedToken);
        }
        self.expect_char('\\')?;
        self.expect_char('u')?;
        let second = self.parse_hex_quad()?;
        if !(0xDC00..=0xDFFF).contains(&second) {
            return Err(ErrorCode::UnexpectedToken);
        }
        let code_point = 0x10000 + (((first - 0xD800) << 10) | (second - 0xDC00));
        char::from_u32(code_point).ok_or(ErrorCode::UnexpectedToken)
    }

    fn parse_hex_quad(&mut self) -> Result<u32> {
        let mut value = 0u32;
        for _ in 0..4 {
            let ch = self.next_char().ok_or(ErrorCode::UnexpectedEnd)?;
            let digit = ch.to_digit(16).ok_or(ErrorCode::UnexpectedToken)?;
            value = (value << 4) | digit;
        }
        Ok(value)
    }

    fn parse_u64(&mut self) -> Result<u64> {
        let start = self.index;
        if self.consume_if('+') || self.consume_if('-') {
            return Err(ErrorCode::InvalidNumber);
        }
        if self.consume_if('0') {
            if matches!(self.peek_char(), Some('0'..='9')) {
                return Err(ErrorCode::InvalidNumber);
            }
        } else {
            let first = self.peek_char().ok_or(ErrorCode::UnexpectedEnd)?;
            if !first.is_ascii_digit() {
                return Err(ErrorCode::InvalidNumber);
            }
            self.consume_digits();
        }
        if matches!(self.peek_char(), Some('.' | 'e' | 'E')) {
            return Err(ErrorCode::InvalidNumber);
        }
        self.text
            .get(start..self.index)
            .ok_or(ErrorCode::InvalidNumber)?
            .parse::<u64>()
            .map_err(|_| ErrorCode::NumberOutOfRange)
    }

    fn consume_digits(&mut self) {
        while matches!(self.peek_char(), Some('0'..='9')) {
            let _ = self.next_char();
        }
    }

    fn consume_literal(&mut self, literal: &str) -> Result<()> {
        for expected in literal.chars() {
            self.expect_char(expected)?;
        }
        Ok(())
    }

    fn expect_char(&mut self, expected: char) -> Result<()> {
        let actual = self.next_char().ok_or(ErrorCode::UnexpectedEnd)?;
        if actual != expected {
            return Err(ErrorCode::UnexpectedToken);
        }
        Ok(())
    }

    fn consume_if(&mut self, expected: char) -> bool {
        if self.peek_char() == Some(expected) {
            let _ = self.next_char();
            true
        } else {
            false
        }
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek_char(), Some(' ' | '\n' | '\r' | '\t')) {
            let _ = self.next_char();
        }
    }

    fn peek_char(&self) -> Option<char> {
        self.text.get(self.index..)?.chars().next()
    }

    fn next_char(&mut self) -> Option<char> {
        let ch = self.peek_char()?;
        self.index += ch.len_utf8();
        Some(ch)
    }

    fn is_eof(&self) -> bool {
        self.index >= self.text.len()
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_json_bytes, parse_json_bytes_with_limits, ParserLimits};
    use crate::error::ErrorCode;

    #[test]
    fn parser_rejects_duplicate_keys_and_trailing_bytes() {
        assert_eq!(
            parse_json_bytes(br#"{"a":1,"a":2}"#).unwrap_err(),
            ErrorCode::DuplicateKey
        );
        assert_eq!(
            parse_json_bytes(br#"{"a":1}x"#).unwrap_err(),
            ErrorCode::TrailingBytes
        );
    }

    #[test]
    fn parser_returns_distinct_limit_errors() {
        assert_eq!(
            parse_json_bytes_with_limits(
                br#"{"a":1}"#,
                ParserLimits {
                    max_bytes: 4,
                    ..ParserLimits::default()
                }
            )
            .unwrap_err(),
            ErrorCode::MaxBytesExceeded
        );
        assert_eq!(
            parse_json_bytes_with_limits(
                nested_empty_arrays(3).as_bytes(),
                ParserLimits {
                    max_depth: 2,
                    ..ParserLimits::default()
                }
            )
            .unwrap_err(),
            ErrorCode::MaxDepthExceeded
        );
        assert_eq!(
            parse_json_bytes_with_limits(
                br#"{"a":1,"b":2}"#,
                ParserLimits {
                    max_fields: 1,
                    ..ParserLimits::default()
                }
            )
            .unwrap_err(),
            ErrorCode::MaxFieldsExceeded
        );
        assert_eq!(
            parse_json_bytes_with_limits(
                br#"[1,2]"#,
                ParserLimits {
                    max_array_items: 1,
                    ..ParserLimits::default()
                }
            )
            .unwrap_err(),
            ErrorCode::MaxArrayItemsExceeded
        );
        assert_eq!(
            parse_json_bytes_with_limits(
                br#"{"a":"abcd"}"#,
                ParserLimits {
                    max_string_bytes: 3,
                    ..ParserLimits::default()
                }
            )
            .unwrap_err(),
            ErrorCode::MaxStringBytesExceeded
        );
    }

    #[test]
    fn parser_enforces_depth_boundaries_for_empty_containers() {
        let limits = ParserLimits {
            max_depth: 2,
            ..ParserLimits::default()
        };

        assert!(parse_json_bytes_with_limits(nested_empty_arrays(2).as_bytes(), limits).is_ok());
        assert!(parse_json_bytes_with_limits(nested_empty_objects(2).as_bytes(), limits).is_ok());
        assert!(parse_json_bytes_with_limits(br#"[{}]"#, limits).is_ok());

        assert_eq!(
            parse_json_bytes_with_limits(nested_empty_arrays(3).as_bytes(), limits).unwrap_err(),
            ErrorCode::MaxDepthExceeded
        );
        assert_eq!(
            parse_json_bytes_with_limits(nested_empty_objects(3).as_bytes(), limits).unwrap_err(),
            ErrorCode::MaxDepthExceeded
        );
        assert_eq!(
            parse_json_bytes_with_limits(br#"[{"k":[]}]"#, limits).unwrap_err(),
            ErrorCode::MaxDepthExceeded
        );
    }

    fn nested_empty_arrays(depth: usize) -> String {
        let mut text = String::with_capacity(depth * 2);
        for _ in 0..depth {
            text.push('[');
        }
        for _ in 0..depth {
            text.push(']');
        }
        text
    }

    fn nested_empty_objects(depth: usize) -> String {
        assert!(depth >= 1);
        let mut text = String::new();
        for _ in 1..depth {
            text.push_str(r#"{"k":"#);
        }
        text.push_str("{}");
        for _ in 1..depth {
            text.push('}');
        }
        text
    }
}
