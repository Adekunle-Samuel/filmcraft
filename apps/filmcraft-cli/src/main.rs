//! FilmCraft headless CLI.
//!
//! ```text
//! filmcraft-cli probe <media>                         media info as JSON
//! filmcraft-cli commands [filter]                     list engine commands
//! filmcraft-cli render [--project p.fcproj|--demo] --seconds S --out frame.png [--scale 0.5]
//! filmcraft-cli run [--project p.fcproj|--demo] <script.jsonl>   run commands (one {"id","params"} per line)
//! filmcraft-cli mcp [--bridge 127.0.0.1:9876] [--demo]            MCP server on stdio
//! ```

use filmcraft_engine::Session;
use serde_json::{Value, json};

fn usage() -> ! {
    eprintln!("usage: filmcraft-cli <probe|commands|render|run|mcp> …  (see --help in source header)");
    std::process::exit(2)
}

fn session_from(args: &[String]) -> Session {
    let mut s = Session::default();
    if let Some(i) = args.iter().position(|a| a == "--project") {
        let p = args.get(i + 1).cloned().unwrap_or_else(|| usage());
        if let Err(e) = s.execute("file.open", json!({"path": p})) {
            eprintln!("filmcraft-cli: {e}");
            std::process::exit(1);
        }
    } else if args.iter().any(|a| a == "--demo") {
        let _ = s.execute("file.openDemoProject", json!({}));
    }
    s
}

fn opt<'a>(args: &'a [String], k: &str) -> Option<&'a str> {
    args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).map(String::as_str)
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first() else { usage() };
    match cmd.as_str() {
        "probe" => {
            let path = args.get(1).unwrap_or_else(|| usage());
            let bytes = std::fs::read(path).unwrap_or_else(|e| {
                eprintln!("{path}: {e}");
                std::process::exit(1)
            });
            match filmcraft_codecs::open_bytes(path, bytes.into()) {
                Ok(src) => println!("{}", serde_json::to_string_pretty(src.info()).unwrap_or_default()),
                Err(e) => {
                    eprintln!("{path}: {e}");
                    std::process::exit(1)
                }
            }
        }
        "commands" => {
            let f = args.get(1).map(|s| s.to_ascii_lowercase()).unwrap_or_default();
            for c in filmcraft_engine::command_specs() {
                if f.is_empty() || c.id.to_ascii_lowercase().contains(&f) {
                    println!("{:<32} {:<34} {:<14} {}", c.id, c.label, c.shortcut.unwrap_or(""), c.params);
                }
            }
        }
        "render" => {
            let mut s = session_from(&args);
            let secs: f64 = opt(&args, "--seconds").and_then(|v| v.parse().ok()).unwrap_or(0.0);
            let scale: f32 = opt(&args, "--scale").and_then(|v| v.parse().ok()).unwrap_or(1.0);
            let out = opt(&args, "--out").unwrap_or("frame.png");
            s.set_playhead(filmcraft_time::Tick::from_seconds_f64(secs));
            let t0 = std::time::Instant::now();
            let Some(img) = s.render_program(scale) else {
                eprintln!("no sequence");
                std::process::exit(1)
            };
            let dt = t0.elapsed();
            let png = filmcraft_automation::png_rgba(img.w as u32, img.h as u32, img.over_black_rgba8(), 0).expect("png");
            std::fs::write(out, png).expect("write");
            eprintln!("rendered {}x{} in {:.1} ms → {out}", img.w, img.h, dt.as_secs_f64() * 1000.0);
        }
        "run" => {
            let mut s = session_from(&args);
            let script = args.last().unwrap_or_else(|| usage());
            let text = std::fs::read_to_string(script).unwrap_or_else(|e| {
                eprintln!("{script}: {e}");
                std::process::exit(1)
            });
            for line in text.lines().filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#')) {
                let v: Value = serde_json::from_str(line).unwrap_or_else(|e| {
                    eprintln!("bad line `{line}`: {e}");
                    std::process::exit(1)
                });
                let id = v["id"].as_str().unwrap_or("");
                match s.execute(id, v.get("params").cloned().unwrap_or(json!({}))) {
                    Ok(r) => println!("{id}: {r}"),
                    Err(e) => {
                        eprintln!("{id}: {e}");
                        std::process::exit(1)
                    }
                }
            }
        }
        "mcp" => {
            let server = match opt(&args, "--bridge") {
                Some(addr) => filmcraft_automation::FilmcraftMcp::bridge(addr).unwrap_or_else(|e| {
                    eprintln!("{e}");
                    std::process::exit(1)
                }),
                None => filmcraft_automation::FilmcraftMcp::headless(session_from(&args)),
            };
            if let Err(e) = server.serve_stdio().await {
                eprintln!("filmcraft-cli mcp: {e}");
                std::process::exit(1)
            }
        }
        _ => usage(),
    }
    let _ = filmcraft_media::MediaKind::Movie;
}
