use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

pub mod code {
    pub const ANIMATION_LIMIT_EXCEEDED: &str = "animation_limit_exceeded";
    pub const CACHE_LIMIT_EXCEEDED: &str = "cache_limit_exceeded";
    pub const DECODE_DEADLINE_EXCEEDED: &str = "decode_deadline_exceeded";
    pub const DECODER_PANIC: &str = "decoder_panic";
    pub const INVALID_PATH: &str = "invalid_path";
    pub const IO_ERROR: &str = "io_error";
    pub const CORRUPT_IMAGE: &str = "corrupt_image";
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewerError {
    pub code: String,
    pub message: String,
    pub parameters: BTreeMap<String, Value>,
}

impl ViewerError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
            parameters: BTreeMap::new(),
        }
    }

    pub fn with_parameter(mut self, key: &'static str, value: impl Into<Value>) -> Self {
        self.parameters.insert(key.to_owned(), value.into());
        self
    }

    pub fn invalid_path(message: impl Into<String>) -> Self {
        Self::new(code::INVALID_PATH, message)
    }

    pub fn io(message: impl Into<String>) -> Self {
        Self::new(code::IO_ERROR, message)
    }

    pub fn corrupt(message: impl Into<String>) -> Self {
        Self::new(code::CORRUPT_IMAGE, message)
    }

    pub fn limit(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(code, message)
    }

    pub fn deadline_exceeded(limit_ms: u64) -> Self {
        Self::new(code::DECODE_DEADLINE_EXCEEDED, "圖片解碼超過安全期限。")
            .with_parameter("limitMs", limit_ms)
    }

    pub fn decoder_panic() -> Self {
        Self::new(code::DECODER_PANIC, "圖片解碼器發生內部錯誤。")
    }
}

impl std::fmt::Display for ViewerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ViewerError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_always_serialize_a_structured_parameters_object() {
        let plain = serde_json::to_value(ViewerError::io("read failed")).unwrap();
        assert_eq!(plain["code"], code::IO_ERROR);
        assert_eq!(plain["parameters"], serde_json::json!({}));

        let deadline = serde_json::to_value(ViewerError::deadline_exceeded(30_000)).unwrap();
        assert_eq!(deadline["code"], code::DECODE_DEADLINE_EXCEEDED);
        assert_eq!(deadline["parameters"]["limitMs"], 30_000);
    }
}
