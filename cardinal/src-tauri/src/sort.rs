pub use search_cache::{SortDirection as SortDirectionPayload, SortKey as SortKeyPayload};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SortStatePayload {
    pub key: SortKeyPayload,
    pub direction: SortDirectionPayload,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_sort_keys_and_directions_deserialize() {
        for key in ["filename", "fullPath", "size", "mtime", "ctime"] {
            for direction in ["asc", "desc"] {
                let value = serde_json::json!({ "key": key, "direction": direction });
                serde_json::from_value::<SortStatePayload>(value).unwrap();
            }
        }
    }
}
