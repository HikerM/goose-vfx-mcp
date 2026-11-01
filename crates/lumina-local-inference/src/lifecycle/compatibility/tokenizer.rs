use super::{GgufInspection, GgufValue, PairingIssue, PairingSide, TokenizerIdentityPolicy};
use std::collections::BTreeSet;

fn same_value(left: &GgufValue, right: &GgufValue) -> bool {
    match (left, right) {
        (
            GgufValue::Array {
                element_type: left_type,
                count: left_count,
                encoded_digest: left_digest,
                ..
            },
            GgufValue::Array {
                element_type: right_type,
                count: right_count,
                encoded_digest: right_digest,
                ..
            },
        ) => left_type == right_type && left_count == right_count && left_digest == right_digest,
        _ => left == right,
    }
}

fn is_tokenizer_key(key: &str) -> bool {
    key.starts_with("tokenizer.")
        && key != "tokenizer.chat_template"
        && key != "tokenizer.chat_templates"
        && !key.starts_with("tokenizer.chat_template.")
}

pub(super) fn compare(
    main: &GgufInspection,
    draft: &GgufInspection,
    policy: TokenizerIdentityPolicy,
) -> std::result::Result<(), PairingIssue> {
    if policy == TokenizerIdentityPolicy::DflashMask {
        match draft.architecture() {
            Some("dflash") => {}
            Some(_) => {
                return Err(PairingIssue::InvalidMetadata {
                    side: PairingSide::Dependency,
                    key: "general.architecture".into(),
                })
            }
            None => {
                return Err(PairingIssue::MissingMetadata {
                    side: PairingSide::Dependency,
                    key: "general.architecture".into(),
                })
            }
        }
    }
    let tokens_key = "tokenizer.ggml.tokens";
    let vocab = match main.metadata.get(tokens_key) {
        Some(GgufValue::Array { count, .. }) if *count > 0 => *count,
        _ => {
            return Err(PairingIssue::MissingMetadata {
                side: PairingSide::Main,
                key: tokens_key.into(),
            })
        }
    };
    for (side, header) in [(PairingSide::Main, main), (PairingSide::Dependency, draft)] {
        if header
            .metadata
            .get("tokenizer.ggml.model")
            .and_then(GgufValue::as_str)
            .is_none_or(str::is_empty)
        {
            return Err(PairingIssue::MissingMetadata {
                side,
                key: "tokenizer.ggml.model".into(),
            });
        }
    }
    let mask_key = "tokenizer.ggml.mask_token_id";
    let ignore_draft_mask =
        policy == TokenizerIdentityPolicy::DflashMask && !main.metadata.contains_key(mask_key);
    if ignore_draft_mask {
        if let Some(mask) = draft.metadata.get(mask_key) {
            if mask.as_u64().is_none_or(|mask| mask >= vocab) {
                return Err(PairingIssue::InvalidMetadata {
                    side: PairingSide::Dependency,
                    key: mask_key.into(),
                });
            }
        }
    }
    let keys = main
        .metadata
        .keys()
        .chain(draft.metadata.keys())
        .filter(|key| is_tokenizer_key(key))
        .collect::<BTreeSet<_>>();
    for key in keys {
        if ignore_draft_mask && key == mask_key {
            continue;
        }
        match (main.metadata.get(key), draft.metadata.get(key)) {
            (Some(left), Some(right)) if same_value(left, right) => {}
            (Some(_), Some(_)) => return Err(PairingIssue::TokenizerMismatch { key: key.clone() }),
            (None, _) => {
                return Err(PairingIssue::MissingMetadata {
                    side: PairingSide::Main,
                    key: key.clone(),
                })
            }
            (_, None) => {
                return Err(PairingIssue::MissingMetadata {
                    side: PairingSide::Dependency,
                    key: key.clone(),
                })
            }
        }
    }
    Ok(())
}
