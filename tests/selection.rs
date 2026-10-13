use file_organizer::selection::Selection;

#[test]
fn click_toggle_range_and_clear() {
    let mut s = Selection::default();
    s.click(2, false, false, 8);
    assert_eq!(s.selected.iter().copied().collect::<Vec<_>>(), vec![2]);
    s.click(4, true, false, 8);
    assert_eq!(s.anchor, Some(4));
    s.click(4, true, false, 8);
    assert_eq!(s.selected.iter().copied().collect::<Vec<_>>(), vec![2]);
    s.click(6, false, true, 8);
    assert_eq!(
        s.selected.iter().copied().collect::<Vec<_>>(),
        vec![4, 5, 6]
    );
    s.click(0, true, true, 8);
    assert_eq!(
        s.selected.iter().copied().collect::<Vec<_>>(),
        vec![0, 1, 2, 3, 4, 5, 6]
    );
    s.click(100, false, false, 8);
    assert_eq!(s.anchor, Some(4));
    s.clear();
    assert!(s.selected.is_empty());
    assert_eq!(s.anchor, None);
}
