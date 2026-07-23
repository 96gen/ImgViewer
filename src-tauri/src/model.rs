use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum NavigationDirection {
    Previous,
    Next,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ViewerStatus {
    Empty,
    Loading,
    Ready,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderDescriptor {
    pub render_id: u64,
    pub mime_type: String,
    pub width: u32,
    pub height: u32,
    pub animated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewerError {
    pub code: String,
    pub message: String,
}

impl ViewerError {
    pub(crate) fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    pub(crate) fn invalid_path(message: impl Into<String>) -> Self {
        Self::new("invalid_path", message)
    }

    pub(crate) fn io(message: impl Into<String>) -> Self {
        Self::new("io_error", message)
    }

    pub(crate) fn corrupt(message: impl Into<String>) -> Self {
        Self::new("corrupt_image", message)
    }

    pub(crate) fn limit(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(code, message)
    }
}

impl std::fmt::Display for ViewerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ViewerError {}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewerSnapshot {
    pub generation: u64,
    pub status: ViewerStatus,
    pub index: Option<usize>,
    pub total: usize,
    pub file_name: Option<String>,
    pub can_previous: bool,
    pub can_next: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub render: Option<RenderDescriptor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ViewerError>,
}

impl ViewerSnapshot {
    pub(crate) fn empty() -> Self {
        Self {
            generation: 0,
            status: ViewerStatus::Empty,
            index: None,
            total: 0,
            file_name: None,
            can_previous: false,
            can_next: false,
            render: None,
            error: None,
        }
    }

    pub(crate) fn loading(generation: u64, index: usize, total: usize, file_name: String) -> Self {
        Self {
            generation,
            status: ViewerStatus::Loading,
            index: Some(index),
            total,
            file_name: Some(file_name),
            can_previous: index > 0,
            can_next: index + 1 < total,
            render: None,
            error: None,
        }
    }

    pub(crate) fn open_error(
        generation: u64,
        file_name: Option<String>,
        error: ViewerError,
    ) -> Self {
        Self {
            generation,
            status: ViewerStatus::Error,
            index: None,
            total: 0,
            file_name,
            can_previous: false,
            can_next: false,
            render: None,
            error: Some(error),
        }
    }
}

#[derive(Debug)]
pub(crate) struct DecodedRender {
    pub bytes: Vec<u8>,
    pub mime_type: &'static str,
    pub width: u32,
    pub height: u32,
    pub animated: bool,
}
