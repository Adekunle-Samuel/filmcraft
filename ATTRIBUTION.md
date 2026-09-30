# Attribution

Every non-code asset in FilmCraft, with its author, source and licence. Each file listed here also has
a `<file>.attribution` sidecar with full details. The rules are in [AGENTS.md](AGENTS.md) §1: no Adobe
iconography, images or other Adobe assets; open licences only; every asset attributed.
`cargo xtask assets` checks that this index and the sidecars cover every asset in the repository.

## Asset files

| Asset | Author | Source | Licence |
|---|---|---|---|
| `assets/fonts/Inter-Regular.ttf` | The Inter Project Authors | https://github.com/rsms/inter | OFL-1.1 (`assets/fonts/OFL-Inter.txt`) |
| `assets/fonts/Inter-Medium.ttf` | The Inter Project Authors | https://github.com/rsms/inter | OFL-1.1 (`assets/fonts/OFL-Inter.txt`) |
| `assets/fonts/Inter-SemiBold.ttf` | The Inter Project Authors | https://github.com/rsms/inter | OFL-1.1 (`assets/fonts/OFL-Inter.txt`) |
| `assets/fonts/JetBrainsMono-Regular.ttf` | The JetBrains Mono Project Authors | https://github.com/JetBrains/JetBrainsMono | OFL-1.1 (`assets/fonts/OFL-JetBrainsMono.txt`) |
| `docs/images/filmcraft-hero.png` | FilmCraft contributors | Original work: screenshot of FilmCraft showing procedurally generated demo footage | MIT OR Apache-2.0 |

## Assets drawn or generated in code

These are original work by FilmCraft contributors under the project licence (MIT OR Apache-2.0). None
is traced or derived from Adobe artwork.

| Asset | Where |
|---|---|
| UI icons (tools, transport, panels, header) | `crates/ui-egui/src/icons.rs`: vector paths on a 16×16 grid |
| Demo footage (ocean sunset, aurora, city night, dunes, forest, plasma), bars and tone, counting leader, colour matte | `crates/media/src/generators.rs` |
| Lumetri "Look" presets (Teal & Orange, Warm Film, …) | `crates/render/src/effects.rs` (`apply_look`): procedural colour transforms, not LUT files |
| Colour palette and layout metrics | `crates/ui-egui/src/theme.rs`: colour values and sizes only; no artwork |
