use crate::error::AppResult;
use crate::sources::{ModSource, SourceId};
use std::collections::HashMap;
use thunderstore_engine::client::ThunderstoreClient;
use thunderstore_engine::models::PackageIndex;

/// Kept as the Thunderstore-only entry point [`run_multi`] is layered over;
/// `main` always calls `run_multi`.
#[cfg_attr(not(test), allow(dead_code))]
pub async fn run(client: &ThunderstoreClient, term: &str) -> AppResult<()> {
  let manifest = client.get_manifest().await?;

  print_results(&manifest, &HashMap::new(), term)
}

/// [`run`], searching every configured source's merged manifest, or just
/// `source_filter` when one is given, and annotating each hit with `[source]`
/// once more than one source is actually in play — so single-source output
/// stays byte-for-byte what it always was.
pub async fn run_multi(
  sources: &[Box<dyn ModSource>],
  source_filter: Option<SourceId>,
  term: &str,
) -> AppResult<()> {
  let selected: Vec<&dyn ModSource> = match source_filter {
    Some(id) => sources
      .iter()
      .filter(|source| source.id() == id)
      .map(|source| source.as_ref())
      .collect(),
    None => crate::sources::as_refs(sources),
  };

  let (manifest, source_map) = crate::sources::merged_manifest(&selected, false).await?;

  let show_source = selected.len() > 1;
  let source_map = if show_source {
    source_map
  } else {
    HashMap::new()
  };

  print_results(&manifest, &source_map, term)
}

/// Prints every package matching `term`, with a `[source]` suffix per hit when
/// `source_map` is non-empty.
fn print_results(
  manifest: &PackageIndex,
  source_map: &HashMap<String, SourceId>,
  term: &str,
) -> AppResult<()> {
  let results = manifest.search(term);

  if results.is_empty() {
    println!("No mods found matching '{term}'");

    return Ok(());
  }

  println!("Found {} mods matching '{term}':\n", results.len());

  for idx in results {
    let Some(package) = manifest.get_package_at(idx) else {
      continue;
    };

    let latest = package.latest_version();
    let version = latest
      .and_then(|v| v.version_number.clone())
      .unwrap_or_else(|| "Unknown".to_string());
    let description = latest
      .and_then(|v| v.description.clone())
      .unwrap_or_default();
    let identifier = package_identifier(
      package.full_name.as_deref(),
      package.owner.as_deref(),
      package.name.as_deref(),
    );
    let source_suffix = package
      .full_name
      .as_deref()
      .and_then(|name| source_map.get(name))
      .map(|source| format!(" [{source}]"))
      .unwrap_or_default();

    println!("{identifier} ({version}){source_suffix}");

    if !description.is_empty() {
      println!("  {description}");
    }

    println!();
  }

  Ok(())
}

/// The `Owner-ModName` identifier to print for one search result.
///
/// `full_name` already *is* that identifier, so when the index carries one it is
/// used whole rather than treated as a half to prefix. This matters because
/// `PackageIndex::search` matches on either `name` or `full_name`, and
/// `get_package_at` preserves both fields as it found them: a package indexed
/// with only a `full_name` is matched, reaches this loop, and prefixing it with
/// the owner would print `Owner-Owner-ModName`.
///
/// Only when `full_name` is absent is the identifier composed from its two
/// halves, each falling back to `Unknown` so a partial record still prints a
/// recognisable line instead of being dropped.
fn package_identifier(full_name: Option<&str>, owner: Option<&str>, name: Option<&str>) -> String {
  match full_name {
    Some(full_name) => full_name.to_string(),
    None => format!(
      "{}-{}",
      owner.unwrap_or("Unknown"),
      name.unwrap_or("Unknown")
    ),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use mockito::Server;
  use tempfile::tempdir;
  use tokio::runtime::Runtime;

  fn test_manifest_json() -> &'static str {
    r#"[{
      "name": "ModA",
      "full_name": "Owner-ModA",
      "owner": "Owner",
      "package_url": "https://example.com/mods/ModA",
      "date_created": "2024-01-01T12:00:00Z",
      "date_updated": "2024-01-02T12:00:00Z",
      "uuid4": "test-uuid",
      "rating_score": 5,
      "is_pinned": false,
      "is_deprecated": false,
      "has_nsfw_content": false,
      "categories": ["category1"],
      "versions": [{
        "name": "ModA",
        "full_name": "Owner-ModA",
        "description": "Test description",
        "icon": "icon.png",
        "version_number": "1.0.0",
        "dependencies": [],
        "download_url": "https://example.com/mods/ModA/download",
        "downloads": 100,
        "date_created": "2024-01-01T12:00:00Z",
        "website_url": "https://example.com",
        "is_active": true,
        "uuid4": "test-version-uuid",
        "file_size": 1024
      }]
    }]"#
  }

  fn test_client(
    server: &mut mockito::ServerGuard,
    cache_dir: &std::path::Path,
  ) -> ThunderstoreClient {
    let last_modified = "Wed, 21 Feb 2024 15:30:45 GMT";
    server
      .mock("GET", "/c/valheim/api/v1/package/")
      .with_status(200)
      .with_header("Content-Type", "application/json")
      .with_header("Last-Modified", last_modified)
      .with_body(test_manifest_json())
      .create();

    ThunderstoreClient::builder()
      .package_index_url(format!("{}/c/valheim/api/v1/package/", server.url()))
      .cache_dir(cache_dir)
      .build()
      .unwrap()
  }

  #[test]
  fn test_run_search_no_results() {
    let mut server = Server::new();
    let temp_dir = tempdir().unwrap();
    let client = test_client(&mut server, temp_dir.path());

    let rt = Runtime::new().unwrap();
    let result = rt.block_on(run(&client, "nonexistent_mod_xyz"));

    assert!(result.is_ok());
  }

  #[test]
  fn test_run_search_with_results() {
    let mut server = Server::new();
    let temp_dir = tempdir().unwrap();
    let client = test_client(&mut server, temp_dir.path());

    let rt = Runtime::new().unwrap();
    // "moda" matches "ModA" (case-insensitive), exercising the result display loop
    let result = rt.block_on(run(&client, "moda"));

    assert!(result.is_ok());
  }

  #[test]
  fn test_run_search_case_insensitive() {
    let mut server = Server::new();
    let temp_dir = tempdir().unwrap();
    let client = test_client(&mut server, temp_dir.path());

    let rt = Runtime::new().unwrap();
    let result = rt.block_on(run(&client, "MODA"));

    assert!(result.is_ok());
  }

  #[test]
  fn a_full_name_is_the_whole_identifier_not_a_half_to_prefix() {
    // The case `search` can match on `full_name` alone. Composing here would
    // print `Owner-Owner-ModA`, which is not installable and not what the index
    // holds.
    assert_eq!(
      package_identifier(Some("Owner-ModA"), Some("Owner"), None),
      "Owner-ModA"
    );
    // A complete record composes to the same string either way, so preferring
    // `full_name` changes nothing for well-formed packages.
    assert_eq!(
      package_identifier(Some("Owner-ModA"), Some("Owner"), Some("ModA")),
      "Owner-ModA"
    );
  }

  #[test]
  fn a_missing_full_name_is_composed_from_its_halves() {
    assert_eq!(
      package_identifier(None, Some("Owner"), Some("ModA")),
      "Owner-ModA"
    );
    // Neither half is guaranteed, and a partial record is still worth printing
    // rather than silently skipping.
    assert_eq!(package_identifier(None, None, Some("ModA")), "Unknown-ModA");
    assert_eq!(
      package_identifier(None, Some("Owner"), None),
      "Owner-Unknown"
    );
    assert_eq!(package_identifier(None, None, None), "Unknown-Unknown");
  }

  #[test]
  fn run_multi_with_one_source_matches_run() {
    let fixture = crate::test_support::Fixture::new();

    let result = Runtime::new()
      .unwrap()
      .block_on(run_multi(&fixture.sources(), None, "moda"));

    assert!(result.is_ok());
  }

  #[test]
  fn run_multi_searches_every_configured_source_by_default() {
    let fixture = crate::test_support::Fixture::new();

    // Hexium-OnlyMod exists on no other source, so finding it here proves both
    // configured sources were actually searched, not just the first.
    let result =
      Runtime::new()
        .unwrap()
        .block_on(run_multi(&fixture.multi_sources(), None, "onlymod"));

    assert!(result.is_ok());
  }

  #[test]
  fn run_multi_with_source_filter_restricts_to_that_source() {
    let fixture = crate::test_support::Fixture::new();

    // Owner-ModA lives on Thunderstore only, so restricting the search to
    // Hexium finds nothing, exercising the "no results" branch through the
    // filtered path.
    let result = Runtime::new().unwrap().block_on(run_multi(
      &fixture.multi_sources(),
      Some(SourceId::Hexium),
      "moda",
    ));

    assert!(result.is_ok());
  }
}
