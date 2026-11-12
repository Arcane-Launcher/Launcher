use crate::error::AppResult;
use crate::models::common::{ProjectType, SearchQuery, SearchResult, SortOrder};
use crate::models::modrinth::RawSearchResponse;

use super::client::ModrinthClient;
use super::convert::project_from_search_hit;

fn sort_index(sort: SortOrder) -> &'static str {
    match sort {
        SortOrder::Relevance => "relevance",
        SortOrder::Downloads => "downloads",
        SortOrder::Updated => "updated",
        SortOrder::Newest => "newest",
        SortOrder::Followers => "follows",
    }
}

fn build_facets(query: &SearchQuery) -> String {
    let mut groups: Vec<Vec<String>> = vec![vec![format!(
        "project_type:{}",
        query.project_type.modrinth_facet_value()
    )]];

    if let Some(loader) = &query.loader {
        if !loader.is_empty() && loader != "all" && query.project_type.accepts_loader_filter(loader)
        {
            groups.push(vec![format!("categories:{loader}")]);
        }
    }
    if let Some(version) = &query.game_version {
        if !version.is_empty() && version != "all" {
            groups.push(vec![format!("versions:{version}")]);
        }
    }
    for category in &query.categories {
        groups.push(vec![format!("categories:{category}")]);
    }

    let serialized: Vec<Vec<String>> = groups
        .into_iter()
        .map(|group| group.into_iter().map(|f| format!("\"{f}\"")).collect())
        .collect();
    let inner: Vec<String> = serialized
        .into_iter()
        .map(|group| format!("[{}]", group.join(",")))
        .collect();
    format!("[{}]", inner.join(","))
}

pub async fn search(client: &ModrinthClient, query: &SearchQuery) -> AppResult<SearchResult> {
    let facets = build_facets(query);
    let path = format!(
        "/search?query={}&facets={}&index={}&offset={}&limit={}",
        urlencoding::encode(&query.query),
        urlencoding::encode(&facets),
        sort_index(query.sort),
        query.offset,
        query.limit.clamp(1, 100),
    );
    let raw: RawSearchResponse = client.get_json(&path).await?;
    let hits = raw
        .hits
        .into_iter()
        .map(project_from_search_hit)
        .map(|mut p| {
            // Modrinth has no distinct "datapack" project — the search facet
            // filters to projects that publish a datapack-flavored version, but
            // each hit's own `project_type` still reports its primary type
            // (almost always "mod"). Since this search was explicitly scoped to
            // datapacks, every result is meant to be treated as one here.
            if query.project_type == ProjectType::Datapack {
                p.project_type = ProjectType::Datapack;
            }
            p
        });
    Ok(SearchResult {
        hits: hits.collect(),
        offset: raw.offset,
        limit: raw.limit,
        total_hits: raw.total_hits,
    })
}
