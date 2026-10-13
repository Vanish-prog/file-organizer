use file_organizer::search::{SearchRequest, needs_processing, search};

#[test]
fn public_search_rejects_invalid_requests_before_spawning() {
    let dir = tempfile::tempdir().unwrap();
    assert!(needs_processing(dir.path()).unwrap());
    let req = SearchRequest {
        root: dir.path().to_path_buf(),
        query: "query".into(),
        reference: None,
        limit: 0,
    };
    assert!(
        search(&req, false, false)
            .unwrap_err()
            .to_string()
            .contains("positive")
    );
}
