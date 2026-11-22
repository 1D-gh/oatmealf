# oatmealf-5.7 (UE 5.7.4 port + minidump reader)

oatmealf v0.1.13 (upstream usage: [README.md](README.md)) with:
- Memory offsets verified against the stock 5.7.4 headers; they equal the 5.6 offsets (FShaderParametersMetadata has a vtable -> LayoutName at +0x08, Size at +0x3C, Layout ptr at +0x48; the editor has an extra FHashedName debug pointer so FileName is at +0x38 instead of +0x30, handled by the dual-profile probe; FRHIUniformBufferLayout: Name FString then 6 TArrays, Hash at name+0x70; FMember 0x30 bytes; TLinkedList node Next/PrevLink/Element, LayoutHash at node+0x18). Runtime validation: oatmealf recomputes every layout hash from the captured members and compares it with the captured hash.
- `src/minidump.rs`: MDMP parser (Memory64List + ModuleList + MiscInfo) with lazy positional file reads (a 4 GB dump is not loaded into memory; about 4 s per capture).
- CLI flags: `--game-minidump <DMP>`, `--save-game-layouts <FILE>`, `--save-editor-layouts <FILE>`, `--capture-only`.
- `src/bin/uniform_layouts.rs`: second binary `uniform_layouts`; writes the game layouts that differ from the editor as the modkit profile file `uniform_layouts.txt`.

## Run

Build: `cargo build --release` (exe: `target\release\oatmealf.exe`).

1. Editor capture (a STOCK 5.7.4 editor is running as `UnrealEditor.exe`):
   `oatmealf --capture-only --save-editor-layouts <editor-layouts.json>`
2. Game capture from a full-memory minidump of the game (no process needed):
   `oatmealf --capture-only --game-minidump <game>.DMP --save-game-layouts <game-layouts.json> -v`
3. Diff and generate against the read-only stock source:
   `oatmealf --game-layouts <game-layouts.json> --editor-layouts <editor-layouts.json> --engine-root <stock 5.7.4 source root> -o Generated -v`
   Output: `Generated\<timestamp>\report.md`, `manifest.json`, `exports\`, `Engine\Source\...` (copies of the stock files with the changes applied).
4. Profile data file from the two captures:
   `uniform_layouts <game-layouts.json> <editor-layouts.json> <profile dir>\uniform_layouts.txt` (exe: `target\release\uniform_layouts.exe`)

Tests: `cargo test --release`; real-dump test (Windows PowerShell): `$env:OATMEALF_TEST_MINIDUMP='<dump>'; cargo test --release --test real_minidump -- --ignored`. Form for other shells: `OATMEALF_TEST_MINIDUMP=<dump> cargo test --release --test real_minidump -- --ignored`.

## Notes

- The dump's main module (`<Game>-Win64-Shipping.exe`) is fully present in a full-memory dump; image strings and pointers are not rebased, so the anchor scan works as on a live process.
- "Source:" paths in the report come from `__FILE__` recorded in the editor capture; the generated copies are produced from the `--engine-root` tree.
- `Generated\` and `target\` are build and run output.
