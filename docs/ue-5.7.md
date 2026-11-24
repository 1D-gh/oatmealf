# UE 5.7.4, minidump capture and uniform_layouts

oatmealf supports Unreal Engine 5.7.4 alongside 5.6, and has three features for working from saved data:

- Game capture from a full-memory Windows minidump (`--game-minidump`), so the game does not have to keep running while the tool reads it.
- Saved captures (`--save-game-layouts`, `--save-editor-layouts`) and `--capture-only`, which stops after saving them. A saved capture is read back with `--game-layouts` / `--editor-layouts`.
- A second program, `uniform_layouts`, which writes the game layouts that differ from the editor's as a text file (format below), for tools that load the game's layouts into an engine at run time.

The usage in [README.md](../README.md) applies to 5.7.4 unchanged.

## UE 5.7.4 memory layout

The offsets oatmealf reads in 5.7.4 equal the 5.6 offsets:

- `FShaderParametersMetadata` has a vtable: `LayoutName` at +0x08, `Size` at +0x3C, `Layout` pointer at +0x48. Editor builds carry an extra `FHashedName` debug pointer, so `FileName` is at +0x38 there and at +0x30 in shipping builds; the capture probes both.
- `FRHIUniformBufferLayout`: `Name` (`FString`), then six `TArray`s; `Hash` at name+0x70; `FMember` is 0x30 bytes.
- Registry list: `TLinkedList` nodes (`Next`, `PrevLink`, `Element`), `LayoutHash` at node+0x18.

oatmealf recomputes every layout hash from the captured members and compares it with the captured hash, so a wrong offset shows up as a verification failure.

## Workflow

"Stock" below means an unmodified Unreal Engine 5.7.4: the editor binary of the Epic Games launcher or a build of Epic's 5.7.4 source.

Build: `cargo build --release` (programs: `target\release\oatmealf.exe` and `target\release\uniform_layouts.exe`).

1. Editor capture, with a stock 5.7.4 editor running as `UnrealEditor.exe`:
   `oatmealf --capture-only --save-editor-layouts <editor-layouts.json>`
2. Game capture from a full-memory minidump of the game process:
   `oatmealf --capture-only --game-minidump <game>.DMP --save-game-layouts <game-layouts.json> -v`
3. Report and source copies against the stock 5.7.4 source tree:
   `oatmealf --game-layouts <game-layouts.json> --editor-layouts <editor-layouts.json> --engine-root <stock 5.7.4 source root> -o Generated -v`
   Output: `Generated\<timestamp>\report.md`, `manifest.json`, `exports\`, and `Engine\Source\...` (copies of the stock files with the changes applied).
4. Data file of the differing layouts:
   `uniform_layouts <game-layouts.json> <editor-layouts.json> <out.txt>`

Tests: `cargo test --release`. The test against a real dump is ignored by default and takes the dump path from `OATMEALF_TEST_MINIDUMP`:

```console
# PowerShell
$env:OATMEALF_TEST_MINIDUMP='<dump>'; cargo test --release --test real_minidump -- --ignored
# other shells
OATMEALF_TEST_MINIDUMP=<dump> cargo test --release --test real_minidump -- --ignored
```

## uniform_layouts output format

One record per game layout that differs from the editor layout of the same name:

```text
layout <Name> editor=<hash> game=<hash> size=<bytes> flags=<n> binding=<n> static=<n> rtoffset=<n>
leaf <GameOffset> <Type> <Size> <Default|-> <Path>
list <R|G|T|B|U|N> <Path> [<Path> ...]
end
```

- `layout`: `editor` and `game` are the RHI layout hashes (`FRHIUniformBufferLayout::GetHash`) of the editor and the game layout; `size` is the game `ConstantBufferSize`; `flags`, `binding` (`EUniformBufferBindingFlags`), `static` (`FUniformBufferStaticSlot`, 255 when none) and `rtoffset` (`RenderTargetsOffset`, 65535 when none) are the game values.
- `leaf`: one line per constant and resource member of the game struct. `GameOffset` is the offset in the game constant data. `Type` is `FLOAT32`, `INT32`, `UINT32` (constants) or `SAMPLER`, `SRV`, `TEXTURE`, `UAV`, `RDG_TEXTURE`, `RDG_TEXTURE_SRV`, `RDG_TEXTURE_UAV`, `RDG_BUFFER_SRV`, `RDG_BUFFER_UAV` (resources, 8 bytes each). `Size` is the byte size (constant arrays: 16 byte stride times count). `Default` names the resource to bind when the editor struct has no such member (`black_texture`, `white_texture`, `black_cube`, `black_srv`, `black_buffer_srv`, `point_sampler`), `-` for constants.
- `list`: the members of one RHI layout array in the game's order: `R` Resources, `G` GraphResources, `T` GraphTextures, `B` GraphBuffers, `U` GraphUniformBuffers, `N` UniformBuffers.
- `Path`: the flattened member name of the editor metadata: `Name`, `Parent.Name` for a nested or included struct member, `Name[i]` for a resource array element, `Name[i].Child` for a member of a struct array element.

After the layout records, the file has a block of `add` records (preceded by the comment `# add records: members of game structs that the editor structs lack`), one per member that a game struct has and the editor struct of the same type lacks:

```text
add <StructTypeName> <AfterMember|-> <Name> <BaseType> <Rows> <Columns> <Elements> <HlslType>
```

- `StructTypeName` is the type name of the struct that directly owns the member (the top-level uniform buffer struct or a nested struct type). A struct type that appears in several layouts has its records once.
- `AfterMember` is the game member the new member follows in that struct's declaration order (it may itself be an added member); `-` for the first member. The records of one struct are in game declaration order.
- `BaseType` is `UBMT_` followed by the `Type` of a leaf. `Rows`, `Columns` and `Elements` (0 = not an array) are the game's values.
- `HlslType` is the shader type of a resource with whitespace removed (`Texture2D<uint2>`, `StructuredBuffer<float4>`, `SamplerState`), or the HLSL type of a constant from its rows and columns (`float`, `float3`, `float4x4`, `uint`, `int2`).

The generator stops with an error when a struct type has different added members in different places, when an added member is a struct, or when a member has a non-default precision.

## Notes

- A full-memory dump contains the whole main module (`<Game>-Win64-Shipping.exe`); its strings and pointers are not rebased, so the anchor scan works as on a live process.
- "Source:" paths in the report come from `__FILE__` in the editor capture; the generated copies come from the `--engine-root` tree.
- `Generated\` and `target\` are run and build output.
