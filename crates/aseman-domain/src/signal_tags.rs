//! Validated signal tags and bounded logical log queries.

use thiserror::Error;

pub const TAG_SEP: char = '|';
pub const MAX_TAG_LEN: usize = 128;
pub const MAX_TAGS: usize = 24;

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum SignalTagError {
    #[error("empty tag")]
    Empty,
    #[error("tag longer than {MAX_TAG_LEN} bytes: {0}")]
    TooLong(String),
    #[error("tag contains unsupported character {character:?}: {tag}")]
    UnsupportedCharacter { character: char, tag: String },
    #[error("more than {MAX_TAGS} tags on one signal")]
    TooMany,
}

fn is_tag_char(character: char) -> bool {
    character.is_ascii_alphanumeric()
        || matches!(character, '=' | '@' | '.' | ':' | '-' | '/' | '+' | '#')
}

pub fn validate_tag(tag: &str) -> Result<String, SignalTagError> {
    let trimmed = tag.trim();
    if trimmed.is_empty() {
        return Err(SignalTagError::Empty);
    }
    if trimmed.len() > MAX_TAG_LEN {
        return Err(SignalTagError::TooLong(trimmed.to_owned()));
    }
    if let Some(character) = trimmed.chars().find(|value| !is_tag_char(*value)) {
        return Err(SignalTagError::UnsupportedCharacter {
            character,
            tag: trimmed.to_owned(),
        });
    }
    Ok(trimmed.to_owned())
}

pub fn validate_tags(tags: &[String]) -> Result<Vec<String>, SignalTagError> {
    if tags.len() > MAX_TAGS {
        return Err(SignalTagError::TooMany);
    }
    let mut output = Vec::with_capacity(tags.len());
    for raw in tags {
        let tag = validate_tag(raw)?;
        if !output.contains(&tag) {
            output.push(tag);
        }
    }
    Ok(output)
}

#[must_use]
pub fn encode_tags(tags: &[String]) -> String {
    if tags.is_empty() {
        return String::new();
    }
    let mut output = String::with_capacity(tags.iter().map(|tag| tag.len() + 1).sum::<usize>() + 1);
    output.push(TAG_SEP);
    for tag in tags {
        output.push_str(tag);
        output.push(TAG_SEP);
    }
    output
}

#[must_use]
pub fn decode_tags(encoded: &str) -> Vec<String> {
    encoded
        .split(TAG_SEP)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct LogQuery {
    pub tags_all: Vec<String>,
    pub tags_any: Vec<String>,
    pub before_time: i64,
    pub after_time: i64,
    pub count: i64,
}

impl LogQuery {
    pub fn validated(mut self) -> Result<Self, SignalTagError> {
        self.tags_all = validate_tags(&self.tags_all)?;
        self.tags_any = validate_tags(&self.tags_any)?;
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_round_trip_deduplicate_and_preserve_whole_match() {
        let tags = validate_tags(&[
            "kind=message".into(),
            "kind=message".into(),
            "thread=main".into(),
        ])
        .unwrap();
        assert_eq!(tags.len(), 2);
        let encoded = encode_tags(&tags);
        assert_eq!(decode_tags(&encoded), tags);
        assert!(!encode_tags(&["thread=main2".into()]).contains("|thread=main|"));
    }

    #[test]
    fn injection_characters_and_unbounded_lists_are_rejected() {
        for value in ["a|b", "a'b", "a%b", "a_b", "a b"] {
            assert!(validate_tag(value).is_err());
        }
        let too_many: Vec<String> = (0..=MAX_TAGS).map(|index| format!("t{index}")).collect();
        assert_eq!(validate_tags(&too_many), Err(SignalTagError::TooMany));
    }
}
