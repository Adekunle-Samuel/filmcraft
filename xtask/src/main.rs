//! Workspace automation: `cargo xtask <layers|wasm|ci>`.
//!
//! - `layers`: enforces the dependency layering of `plan/architecture.md` §3 (downward-only edges,
//!   listed same-layer edges, L0 codec crates depend on `bitstream` only, no UI/OS crates below L5).
//! - `wasm`: `cargo check --target wasm32-unknown-unknown` for every crate in L0–L4.
//! - `ci`: fmt check, clippy -D warnings, tests, layers, wasm.

use std::process::{Command, ExitCode};

use serde_json::Value;

/// (crate name without the `filmcraft-` prefix, layer).
const LAYERS: &[(&str, u8)] = &[
    ("time", 0),
    ("geom", 0),
    ("color", 0),
    ("bitstream", 0),
    ("isobmff", 0),
    ("matroska", 0),
    ("riff", 0),
    ("h264", 0),
    ("h264enc", 0),
    ("hevc", 0),
    ("prores", 0),
    ("mjpeg", 0),
    ("dnx", 0),
    ("aac", 0),
    ("opus", 0),
    ("frame", 1),
    ("media", 1),
    ("project", 1),
    ("audio-dsp", 1),
    ("edit", 2),
    ("codecs", 2),
    ("keyframe", 2),
    ("effects", 2),
    ("text", 2),
    ("audio", 2),
    ("captions", 2),
    ("interchange", 2),
    ("render", 3),
    ("gpu", 3),
    ("scopes", 3),
    ("playback", 3),
    ("export", 3),
    ("format", 3),
    ("engine", 4),
    ("platform", 5),
    ("ui-egui", 5),
    ("automation", 5),
    ("filmcraft", 6),
    ("cli", 6),
];

/// L0 crates that are not codecs/containers (no `bitstream`-only restriction).
const L0_FOUNDATION: &[&str] = &["time", "geom", "color", "bitstream"];

/// Allowed same-layer edges (from, to).
const SAME_LAYER: &[(&str, &str)] = &[
    ("media", "frame"),
    ("project", "media"),
    ("project", "frame"),
    ("effects", "keyframe"),
    ("audio", "keyframe"),
    ("playback", "render"),
    ("playback", "gpu"),
    ("export", "render"),
    ("scopes", "gpu"),
    ("gpu", "render"),
    ("cli", "filmcraft"),
];

/// Crates that must not appear below L5 (UI toolkits, windowing, OS audio/menus).
const UI_ONLY: &[&str] = &["egui", "eframe", "egui-wgpu", "winit", "rfd", "cpal", "muda"];

fn short(name: &str) -> &str {
    name.strip_prefix("filmcraft-").unwrap_or(name)
}

fn layer_of(name: &str) -> Option<u8> {
    LAYERS.iter().find(|(n, _)| *n == short(name)).map(|(_, l)| *l)
}

fn metadata() -> Result<Value, String> {
    let out = Command::new(env!("CARGO")).args(["metadata", "--format-version", "1", "--no-deps"]).output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into());
    }
    serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())
}

fn workspace_crates(md: &Value) -> Vec<(String, Vec<String>)> {
    md["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["name"] != "xtask")
        .map(|p| {
            let deps = p["dependencies"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|d| d["kind"].is_null() || d["kind"] == "build")
                .filter_map(|d| d["name"].as_str().map(str::to_string))
                .collect();
            (p["name"].as_str().unwrap_or_default().to_string(), deps)
        })
        .collect()
}

fn layers() -> Result<(), String> {
    let md = metadata()?;
    let mut errors = Vec::new();
    for (name, deps) in workspace_crates(&md) {
        let Some(l) = layer_of(&name) else {
            errors.push(format!("{name}: not assigned a layer (add it to xtask LAYERS and plan/architecture.md §3)"));
            continue;
        };
        for d in &deps {
            if l < 5 && UI_ONLY.contains(&d.as_str()) {
                errors.push(format!("{name} (L{l}) depends on UI/OS crate `{d}`"));
            }
            if !d.starts_with("filmcraft-") {
                continue;
            }
            let Some(dl) = layer_of(d) else { continue };
            let (a, b) = (short(&name), short(d));
            if l == 0 && !L0_FOUNDATION.contains(&a) && b != "bitstream" {
                errors.push(format!("{a} (L0 codec/container) may depend only on bitstream, found {b}"));
            } else if dl > l {
                errors.push(format!("{a} (L{l}) depends upward on {b} (L{dl})"));
            } else if dl == l && l > 0 && !SAME_LAYER.contains(&(a, b)) {
                errors.push(format!("{a} → {b}: same-layer edge (L{l}) not in the allowed list"));
            }
        }
    }
    if errors.is_empty() {
        println!("layers: ok");
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}

fn run(cmd: &mut Command) -> Result<(), String> {
    eprintln!("$ {cmd:?}");
    let st = cmd.status().map_err(|e| e.to_string())?;
    if st.success() { Ok(()) } else { Err(format!("failed: {cmd:?}")) }
}

fn wasm() -> Result<(), String> {
    let md = metadata()?;
    let mut cmd = Command::new(env!("CARGO"));
    cmd.args(["check", "--target", "wasm32-unknown-unknown"]);
    let mut n = 0;
    for (name, _) in workspace_crates(&md) {
        if layer_of(&name).is_some_and(|l| l <= 4) {
            cmd.args(["-p", &name]);
            n += 1;
        }
    }
    if n == 0 {
        return Ok(());
    }
    run(&mut cmd)?;
    println!("wasm: ok ({n} crates)");
    Ok(())
}

fn ci() -> Result<(), String> {
    let cargo = env!("CARGO");
    run(Command::new(cargo).args(["fmt", "--check"]))?;
    run(Command::new(cargo).args(["clippy", "--workspace", "--all-targets", "--release", "--", "-D", "warnings"]))?;
    run(Command::new(cargo).args(["test", "--workspace", "--release"]))?;
    layers()?;
    wasm()
}

fn main() -> ExitCode {
    let task = std::env::args().nth(1).unwrap_or_default();
    let r = match task.as_str() {
        "layers" => layers(),
        "wasm" => wasm(),
        "ci" => ci(),
        _ => Err("usage: cargo xtask <layers|wasm|ci>".into()),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
