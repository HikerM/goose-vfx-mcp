use std::collections::HashSet;

use agent_client_protocol::RawJsonRpcMessage;

pub(crate) const MAX_ACP_JSON_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MAX_ACP_JSON_DEPTH: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JsonSafetyError {
    DuplicateKey,
    TooDeep,
    TooLarge,
    Invalid,
}

pub(crate) fn accepts_acp_json(input: &[u8]) -> bool {
    !matches!(
        inspect_json(input),
        Err(JsonSafetyError::DuplicateKey | JsonSafetyError::TooDeep | JsonSafetyError::TooLarge)
    )
}

pub(crate) fn parse_acp_json(input: &[u8]) -> Result<RawJsonRpcMessage, JsonSafetyError> {
    inspect_json(input)?;
    serde_json::from_slice(input).map_err(|_| JsonSafetyError::Invalid)
}

pub(crate) fn inspect_json(input: &[u8]) -> Result<(), JsonSafetyError> {
    if input.len() > MAX_ACP_JSON_BYTES {
        return Err(JsonSafetyError::TooLarge);
    }
    if std::str::from_utf8(input).is_err() {
        return Err(JsonSafetyError::Invalid);
    }

    let mut parser = JsonParser { input, offset: 0 };
    parser.skip_whitespace();
    parser.parse_value(0)?;
    parser.skip_whitespace();
    if parser.offset == input.len() {
        Ok(())
    } else {
        Err(JsonSafetyError::Invalid)
    }
}

struct JsonParser<'a> {
    input: &'a [u8],
    offset: usize,
}

impl JsonParser<'_> {
    fn parse_value(&mut self, depth: usize) -> Result<(), JsonSafetyError> {
        if depth >= MAX_ACP_JSON_DEPTH {
            return Err(JsonSafetyError::TooDeep);
        }
        self.skip_whitespace();
        match self.peek() {
            Some(b'{') => self.parse_object(depth + 1),
            Some(b'[') => self.parse_array(depth + 1),
            Some(b'"') => self.parse_string().map(|_| ()),
            Some(b't') => self.expect_literal(b"true"),
            Some(b'f') => self.expect_literal(b"false"),
            Some(b'n') => self.expect_literal(b"null"),
            Some(b'-' | b'0'..=b'9') => self.parse_number(),
            _ => Err(JsonSafetyError::Invalid),
        }
    }

    fn parse_object(&mut self, depth: usize) -> Result<(), JsonSafetyError> {
        self.take(b'{')?;
        self.skip_whitespace();
        if self.consume(b'}') {
            return Ok(());
        }

        let mut keys = HashSet::new();
        loop {
            self.skip_whitespace();
            let key = self.parse_string()?;
            if !keys.insert(key) {
                return Err(JsonSafetyError::DuplicateKey);
            }
            self.skip_whitespace();
            self.take(b':')?;
            self.parse_value(depth)?;
            self.skip_whitespace();
            if self.consume(b'}') {
                return Ok(());
            }
            self.take(b',')?;
        }
    }

    fn parse_array(&mut self, depth: usize) -> Result<(), JsonSafetyError> {
        self.take(b'[')?;
        self.skip_whitespace();
        if self.consume(b']') {
            return Ok(());
        }
        loop {
            self.parse_value(depth)?;
            self.skip_whitespace();
            if self.consume(b']') {
                return Ok(());
            }
            self.take(b',')?;
        }
    }

    fn parse_string(&mut self) -> Result<String, JsonSafetyError> {
        let start = self.offset;
        self.take(b'"')?;
        loop {
            let byte = self.next().ok_or(JsonSafetyError::Invalid)?;
            match byte {
                b'"' => {
                    return serde_json::from_slice(&self.input[start..self.offset])
                        .map_err(|_| JsonSafetyError::Invalid);
                }
                b'\\' => match self.next().ok_or(JsonSafetyError::Invalid)? {
                    b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => {}
                    b'u' => {
                        for _ in 0..4 {
                            if !self.next().is_some_and(|byte| byte.is_ascii_hexdigit()) {
                                return Err(JsonSafetyError::Invalid);
                            }
                        }
                    }
                    _ => return Err(JsonSafetyError::Invalid),
                },
                0..=0x1f => return Err(JsonSafetyError::Invalid),
                _ => {}
            }
        }
    }

    fn parse_number(&mut self) -> Result<(), JsonSafetyError> {
        self.consume(b'-');
        match self.next() {
            Some(b'0') => {
                if self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                    return Err(JsonSafetyError::Invalid);
                }
            }
            Some(b'1'..=b'9') => self.consume_digits(),
            _ => return Err(JsonSafetyError::Invalid),
        }
        if self.consume(b'.') && !self.consume_digit() {
            return Err(JsonSafetyError::Invalid);
        }
        if self.peek().is_some_and(|byte| matches!(byte, b'e' | b'E')) {
            self.offset += 1;
            self.consume(b'+') || self.consume(b'-');
            if !self.consume_digit() {
                return Err(JsonSafetyError::Invalid);
            }
        }
        Ok(())
    }

    fn consume_digit(&mut self) -> bool {
        let Some(byte) = self.peek() else {
            return false;
        };
        if !byte.is_ascii_digit() {
            return false;
        }
        self.offset += 1;
        self.consume_digits();
        true
    }

    fn consume_digits(&mut self) {
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.offset += 1;
        }
    }

    fn expect_literal(&mut self, literal: &[u8]) -> Result<(), JsonSafetyError> {
        if self.input.get(self.offset..self.offset + literal.len()) == Some(literal) {
            self.offset += literal.len();
            Ok(())
        } else {
            Err(JsonSafetyError::Invalid)
        }
    }

    fn skip_whitespace(&mut self) {
        while self
            .peek()
            .is_some_and(|byte| matches!(byte, b' ' | b'\n' | b'\r' | b'\t'))
        {
            self.offset += 1;
        }
    }

    fn take(&mut self, expected: u8) -> Result<(), JsonSafetyError> {
        if self.consume(expected) {
            Ok(())
        } else {
            Err(JsonSafetyError::Invalid)
        }
    }

    fn consume(&mut self, expected: u8) -> bool {
        if self.peek() != Some(expected) {
            return false;
        }
        self.offset += 1;
        true
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.offset).copied()
    }

    fn next(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.offset += 1;
        Some(byte)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_duplicate_keys_at_every_object_depth() {
        for input in [
            br#"{"jsonrpc":"2.0","jsonrpc":"2.0"}"# as &[u8],
            br#"{"params":{"a":1,"a":2}}"#,
            br#"[{"nested":{"a":1,"a":2}}]"#,
        ] {
            assert_eq!(inspect_json(input), Err(JsonSafetyError::DuplicateKey));
            assert!(!accepts_acp_json(input));
        }
    }

    #[test]
    fn treats_escaped_and_unicode_equivalent_keys_as_duplicates() {
        assert_eq!(
            inspect_json(br#"{"a":1,"\u0061":2}"#),
            Err(JsonSafetyError::DuplicateKey)
        );
        assert_eq!(
            inspect_json("{\"é\":1,\"\\u00e9\":2}".as_bytes()),
            Err(JsonSafetyError::DuplicateKey)
        );
    }

    #[test]
    fn accepts_valid_json_and_then_performs_typed_json_rpc_parsing() {
        let input = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        assert!(accepts_acp_json(input));
        assert!(matches!(
            parse_acp_json(input),
            Ok(RawJsonRpcMessage::Request(_))
        ));
    }

    #[test]
    fn invalid_json_is_left_for_the_protocol_parser_to_report() {
        assert_eq!(
            inspect_json(br#"{"jsonrpc":"#),
            Err(JsonSafetyError::Invalid)
        );
        assert!(accepts_acp_json(br#"{"jsonrpc":"#));
    }

    #[test]
    fn rejects_excessive_nesting_without_reporting_content() {
        let mut input = vec![b'['; MAX_ACP_JSON_DEPTH + 1];
        input.extend(std::iter::repeat_n(b']', MAX_ACP_JSON_DEPTH + 1));
        assert_eq!(inspect_json(&input), Err(JsonSafetyError::TooDeep));
    }

    #[test]
    fn accepts_the_maximum_supported_nesting_depth() {
        let mut input = vec![b'['; MAX_ACP_JSON_DEPTH];
        input.extend(std::iter::repeat_n(b']', MAX_ACP_JSON_DEPTH));
        assert_eq!(inspect_json(&input), Ok(()));
    }

    #[test]
    fn enforces_the_json_byte_limit_at_the_exact_boundary() {
        let mut within_limit = Vec::with_capacity(MAX_ACP_JSON_BYTES);
        within_limit.push(b'"');
        within_limit.extend(std::iter::repeat_n(b'a', MAX_ACP_JSON_BYTES - 2));
        within_limit.push(b'"');
        assert_eq!(within_limit.len(), MAX_ACP_JSON_BYTES);
        assert_eq!(inspect_json(&within_limit), Ok(()));

        let mut over_limit = within_limit;
        over_limit.insert(over_limit.len() - 1, b'a');
        assert_eq!(inspect_json(&over_limit), Err(JsonSafetyError::TooLarge));
    }

    #[test]
    fn accepts_valid_strings_numbers_and_arrays() {
        for input in [
            br#""escaped \"quote\" and \u00e9""# as &[u8],
            br#"-12.5e+3"#,
            br#"[0,-1,1.5,6e-2,true,false,null,{"key":"value"}]"#,
        ] {
            assert_eq!(inspect_json(input), Ok(()));
            assert!(accepts_acp_json(input));
        }
    }
}
