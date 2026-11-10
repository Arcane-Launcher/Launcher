use crate::error::AppResult;
use crate::models::modrinth::RawCollection;

use super::client::ModrinthClient;

/// Fetches a public Modrinth collection (v3-only endpoint) by its id — the
/// short alphanumeric code Modrinth uses in collection URLs, e.g.
/// `https://modrinth.com/collection/YQlDVmqc` -> `YQlDVmqc`.
pub async fn get_collection(client: &ModrinthClient, collection_id: &str) -> AppResult<RawCollection> {
    let path = format!("/collection/{}", urlencoding::encode(collection_id));
    client.get_json_v3(&path).await
}

/// Accepts either a bare collection id or a full Modrinth collection URL and
/// returns just the id, since users will naturally paste the URL from their
/// browser's address bar.
pub fn extract_collection_id(input: &str) -> String {
    let trimmed = input.trim();
    if let Some(idx) = trimmed.find("/collection/") {
        let after = &trimmed[idx + "/collection/".len()..];
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        return after[..end].to_string();
    }
    trimmed.to_string()
}
