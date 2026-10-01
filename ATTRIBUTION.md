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
| `docs/images/filmcraft-hero.png` | FilmCraft contributors | Original work: FilmCraft screenshot; frames from public-domain films (Night of the Living Dead 1968, Carnival of Souls 1962), CC0 Chopin (Musopen) | MIT OR Apache-2.0; film frames public domain |
| `docs/images/filmcraft-color.png` | FilmCraft contributors | Original work: FilmCraft screenshot; frames from Charade (1963, public domain) | MIT OR Apache-2.0; film frames public domain |
| `docs/images/filmcraft-keyframes.png` | FilmCraft contributors | Original work: FilmCraft screenshot; frames from Night of the Living Dead (1968, public domain) | MIT OR Apache-2.0; film frames public domain |
| `docs/images/filmcraft-export.png` | FilmCraft contributors | Original work: FilmCraft screenshot; frame from Night of the Living Dead (1968, public domain) | MIT OR Apache-2.0; film frames public domain |

## Third-party media shown in screenshots

The README screenshots show frames from these works. The media files themselves are not in the
repository; each screenshot's sidecar lists exactly which appear.

| Work | Author | Status | Source |
|---|---|---|---|
| Night of the Living Dead (1968) | George A. Romero / Image Ten | US public domain (no copyright notice) | https://archive.org/details/night-of-the-living-dead-1968-english |
| Carnival of Souls (1962) | Herk Harvey / Harcourt Productions | US public domain | https://archive.org/details/CarnivalOfSoulsVideoQualityUpgrade |
| Charade (1963) | Stanley Donen / Universal | US public domain (defective notice) | https://archive.org/details/charade-1963-cary-grant-audrey-hepburn-comedy-mystery-romance-thriller-full-movie |
| Earth Views from the ISS (NHQ_2020_1221) | NASA | US Government work, public domain (no NASA endorsement implied) | https://images.nasa.gov/details/NHQ_2020_1221_Earth%20Views |
| Chopin: Nocturne Op. 48 No. 1, Ballade No. 1 | Musopen | CC0-1.0 | https://archive.org/details/musopen-chopin |

## Assets drawn or generated in code

These are original work by FilmCraft contributors under the project licence (MIT OR Apache-2.0). None
is traced or derived from Adobe artwork.

| Asset | Where |
|---|---|
| UI icons (tools, transport, panels, header) | `crates/ui-egui/src/icons.rs`: vector paths on a 16×16 grid |
| Demo footage (ocean sunset, aurora, city night, dunes, forest, plasma), bars and tone, counting leader, colour matte | `crates/media/src/generators.rs` |
| Lumetri "Look" presets (Teal & Orange, Warm Film, …) | `crates/render/src/effects.rs` (`apply_look`): procedural colour transforms, not LUT files |
| Colour palette and layout metrics | `crates/ui-egui/src/theme.rs`: colour values and sizes only; no artwork |
