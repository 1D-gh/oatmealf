//! Ignored integration test against a real full-memory Windows minidump (.DMP) of a process that runs UE 5.6 or UE 5.7.4.
//! The path comes from the environment; the capture must contain the layout named by `OATMEALF_TEST_MINIDUMP_LAYOUT`
//! (default `View`, which every renderer registers):
//! `OATMEALF_TEST_MINIDUMP=<dump> cargo test --release --test real_minidump -- --ignored`

use oatmealf::capture;

#[test]
#[ignore]
fn real_minidump_capture_contains_layout() {
    let dump = std::path::PathBuf::from(
        std::env::var_os("OATMEALF_TEST_MINIDUMP")
            .expect("OATMEALF_TEST_MINIDUMP must name a full-memory minidump (.DMP) file"),
    );
    let expected =
        std::env::var("OATMEALF_TEST_MINIDUMP_LAYOUT").unwrap_or_else(|_| "View".to_string());
    let out = std::env::temp_dir().join("oatmealf-real-minidump-layouts.json");
    capture::run_from_dump(&dump, &out, false).unwrap();
    let text = std::fs::read_to_string(&out).unwrap();
    std::fs::remove_file(&out).ok();

    let capture: serde_json::Value = serde_json::from_str(&text).unwrap();
    let layouts = capture["layouts"]
        .as_array()
        .expect("capture has a layouts array");
    assert!(!layouts.is_empty(), "the capture has no layouts");
    assert!(
        layouts
            .iter()
            .any(|layout| layout["name"].as_str() == Some(expected.as_str())),
        "the capture has no layout named {expected}"
    );
}
