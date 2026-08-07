use oatmealf::process::find_exactly_one;
use oatmealf::{capture, dump, hash};
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
#[ignore = "requires one running UnrealEditor.exe"]
fn captures_and_validates_running_editor() {
    let editor = find_exactly_one("UnrealEditor.exe", "editor").unwrap();
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!("oatmealf-live-{unique}"));
    fs::create_dir(&directory).unwrap();
    let output = directory.join("editor-layouts.json");
    let captured = capture::run(&editor, &output).unwrap();
    let dump = dump::read(&output, 100).unwrap();
    let validated = hash::validate(&dump).unwrap();
    assert_eq!(captured, validated);
    println!(
        "validated {validated} live editor layouts at {}",
        output.display()
    );
    fs::remove_dir_all(directory).unwrap();
}
