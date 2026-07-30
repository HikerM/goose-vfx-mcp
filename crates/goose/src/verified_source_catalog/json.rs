use std::collections::BTreeMap;

use anyhow::{anyhow, bail, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AstValue {
    Object(BTreeMap<String, AstValue>),
    Array(Vec<AstValue>),
    String(String),
    Integer(i64),
    Bool(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParserLimits {
    pub max_bytes: usize,
    pub max_depth: usize,
    pub max_total_fields: usize,
    pub max_array_items: usize,
    pub max_string_bytes: usize,
}

impl Default for ParserLimits {
    fn default() -> Self {
        Self {
            max_bytes: 256 * 1024,
            max_depth: 16,
            max_total_fields: 4096,
            max_array_items: 2048,
            max_string_bytes: 4096,
        }
    }
}

pub fn parse_json_bytes(raw: &[u8], limits: ParserLimits) -> Result<AstValue> {
    if raw.len() > limits.max_bytes {
        bail!("JSON document exceeds byte limit");
    }
    if raw.starts_with(&[0xEF, 0xBB, 0xBF]) {
        bail!("JSON BOM is not allowed");
    }
    let text = std::str::from_utf8(raw).map_err(|_| anyhow!("JSON must be valid UTF-8"))?;
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
        bail!("Trailing content after JSON value");
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
            '\u{0C}' => out.extend_from_slice(br#"\f"#),
            '\n' => out.extend_from_slice(br#"\n"#),
            '\r' => out.extend_from_slice(br#"\r"#),
            '\t' => out.extend_from_slice(br#"\t"#),
            c if c <= '\u{1F}' => {
                let escaped = format!("\\u{:04x}", c as u32);
                out.extend_from_slice(escaped.as_bytes());
            }
            other => {
                let mut buf = [0u8; 4];
                let encoded = other.encode_utf8(&mut buf);
                out.extend_from_slice(encoded.as_bytes());
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
    fn parse_value(&mut self, depth: usize) -> Result<AstValue> {
        if depth > self.limits.max_depth {
            bail!("JSON nesting exceeds depth limit");
        }
        self.skip_whitespace();
        let ch = self
            .peek_char()
            .ok_or_else(|| anyhow!("Unexpected end of JSON input"))?;
        match ch {
            '{' => self.parse_object(depth + 1),
            '[' => self.parse_array(depth + 1),
            '"' => Ok(AstValue::String(self.parse_string()?)),
            't' => {
                self.consume_literal("true")?;
                Ok(AstValue::Bool(true))
            }
            'f' => {
                self.consume_literal("false")?;
                Ok(AstValue::Bool(false))
            }
            'n' => bail!("JSON null values are not allowed"),
            '-' | '0'..='9' => Ok(AstValue::Integer(self.parse_integer()?)),
            _ => bail!("Unexpected JSON token"),
        }
    }

    fn parse_object(&mut self, depth: usize) -> Result<AstValue> {
        self.expect_char('{')?;
        self.skip_whitespace();
        let mut map = BTreeMap::new();
        if self.consume_if('}') {
            return Ok(AstValue::Object(map));
        }
        loop {
            self.skip_whitespace();
            let key = self.parse_string()?;
            self.total_fields += 1;
            if self.total_fields > self.limits.max_total_fields {
                bail!("JSON field count exceeds limit");
            }
            if map.contains_key(&key) {
                bail!("Duplicate JSON object key: {key}");
            }
            self.skip_whitespace();
            self.expect_char(':')?;
            let value = self.parse_value(depth)?;
            map.insert(key, value);
            self.skip_whitespace();
            if self.consume_if('}') {
                return Ok(AstValue::Object(map));
            }
            self.expect_char(',')?;
        }
    }

    fn parse_array(&mut self, depth: usize) -> Result<AstValue> {
        self.expect_char('[')?;
        self.skip_whitespace();
        let mut items = Vec::new();
        if self.consume_if(']') {
            return Ok(AstValue::Array(items));
        }
        loop {
            if items.len() >= self.limits.max_array_items {
                bail!("JSON array length exceeds limit");
            }
            items.push(self.parse_value(depth)?);
            self.skip_whitespace();
            if self.consume_if(']') {
                return Ok(AstValue::Array(items));
            }
            self.expect_char(',')?;
        }
    }

    fn parse_string(&mut self) -> Result<String> {
        self.expect_char('"')?;
        let mut value = String::new();
        loop {
            let ch = self
                .next_char()
                .ok_or_else(|| anyhow!("Unterminated JSON string"))?;
            match ch {
                '"' => {
                    if value.len() > self.limits.max_string_bytes {
                        bail!("JSON string exceeds size limit");
                    }
                    return Ok(value);
                }
                '\\' => value.push(self.parse_escape()?),
                c if c <= '\u{1F}' => bail!("Control characters are not allowed in JSON strings"),
                c => value.push(c),
            }
            if value.len() > self.limits.max_string_bytes {
                bail!("JSON string exceeds size limit");
            }
        }
    }

    fn parse_escape(&mut self) -> Result<char> {
        let escape = self
            .next_char()
            .ok_or_else(|| anyhow!("Invalid JSON escape"))?;
        match escape {
            '"' => Ok('"'),
            '\\' => Ok('\\'),
            '/' => Ok('/'),
            'b' => Ok('\u{08}'),
            'f' => Ok('\u{0C}'),
            'n' => Ok('\n'),
            'r' => Ok('\r'),
            't' => Ok('\t'),
            'u' => self.parse_unicode_escape(),
            _ => bail!("Invalid JSON escape"),
        }
    }

    fn parse_unicode_escape(&mut self) -> Result<char> {
        let first = self.parse_hex_quad()?;
        if !(0xD800..=0xDFFF).contains(&first) {
            return char::from_u32(first).ok_or_else(|| anyhow!("Invalid unicode escape"));
        }
        if !(0xD800..=0xDBFF).contains(&first) {
            bail!("Dangling low surrogate is not allowed");
        }
        self.expect_char('\\')?;
        self.expect_char('u')?;
        let second = self.parse_hex_quad()?;
        if !(0xDC00..=0xDFFF).contains(&second) {
            bail!("Invalid unicode surrogate pair");
        }
        let code_point = 0x10000 + (((first - 0xD800) << 10) | (second - 0xDC00));
        char::from_u32(code_point).ok_or_else(|| anyhow!("Invalid unicode escape"))
    }

    fn parse_hex_quad(&mut self) -> Result<u32> {
        let mut value = 0u32;
        for _ in 0..4 {
            let ch = self
                .next_char()
                .ok_or_else(|| anyhow!("Truncated unicode escape"))?;
            let digit = ch
                .to_digit(16)
                .ok_or_else(|| anyhow!("Invalid unicode escape"))?;
            value = (value << 4) | digit;
        }
        Ok(value)
    }

    fn parse_integer(&mut self) -> Result<i64> {
        let start = self.index;
        if self.consume_if('-') {
            let next = self
                .peek_char()
                .ok_or_else(|| anyhow!("Invalid integer literal"))?;
            if !next.is_ascii_digit() {
                bail!("Invalid integer literal");
            }
        }
        if self.consume_if('0') {
            if matches!(self.peek_char(), Some('0'..='9')) {
                bail!("Leading zeroes are not allowed");
            }
        } else {
            self.consume_digits();
        }
        match self.peek_char() {
            Some('.') | Some('e') | Some('E') => bail!("Only integer JSON numbers are allowed"),
            _ => {}
        }
        let slice = &self.text[start..self.index];
        slice
            .parse::<i64>()
            .map_err(|_| anyhow!("Integer literal is out of range"))
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
        let actual = self
            .next_char()
            .ok_or_else(|| anyhow!("Unexpected end of JSON input"))?;
        if actual != expected {
            bail!("Expected JSON character `{expected}`");
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
        self.text[self.index..].chars().next()
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
