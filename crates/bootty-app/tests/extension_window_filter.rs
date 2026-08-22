use bootty_app::ui::extension_window::filtered;
use bootty_extension::ModuleItem;

fn item(key: &str, text: &str) -> ModuleItem {
    ModuleItem {
        text: text.to_owned(),
        key: Some(key.to_owned()),
        ..ModuleItem::default()
    }
}

#[test]
fn filter_matches_item_text_or_key() {
    let items = vec![item("a", "Restart server"), item("b", "Open logs")];
    assert_eq!(filtered(&items, "logs"), vec![1]);
    assert_eq!(filtered(&items, "a"), vec![0]);
    assert_eq!(filtered(&items, ""), vec![0, 1]);
    assert_eq!(filtered(&items, "zzz"), Vec::<usize>::new());
}
