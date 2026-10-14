use file_organizer::search::{
    SearchProgress, SearchRequest, needs_processing, search, search_with_progress,
};

#[test]
fn public_progress_api_rejects_invalid_requests_without_callbacks() {
    let dir = tempfile::tempdir().unwrap();
    let req = SearchRequest {
        root: dir.path().into(),
        query: "query".into(),
        reference: None,
        limit: 0,
    };
    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::<SearchProgress>::new()));
    let collected = events.clone();
    assert!(
        search_with_progress(&req, false, false, move |event| {
            collected.lock().unwrap().push(event);
        })
        .unwrap_err()
        .to_string()
        .contains("positive")
    );
    assert!(events.lock().unwrap().is_empty());
}

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
