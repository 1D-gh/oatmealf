# oatmealf

Compare shader parameter layouts from a running UE 5.6 game and editor, then generate source copies for layouts that differ.

The tool reads both processes without modifying them and checks the full uniform buffer registry. Generated files are written to a separate output directory. The original engine source is never changed.

## Usage

Start the game and its matching editor, then run:

```console
oatmealf
```

The game is autodetected when one `*-Win64-Shipping.exe` process is running. The editor defaults to `UnrealEditor.exe`, and its executable path is used to locate the engine source. Saved captures need `--engine-root` because they do not identify an engine source tree.

Use `--game` to select a specific game process instead of autodetecting:

```console
oatmealf --game Subnautica2-Win64-Shipping
```

Output looks like:

```text
Shader layout changes modeled successfully.

153 layouts compared
1 changed layout found
1 source file generated
1/1 changed layout verified

Output: C:\path\to\Generated\20260717-203000-a1b2c3
```

## Options

| Option | Description |
| --- | --- |
| `--game <PROCESS>` | Select a specific game process instead of autodetecting. |
| `--editor <PROCESS>` | Select a different editor process. Defaults to `UnrealEditor`. |
| `--engine-root <PATH>` | Use a specific Unreal Engine root, `Engine` directory or `Engine\Source` directory instead of locating it from the editor. |
| `--game-layouts <FILE>` | Use a saved `game-layouts.json` capture instead of a running game. |
| `--editor-layouts <FILE>` | Use a saved `editor-layouts.json` capture instead of a running editor. |
| `-o, --output <PATH>` | Select a specific output directory. The default is `Generated`. |
| `--layout <NAME>` | Only check selected layouts. Repeat the option or separate names with commas. |
| `-v, --verbose` | Show capture and verification details. |
| `-h, --help` | Print the command-line help. |
| `-V, --version` | Print the tool version. |

## Output

Each run creates a new directory:

```text
Generated\20260717-203000-a1b2c3\
  report.md
  manifest.json
  exports\
    game-layouts.json
    editor-layouts.json
  Engine\Source\...
```

`report.md` lists the changed layouts, generated files and anything that still needs review.

`manifest.json` contains the result in a machine readable format. The exported JSON files contain the game and editor layouts used for that run.

Files under `Engine\Source` are complete copies of the supplied engine source files with the generated changes applied. They can be reviewed and copied into the engine tree manually.

When a generated resource member needs a placeholder value, oatmealf also updates a matching renderer fill function when it can identify the function and value safely. If it cannot, the member is left under `REVIEW NEEDED` instead of being guessed.

There are three possible results:

- `READY`: every changed layout was modeled and verified.
- `MISSING LAYOUTS`: some game layouts were not loaded in the editor. This is usually due to missing plugins.
- `REVIEW NEEDED`: part of the change could not be generated safely, or a new resource needs a runtime value.

A `MISSING LAYOUTS` or `REVIEW NEEDED` run can still contain source files for layouts that were modeled successfully.

When a missing layout comes from an engine plugin, the report names that plugin when it can be identified from the supplied source tree.

## Saved Captures

The exported layout files are readable JSON and can be reused without reopening the game or editor:

```console
oatmealf --game-layouts game-layouts.json --editor-layouts editor-layouts.json --engine-root C:\path\to\UnrealEngine
```

Use `--layout View,OpaqueBasePass` to limit the comparison while debugging. Normal runs check the full registry.

## Verification

Before generating source files, the tool validates both captures, recomputes their Unreal layout hashes and verifies that the generated declarations reproduce the captured layouts.

If a change cannot be verified, it is reported as `REVIEW NEEDED` instead of being guessed.

Placeholder constant names do not affect shader binding. The tool can recover the required layout, but it cannot recover the original field names, runtime values or custom renderer behavior from metadata alone.

## Compatibility

oatmealf is built for UE 5.6 and tested with Subnautica 2. Other UE 5.6 games may work but are untested. Other engine versions are not supported.

## Build

Install Rust 1.85 or newer and run:

```console
cargo build --release
```

The executable is written to `target/release`.

Run the tests with:

```console
cargo test
```

## Exit Codes

```text
0  run completed, including MISSING LAYOUTS and REVIEW NEEDED results
2  capture, input or tool error
130 interrupted with Ctrl+C
```

## Manual Workflow

This tool was made to automate the process documented in [Manually Recovering Subnautica 2 Shader Layouts](docs/subnautica2-manual-workflow.md).

## License

Licensed under the MIT License.
