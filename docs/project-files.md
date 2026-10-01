# Project files

FilmCraft projects are `.fcproj` files: UTF-8 JSON with an explicit schema version. Reading and
writing them is `crates/format` (`filmcraft-format`); the engine's `file.*` commands use it.

## Format

```json
{
  "format": "filmcraft.project",
  "schema_version": 2,
  "generator": "FilmCraft 0.1.0",
  "project": { "name": "…", "settings": { … }, "root": { … }, "items": { … }, "next_id": 48 }
}
```

- `schema_version` is the version of the whole document. This build writes
  `filmcraft_format::SCHEMA_VERSION` and reads every version from 1 up to it.
- `project` is the serialized `filmcraft_project::Project` (bins, media clips, sequences, tracks,
  clips, effects, keyframes, markers). Media is referenced by path, never embedded.
- Files are written compact (no indentation). A 2,000-clip project is about 1.4 MB.

### Schema history

| Version | Since | Shape |
|---|---|---|
| 1 | M0 | the bare `Project` object with a `"version": 1` field |
| 2 | M11.1 | the envelope above; the project's own `version` field is gone |

### Migrations

Older files are upgraded on load, in memory, by a chain of single-step functions
(`filmcraft_format::MIGRATIONS`, where entry *i* upgrades v*i+1* to v*i+2*). Each step rewrites the JSON
document; after the last one the result is deserialized into the current model. To change the schema:

1. add a function `vN_to_vN1(doc: Value) -> Result<Value, String>` and append it to `MIGRATIONS`
   (`SCHEMA_VERSION` follows automatically);
2. add a small hand-made fixture of the old shape to `crates/format/tests/fixtures/` and a test that it
   loads;
3. never edit a migration that has shipped.

Fields added with `#[serde(default)]` don't need a migration: missing values take their defaults.

When you open an older file FilmCraft says so ("Upgraded project from schema v1 to v2"). The file
on disk is unchanged until you save. The **first save over it** keeps the original next to it as
`<name> (schema v1 backup).fcproj`, so an older FilmCraft can still open your work.

### Newer files

A file with a `schema_version` newer than the build supports is **refused**, not half-read:

> this project was saved by a newer version of FilmCraft (project schema v3); this build reads up to
> v2. Update FilmCraft to open it.

Reading it partially and saving it back would silently drop whatever the newer version added.

## Saving is atomic

Every write of a project file, auto-save, recovery snapshot or preferences file goes through
`filmcraft_format::atomic_write`:

1. write the bytes to a hidden temp file in the **same directory** (`.<name>.<pid>-<n>.tmp`);
2. `fsync` the temp file;
3. rename it over the target (an atomic replace on every supported OS);
4. `fsync` the directory so the rename itself is durable.

A crash, `kill -9` or power cut at any point leaves either the complete old file or the complete new
one. If a step fails, the temp file is removed and the old file is untouched.

## Commands

| Id | Menu | |
|---|---|---|
| `file.save {path?}` | File ▸ Save (⌘S) | write to the project's path (or `path`) |
| `file.saveAs {path}` | File ▸ Save As… (⇧⌘S) | write and adopt the new path |
| `file.saveCopy {path}` | File ▸ Save a Copy… (⌥⌘S) | write a copy; the project keeps its path and unsaved state |
| `file.revert` | File ▸ Revert | reload the last saved version, discarding changes |
| `file.open {path}` | File ▸ Open Project… (⌘O) | returns `{schemaVersion, migrated}` |

In the UI, Save / Save As / Save a Copy without a `path` show a file dialog.
