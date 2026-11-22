//! Ignored integration test against a real full-memory Windows minidump (.DMP) of a process that runs UE 5.7.4.
//! The dump must contain the engine's GPUCulling uniform buffer layout. The path comes from the environment:
//! `OATMEALF_TEST_MINIDUMP=<dump> cargo test --release --test real_minidump -- --ignored`

use oatmealf::capture;

#[test]
#[ignore]
fn real_minidump_contains_gpuculling_layout() {
    let dump = std::path::PathBuf::from(
        std::env::var_os("OATMEALF_TEST_MINIDUMP")
            .expect("OATMEALF_TEST_MINIDUMP must name a full-memory minidump (.DMP) file"),
    );
    let out = std::env::temp_dir().join("oatmealf-real-minidump-layouts.json");
    capture::run_from_dump(&dump, &out, false).unwrap();
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(text.contains("GPUCulling"));
    std::fs::remove_file(out).ok();
}
