use serde::{Deserialize, Serialize};

use crate::error::ViewerError;

pub const PROTOCOL_VERSION: u32 = 1;

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
pub struct ViewerSnapshot {
    pub protocol_version: u32,
    pub revision: u64,
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
            protocol_version: PROTOCOL_VERSION,
            revision: 0,
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

    pub(crate) fn loading(
        generation: u64,
        revision: u64,
        index: usize,
        total: usize,
        file_name: String,
    ) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            revision,
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
        revision: u64,
        file_name: Option<String>,
        error: ViewerError,
    ) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            revision,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_wire_contract_is_versioned_and_camel_case() {
        let value = serde_json::to_value(ViewerSnapshot::empty()).unwrap();
        assert_eq!(value["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(value["revision"], 0);
        assert_eq!(value["generation"], 0);
        assert!(value.get("protocol_version").is_none());
    }
}
