//! A title DLL that cannot be loaded is named in the error.
#![cfg(windows)]

#[test]
fn a_title_dll_that_does_not_prepare_is_named() {
    let title = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(title.path().join("broken.dll"), b"MZ not a PE").unwrap();
    let wanted = |_: &str| true;
    let loaded =
        victory_loader::load_title_dll("broken.dll", &[title.path()], cache.path(), &wanted);
    let Err(error) = loaded else {
        panic!("a DLL that is not a PE loaded");
    };
    let text = format!("{error:#}");
    assert!(text.contains("broken.dll"), "{text}");
}

#[test]
fn a_title_dll_is_found_in_a_later_folder() {
    let exe = tempfile::tempdir().unwrap();
    let dump = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    // An exe kept apart from its dump: the DLLs live with the dump only.
    std::fs::copy(
        r"C:\Windows\System32\version.dll",
        dump.path().join("late.dll"),
    )
    .unwrap();
    let loaded = victory_loader::load_title_dll(
        "late.dll",
        &[exe.path(), dump.path()],
        cache.path(),
        &|_| true,
    )
    .unwrap();
    let names: Vec<_> = loaded.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["late.dll"]);
}
