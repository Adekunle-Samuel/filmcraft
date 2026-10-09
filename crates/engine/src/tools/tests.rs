//! The tool catalogue: schemas in the strict subset, golden inputs, hostile inputs, the policy
//! table, and the available tools run on the demo project.

use serde_json::{Value, json};

use super::*;
use crate::Session;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

/// One valid input per tool (every tool, available or not, has one).
fn golden(name: &str) -> Value {
    match name {
        "project_overview" => json!({}),
        "import_media" => json!({"paths": ["/tmp/a.mov"], "bin": null}),
        "read_transcript" => json!({"offset": 0, "limit": 20}),
        "contact_sheet" => json!({"item": null, "sequence": true, "count": 6, "times": null, "cols": 3, "max_side": 512}),
        "render_frame" => json!({"item": null, "sequence": null, "seconds": 1.5, "max_side": 256}),
        "add_captions" => json!({"max_chars": 32, "lines": 2, "name": null}),
        "set_loudness" => json!({"clips": null, "target_lufs": -16.0}),
        "export" => json!({"path": "/tmp/out.mp4", "preset": null, "format": "h264", "range": "entire", "sequence": null, "burn_captions": true}),
        "command_search" => json!({"query": "razor"}),
        "command_describe" => json!({"id": "timeline.razor"}),
        "command_run" => json!({"id": "sequence.inspect", "params": null}),
        "command_batch" => json!({"steps": [{"id": "playhead.set", "params": "{\"seconds\":1}"}], "stop_on_error": true}),
        "transcribe" => json!({"items": [3], "language": "en", "model": null, "keep_fillers": null, "regions": [[0.0, 2.5], [3.1, 9.0]]}),
        "find_silences" => json!({"min_seconds": 0.5, "pad_seconds": null, "threshold_db": null, "start_seconds": null, "end_seconds": null}),
        "measure_loudness" => json!({"start_seconds": null, "end_seconds": 2.5}),
        "analyze_media" => json!({"item": 3, "max_frames": null}),
        "propose_edit_plan" => json!({"plan": "{\"version\":1}"}),
        "apply_edit_plan" => json!({"plan": "{\"version\":1}", "source_hash": "abc"}),
        "create_variations" => json!({"plans": ["{\"version\":1}", "{\"version\":1}"], "source_hash": "abc"}),
        "match_grade" => json!({"item": 3, "clips": [10, 11], "samples": 6}),
        "bake_lut" => json!({"clip": 10, "name": "warm look", "size": 33}),
        "export_variations" => json!({"sequences": [5, 9], "preset": "Social Vertical 1080×1920", "folder": "/tmp/variations", "overwrite": null}),
        other => panic!("no golden input for {other}"),
    }
}

#[test]
fn every_schema_is_in_the_strict_subset() {
    let mut names = std::collections::HashSet::new();
    for t in catalogue() {
        assert!(names.insert(t.name), "{} listed twice", t.name);
        assert!(t.name.chars().all(|c| c.is_ascii_lowercase() || c == '_'), "{} is not snake_case", t.name);
        assert!(!t.title.is_empty() && !t.description.is_empty(), "{}", t.name);
        let v: Value = serde_json::from_str(t.schema).unwrap_or_else(|e| panic!("{}: schema does not parse: {e}", t.name));
        schema::check_strict(&v).unwrap_or_else(|e| panic!("{}: {e}", t.name));
        // what strict tool use is sent has none of the engine-only keywords
        let api = schema::for_strict_api(&v).to_string();
        for k in ["minimum", "maximum", "maxItems", "minItems", "maxLength"] {
            assert!(!api.contains(&format!("\"{k}\"")), "{}: {k} left in {api}", t.name);
        }
        assert!(!t.requires.is_empty(), "{}", t.name);
        if t.read_only {
            assert!(!t.destructive && t.approval == Approval::Never, "{}: read-only tools run without asking", t.name);
        }
    }
}

#[test]
fn the_checker_rejects_schemas_outside_the_subset() {
    for bad in [
        json!({"type": "array", "items": {"type": "string"}}),
        json!({"type": "object", "properties": {"a": {"type": "string"}}, "required": [], "additionalProperties": false}),
        json!({"type": "object", "properties": {"a": {"type": "string"}}, "required": ["a"], "additionalProperties": true}),
        json!({"type": "object", "properties": {"a": {"$ref": "#/x"}}, "required": ["a"], "additionalProperties": false}),
        json!({"type": "object", "properties": {"a": {"type": "string", "pattern": "x"}}, "required": ["a"], "additionalProperties": false}),
        json!({"type": "object", "properties": {"a": {"type": "array"}}, "required": ["a"], "additionalProperties": false}),
        json!({"type": "object", "properties": {"a": {"type": ["string", "string"]}}, "required": ["a"], "additionalProperties": false}),
        json!({"type": "object", "properties": {"a": {"type": "string", "minimum": 1}}, "required": ["a"], "additionalProperties": false}),
        json!({"type": "object", "properties": {"a": {"type": "integer", "enum": ["x"]}}, "required": ["a"], "additionalProperties": false}),
        json!({"type": "object", "properties": {"a": {"type": "object"}}, "required": ["a"], "additionalProperties": false}),
    ] {
        assert!(schema::check_strict(&bad).is_err(), "{bad} should be refused");
    }
}

#[test]
fn golden_inputs_validate() {
    for t in catalogue() {
        let g = golden(t.name);
        schema::validate(&t.schema_value(), &g).unwrap_or_else(|e| panic!("{}: {e}", t.name));
        validate(t.name, &g).unwrap();
    }
}

#[test]
fn hostile_inputs_are_rejected() {
    let huge: Vec<f64> = vec![1.0; 100_000];
    let cases: Vec<(&str, Value)> = vec![
        ("project_overview", json!({"verbose": true})),
        ("project_overview", json!([])),
        ("project_overview", json!(null)),
        ("import_media", json!({"paths": [], "bin": null})),
        ("import_media", json!({"paths": "/tmp/a.mov", "bin": null})),
        ("import_media", json!({"paths": vec!["/x"; 65], "bin": null})),
        ("import_media", json!({"paths": ["/x"], "bin": -1})),
        ("import_media", json!({"paths": ["/x"]})),
        ("read_transcript", json!({"offset": 1.5, "limit": null})),
        ("read_transcript", json!({"offset": null, "limit": 1e9})),
        ("read_transcript", json!({"offset": "0", "limit": null})),
        ("contact_sheet", json!({"item": null, "sequence": null, "count": 1e9, "times": null, "cols": null, "max_side": null})),
        ("contact_sheet", json!({"item": null, "sequence": null, "count": null, "times": huge, "cols": null, "max_side": null})),
        ("contact_sheet", json!({"item": null, "sequence": null, "count": null, "times": ["NaN"], "cols": null, "max_side": null})),
        ("contact_sheet", json!({"item": null, "sequence": null, "count": null, "times": [-1], "cols": null, "max_side": null})),
        ("contact_sheet", json!({"item": null, "sequence": null, "count": null, "times": null, "cols": null, "max_side": 1e12})),
        ("contact_sheet", json!({"item": null, "sequence": "yes", "count": null, "times": null, "cols": null, "max_side": null})),
        ("render_frame", json!({"item": null, "sequence": null, "seconds": null, "max_side": null})),
        ("render_frame", json!({"item": null, "sequence": null, "seconds": "NaN", "max_side": null})),
        ("render_frame", json!({"item": 1.5, "sequence": null, "seconds": 1, "max_side": null})),
        ("add_captions", json!({"max_chars": null, "lines": 3, "name": null})),
        ("export", json!({"path": "/tmp/x.mp4", "preset": null, "format": "exe", "range": null})),
        ("export", json!({"path": "/tmp/x.mp4", "preset": null, "format": null, "range": null, "wait": true})),
        ("command_run", json!({"id": "x", "params": {"seconds": 1}})),
        ("command_run", json!({"id": 7, "params": null})),
        ("command_batch", json!({"steps": [{"id": "a", "params": null, "extra": 1}], "stop_on_error": null})),
        ("command_batch", json!({"steps": vec![json!({"id": "a", "params": null}); 51], "stop_on_error": null})),
        ("command_batch", json!({"steps": [], "stop_on_error": null})),
    ];
    for (name, input) in cases {
        assert!(validate(name, &input).is_err(), "{name} {input} should be rejected");
        let mut s = Session::default();
        assert!(call(&mut s, name, &input).is_err(), "{name} {input} should not run");
    }
    // an unknown field is named, with the accepted ones
    let e = validate("command_search", &json!({"query": "x", "limit": 3})).unwrap_err().to_string();
    assert!(e.contains("\"limit\"") && e.contains("query"), "{e}");
    // an unknown tool names the valid ones
    let e = call(&mut Session::default(), "rm_rf", &json!({})).unwrap_err().to_string();
    assert!(e.contains("rm_rf") && e.contains("project_overview") && e.contains("contact_sheet"), "{e}");
}

#[test]
fn the_policy_table() {
    for id in [
        "file.quit",
        "app.quit",
        "prefs.set",
        "prefs.reset",
        "shortcuts.reset",
        "media.makeOffline",
        "project.removeUnused",
        "file.close",
        "file.closeProject",
        "transcript.downloadModel",
        "tools.call",
        "no.such.command",
    ] {
        assert!(matches!(policy(id), Policy::Deny(_)), "{id} must be denied");
    }
    for id in
        ["sequence.inspect", "project.inspect", "command.list", "jobs.list", "transcript.inspect", "media.renderFrame", "media.contactSheet", "tools.list"]
    {
        assert_eq!(policy(id), Policy::Allow, "{id} is a query");
    }
    for id in ["timeline.razor", "file.import", "transcript.createCaptions", "file.exportMedia"] {
        assert_eq!(policy(id), Policy::Ask, "{id} edits");
    }
    // every registered query is allowed unless denylisted; every edit asks
    for c in crate::command_specs() {
        match policy(c.id) {
            Policy::Allow => assert!(!c.journal, "{}", c.id),
            Policy::Ask => assert!(c.journal, "{}", c.id),
            Policy::Deny(_) => {}
        }
    }
    assert_eq!(approval_for("command_run", &json!({"id": "sequence.inspect", "params": null})), Approval::Never);
    assert_eq!(approval_for("command_run", &json!({"id": "timeline.razor", "params": null})), Approval::Ask);
    let batch = json!({"steps": [{"id": "sequence.inspect", "params": null}, {"id": "timeline.razor", "params": null}], "stop_on_error": null});
    assert_eq!(approval_for("command_batch", &batch), Approval::Ask);
    assert_eq!(approval_for("export", &golden("export")), Approval::AskIfOverwrite);
    assert_eq!(approval_for("project_overview", &json!({})), Approval::Never);
}

#[test]
fn availability_follows_the_backing_commands() {
    let s = Session::default();
    let avail: Vec<&str> = catalogue_available(&s).iter().map(|t| t.name).collect();
    for want in [
        "project_overview",
        "import_media",
        "read_transcript",
        "contact_sheet",
        "render_frame",
        "add_captions",
        "set_loudness",
        "export",
        "command_search",
        "command_describe",
        "command_run",
        "command_batch",
    ] {
        assert!(avail.contains(&want), "{want} should be available");
    }
    for t in catalogue() {
        assert_eq!(avail.contains(&t.name), crate::commands::find(t.requires).is_some(), "{}", t.name);
        if !t.available() {
            let e = call(&mut Session::default(), t.name, &golden(t.name)).unwrap_err().to_string();
            assert!(e.contains(t.requires), "{e}");
        }
    }
}

#[test]
fn project_overview_is_compact() {
    let mut s = demo();
    let out = call(&mut s, "project_overview", &json!({})).unwrap();
    let v = &out.json;
    assert!(v.to_string().len() <= MAX_RESULT_CHARS);
    assert!(v["items"].as_array().unwrap().len() >= 6, "{v}");
    assert!(v["items"].as_array().unwrap().iter().all(|i| i["id"].is_u64() && i["seconds"].is_f64() && i["hasTranscript"] == false));
    let seq = &v["activeSequence"];
    assert_eq!(seq["width"], 1920);
    assert!(seq["seconds"].as_f64().unwrap() > 10.0, "{seq}");
    assert!(seq["video"][0]["clips"][0]["clip"].is_u64(), "{seq}");
    assert!(out.images.is_empty() && out.pending_job.is_none());
}

#[test]
fn frames_come_back_as_images() {
    let mut s = demo();
    let playhead = s.playhead();
    let out = call(&mut s, "contact_sheet", &golden("contact_sheet")).unwrap();
    assert_eq!(out.images.len(), 1);
    let img = &out.images[0];
    assert!(img.png.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert!(img.width <= 512 && img.height <= 512);
    assert_eq!((out.json["width"].as_u64(), out.json["cols"].as_u64()), (Some(u64::from(img.width)), Some(3)));
    assert!(out.json.get("png").is_none(), "the PNG goes in images, not in the JSON");
    let out = call(&mut s, "render_frame", &golden("render_frame")).unwrap();
    assert_eq!((out.images[0].width, out.images[0].height), (256, 144));
    assert_eq!(s.playhead(), playhead);
    // through the command: base64 images
    let v = s.execute("tools.call", json!({"name": "render_frame", "input": golden("render_frame")})).unwrap();
    assert_eq!(v["images"][0]["mimeType"], "image/png");
    assert!(v["images"][0]["png"].as_str().unwrap().starts_with("iVBORw0KGgo"));
}

#[test]
fn read_transcript_pages_by_paragraph() {
    let mut s = demo();
    let seq = s.active_sequence().unwrap();
    let first = &seq.audio_tracks[0].items[0];
    let (item, src_in) = (first.item, first.source_in);
    let tick = |sec: f64| src_in.0 + crate::time::Tick::from_seconds_f64(sec).0;
    // two paragraphs: a 2 s pause between them
    let words: Vec<Value> = ["so", "um", "welcome", "back", "today", "we", "cut"]
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let at = if i < 4 { 0.1 + i as f64 * 0.3 } else { 3.5 + (i - 4) as f64 * 0.3 };
            json!({"text": w, "start": tick(at), "end": tick(at + 0.25), "speaker": 0})
        })
        .collect();
    s.execute("transcript.set", json!({"item": item.0, "transcript": {"language": "en", "speakers": [{"name": "Ada"}], "words": words}})).unwrap();
    let v = call(&mut s, "read_transcript", &json!({"offset": null, "limit": 1})).unwrap().json;
    assert_eq!((v["total"].as_u64(), v["words"].as_u64(), v["next"].as_u64()), (Some(2), Some(7), Some(1)), "{v}");
    let line = v["lines"][0].as_str().unwrap();
    assert!(line.starts_with("[w0–w3 | 00:00.1–") && line.ends_with("| Ada] so um welcome back"), "{line}");
    let v = call(&mut s, "read_transcript", &json!({"offset": 1, "limit": null})).unwrap().json;
    assert_eq!(v["lines"][0].as_str().map(|l| l.starts_with("[w4–w6 | 00:03.5")), Some(true), "{v}");
    assert!(v["next"].is_null());
    let v = call(&mut s, "read_transcript", &json!({"offset": 99, "limit": null})).unwrap().json;
    assert_eq!(v["lines"].as_array().map(Vec::len), Some(0));
    // no transcript: a note, not an error
    let v = call(&mut demo(), "read_transcript", &json!({"offset": null, "limit": null})).unwrap().json;
    assert!(v["note"].is_string(), "{v}");
}

#[test]
fn the_escape_hatch_follows_the_policy() {
    let mut s = demo();
    let v = call(&mut s, "command_search", &json!({"query": "RAZOR"})).unwrap().json;
    let ids: Vec<&str> = v["results"].as_array().unwrap().iter().filter_map(|r| r["id"].as_str()).collect();
    assert!(ids.contains(&"timeline.razor"), "{v}");
    assert!(v["results"].as_array().unwrap().iter().all(|r| r["policy"].is_string() && r["enabled"].is_boolean()));
    let all = call(&mut s, "command_search", &json!({"query": ""})).unwrap().json;
    assert_eq!(all["results"].as_array().map(Vec::len), Some(40));
    assert_eq!(all["more"], true);
    let v = call(&mut s, "command_describe", &json!({"id": "prefs.reset"})).unwrap().json;
    assert_eq!(v["policy"], "deny");
    assert!(call(&mut s, "command_describe", &json!({"id": "nope"})).is_err());

    let e = call(&mut s, "command_run", &json!({"id": "file.quit", "params": null})).unwrap_err().to_string();
    assert!(e.contains("file.quit"), "{e}");
    let v = call(&mut s, "command_run", &json!({"id": "sequence.inspect", "params": null})).unwrap().json;
    assert!(v.get("result").is_some() || v["truncated"] == true, "{v}");
    assert!(call(&mut s, "command_run", &json!({"id": "playhead.set", "params": "[1,2]"})).is_err());
    assert!(call(&mut s, "command_run", &json!({"id": "playhead.set", "params": "{not json"})).is_err());
    let undo = s.history.undo.len();
    call(&mut s, "command_run", &json!({"id": "timeline.razor", "params": "{\"seconds\": 2.5}"})).unwrap();
    assert_eq!(s.history.undo.len(), undo + 1, "an edit is one undo step");
    s.undo();

    // a denied step refuses the whole batch before anything runs
    let before = s.history.undo.len();
    let batch = json!({"steps": [{"id": "timeline.razor", "params": "{\"seconds\": 1.5}"}, {"id": "prefs.reset", "params": null}], "stop_on_error": false});
    assert!(call(&mut s, "command_batch", &batch).is_err());
    assert_eq!(s.history.undo.len(), before);
    let batch = json!({"steps": [{"id": "timeline.razor", "params": "{\"seconds\": 1.5}"}, {"id": "no.such", "params": null}], "stop_on_error": true});
    assert!(call(&mut s, "command_batch", &batch).is_err(), "unknown ids are refused up front too");
    let batch = json!({"steps": [{"id": "timeline.razor", "params": "{\"seconds\": 1.5}"}, {"id": "sequence.addEdit", "params": "{\"time\": \"x\"}"}, {"id": "sequence.inspect", "params": null}], "stop_on_error": null});
    let v = call(&mut s, "command_batch", &batch).unwrap().json;
    assert!(v["completed"].as_u64().unwrap() >= 1, "{v}");
}

#[test]
fn tools_commands_list_and_call() {
    let mut s = demo();
    let list = s.execute("tools.list", json!({})).unwrap();
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), catalogue_available(&s).len());
    for t in list {
        assert!(t["title"].is_string() && t["inputSchema"]["type"] == "object", "{t}");
        for h in ["readOnlyHint", "destructiveHint", "idempotentHint", "openWorldHint"] {
            assert!(t["annotations"][h].is_boolean(), "{t}");
        }
        assert!(matches!(t["approval"].as_str(), Some("never" | "ask" | "askIfOverwrite")));
    }
    let journal = s.journal.len();
    let v = s.execute("tools.call", json!({"name": "project_overview", "input": {}})).unwrap();
    assert!(v["result"]["items"].is_array() && v["images"] == json!([]) && v["job"].is_null(), "{v}");
    assert_eq!(s.journal.len(), journal, "queries leave no journal entry");
    // an edit through a tool is journaled as the command it ran, once
    s.execute("tools.call", json!({"name": "command_run", "input": {"id": "timeline.razor", "params": "{\"seconds\": 2.5}"}})).unwrap();
    assert_eq!(s.journal.last().map(|j| j.0.as_str()), Some("timeline.razor"));
    assert_eq!(s.journal.len(), journal + 1);
    assert!(s.execute("tools.call", json!({"input": {}})).is_err());
    assert!(s.execute("tools.call", json!({"name": "project_overview", "input": {"x": 1}})).is_err());
}

#[test]
fn export_returns_its_job() {
    let mut s = demo();
    let dir = std::env::temp_dir().join(format!("fc-tools-export-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("out.wav").to_string_lossy().to_string();
    let out = call(&mut s, "export", &json!({"path": path, "preset": null, "format": "wav", "range": null, "sequence": null, "burn_captions": null})).unwrap();
    let job = out.pending_job.expect("export starts a job");
    let t0 = std::time::Instant::now();
    loop {
        let jobs = s.execute("jobs.list", json!({})).unwrap();
        let j = jobs.as_array().unwrap().iter().find(|j| j["id"] == job).cloned().unwrap();
        if j["finished"] == true {
            assert!(j["result"]["error"].is_null(), "{j}");
            break;
        }
        assert!(t0.elapsed().as_secs() < 120, "export did not finish");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(std::path::Path::new(&path).exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn long_results_are_capped() {
    let v = cap(json!({"text": "é".repeat(MAX_RESULT_CHARS)}));
    assert_eq!(v["truncated"], true);
    assert!(v["text"].as_str().unwrap().len() <= MAX_RESULT_CHARS);
    assert_eq!(cap(json!({"a": 1})), json!({"a": 1}));
}

#[test]
fn strict_api_schemas_describe_the_engine_checks() {
    let t = find("contact_sheet").unwrap();
    let api = schema::for_strict_api(&t.schema_value());
    let d = api["properties"]["count"]["description"].as_str().unwrap();
    assert!(d.contains("minimum 1") && d.contains("maximum 48"), "{d}");
    assert_eq!(api["additionalProperties"], false);
    assert_eq!(api["required"], t.schema_value()["required"]);
}

/// The demo project with two more copies of its sequence, named like variations (one name with a
/// path separator, one starting with `../`). Returns the three ids.
fn variations_project() -> (Session, [u64; 3]) {
    let mut s = demo();
    let first = s.state.active_sequence.unwrap();
    let seq = s.active_sequence().unwrap().clone();
    let p = std::sync::Arc::make_mut(&mut s.project);
    p.item_mut(first).unwrap().name = "Cut: 30s".into();
    let kind = || filmcraft_project::ItemKind::Sequence(Box::new(seq.clone()));
    let b = p.add_item("Cut/30s", filmcraft_project::Label::Iris, kind(), None);
    let c = p.add_item("../Vertical", filmcraft_project::Label::Iris, kind(), None);
    (s, [first.0, b.0, c.0])
}

fn scratch(name: &str) -> std::path::PathBuf {
    let d = filmcraft_testkit::workspace_root().join("target").join("tools-tests").join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Follow a job the way the hosts do: poll the session until `jobs.list` says it finished.
fn follow(s: &mut Session, job: u64) -> Value {
    let t0 = std::time::Instant::now();
    loop {
        s.poll_persistence();
        let jobs = s.execute("jobs.list", json!({})).unwrap();
        let j = jobs.as_array().unwrap().iter().find(|j| j["id"] == job).cloned().unwrap();
        if j["finished"] == true {
            return j;
        }
        assert!(t0.elapsed().as_secs() < 180, "job {job} did not finish: {j}");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn queue_len(s: &mut Session) -> usize {
    s.execute("export.queue.list", json!({})).unwrap()["items"].as_array().map_or(0, Vec::len)
}

#[test]
fn export_variations_queues_one_file_per_sequence_and_follows_them_as_one_job() {
    let (mut s, ids) = variations_project();
    let dir = scratch("variations");
    let folder = dir.to_string_lossy().to_string();
    let undo = s.history.undo.len();
    let input = json!({"sequences": ids, "preset": "Waveform Audio 48 kHz 16-bit", "folder": folder, "overwrite": null});
    let out = call(&mut s, "export_variations", &input).unwrap();
    let job = out.pending_job.expect("the batch runs as one job");
    assert_eq!(out.json["job"], job);
    let names: Vec<String> = out.json["files"].as_array().unwrap().iter().map(|f| f["path"].as_str().unwrap().to_string()).collect();
    let want: Vec<String> = ["Cut_ 30s.wav", "Cut_30s.wav", "_Vertical.wav"].iter().map(|n| dir.join(n).to_string_lossy().to_string()).collect();
    assert_eq!(names, want, "sanitised `<sequence name>.<ext>` in the folder");
    // the queue has one item per sequence with the preset
    let q = s.execute("export.queue.list", json!({})).unwrap()["items"].as_array().unwrap().clone();
    assert_eq!(q.len(), 3);
    for (it, id) in q.iter().zip(ids) {
        assert_eq!((it["sequence"].as_u64(), it["preset"].as_str()), (Some(id), Some("Waveform Audio 48 kHz 16-bit")), "{it}");
    }
    let j = follow(&mut s, job);
    assert!(j["result"]["error"].is_null(), "{j}");
    assert_eq!(j["progress"].as_f64(), Some(1.0), "{j}");
    for p in &want {
        assert!(std::path::Path::new(p).exists(), "{p} was written");
    }
    assert_eq!(j["result"]["extra_files"].as_array().map(Vec::len), Some(2), "{j}");
    assert_eq!(s.history.undo.len(), undo, "exporting adds no undo step");

    // the files exist now: a second run is refused and queues nothing
    let before = queue_len(&mut s);
    let e = call(&mut s, "export_variations", &input).unwrap_err().to_string();
    assert!(e.contains("already exist") && e.contains("_Vertical.wav"), "{e}");
    assert_eq!(queue_len(&mut s), before);
    let one = |overwrite: Value| json!({"sequences": [ids[2]], "preset": "Waveform Audio 48 kHz 16-bit", "folder": folder, "overwrite": overwrite});
    let e = call(&mut s, "export_variations", &one(json!(false))).unwrap_err().to_string();
    assert!(e.contains("_Vertical.wav"), "{e}");
    // unless the user agreed to replace them
    let out = call(&mut s, "export_variations", &one(json!(true))).unwrap();
    let j = follow(&mut s, out.pending_job.unwrap());
    assert!(j["result"]["error"].is_null(), "{j}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn colliding_names_are_numbered() {
    let (mut s, ids) = variations_project();
    let p = std::sync::Arc::make_mut(&mut s.project);
    p.item_mut(filmcraft_project::ItemId(ids[1])).unwrap().name = "Cut: 30s".into();
    p.item_mut(filmcraft_project::ItemId(ids[2])).unwrap().name = "...".into();
    let dir = scratch("variations-names");
    let input = json!({"sequences": ids, "preset": "Waveform Audio 48 kHz 16-bit", "folder": dir.to_string_lossy(), "overwrite": null});
    let out = call(&mut s, "export_variations", &input).unwrap();
    let names: Vec<String> = out.json["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| std::path::Path::new(f["path"].as_str().unwrap()).file_name().unwrap().to_string_lossy().to_string())
        .collect();
    assert_eq!(names, ["Cut_ 30s.wav".to_string(), "Cut_ 30s (2).wav".to_string(), format!("Sequence {}.wav", ids[2])]);
    follow(&mut s, out.pending_job.unwrap());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cancelling_the_variations_job_cancels_its_exports() {
    let (mut s, ids) = variations_project();
    let dir = scratch("variations-cancel");
    let input = json!({"sequences": ids, "preset": "Apple ProRes 422 HQ", "folder": dir.to_string_lossy(), "overwrite": null});
    let job = call(&mut s, "export_variations", &input).unwrap().pending_job.unwrap();
    s.execute("jobs.cancel", json!({"job": job})).unwrap();
    let j = follow(&mut s, job);
    assert_eq!(j["result"]["error"], "cancelled", "{j}");
    let t0 = std::time::Instant::now();
    loop {
        let q = s.execute("export.queue.list", json!({})).unwrap()["items"].as_array().unwrap().clone();
        if q.iter().all(|i| i["status"] == "cancelled" || i["status"] == "done") {
            assert!(q.iter().any(|i| i["status"] == "cancelled"), "{q:?}");
            break;
        }
        assert!(t0.elapsed().as_secs() < 60, "{q:?}");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn export_variations_refuses_hostile_input() {
    let (mut s, ids) = variations_project();
    let dir = scratch("variations-hostile");
    let folder = dir.to_string_lossy().to_string();
    let wav = "Waveform Audio 48 kHz 16-bit";
    for input in [
        json!({"sequences": [], "preset": wav, "folder": folder, "overwrite": null}),
        json!({"sequences": [1, 2, 3, 4, 5, 6, 7], "preset": wav, "folder": folder, "overwrite": null}),
        json!({"sequences": [-1], "preset": wav, "folder": folder, "overwrite": null}),
        json!({"sequences": [1.5], "preset": wav, "folder": folder, "overwrite": null}),
        json!({"sequences": "1", "preset": wav, "folder": folder, "overwrite": null}),
        json!({"sequences": [ids[0]], "preset": wav, "folder": folder}),
        json!({"sequences": [ids[0]], "preset": wav, "folder": folder, "overwrite": "yes"}),
        json!({"sequences": [ids[0]], "preset": wav, "folder": "x".repeat(5000), "overwrite": null}),
        json!({"sequences": [ids[0]], "preset": wav, "folder": folder, "overwrite": null, "wait": true}),
    ] {
        assert!(validate("export_variations", &input).is_err(), "{input}");
        assert!(call(&mut s, "export_variations", &input).is_err(), "{input}");
    }
    // valid shape, bad values: refused with a reason, nothing queued
    let missing = dir.join("missing").to_string_lossy().to_string();
    for (input, why) in [
        (json!({"sequences": [ids[0]], "preset": "No Such Preset", "folder": folder, "overwrite": null}), "No Such Preset"),
        (json!({"sequences": [ids[0]], "preset": "TIFF Sequence", "folder": folder, "overwrite": null}), "stills"),
        (json!({"sequences": [ids[0]], "preset": wav, "folder": "relative/dir", "overwrite": null}), "absolute"),
        (json!({"sequences": [ids[0]], "preset": wav, "folder": "", "overwrite": null}), "absolute"),
        (json!({"sequences": [ids[0]], "preset": wav, "folder": format!("{folder}/\u{0}x"), "overwrite": null}), "absolute"),
        (json!({"sequences": [ids[0]], "preset": wav, "folder": missing, "overwrite": null}), "does not exist"),
        (json!({"sequences": [ids[0], ids[0]], "preset": wav, "folder": folder, "overwrite": null}), "twice"),
        (json!({"sequences": [u64::MAX], "preset": wav, "folder": folder, "overwrite": null}), "not a sequence"),
        // a media item is not a sequence
        (json!({"sequences": [ids[0], 1], "preset": wav, "folder": folder, "overwrite": null}), "not a sequence"),
    ] {
        let e = call(&mut s, "export_variations", &input).unwrap_err().to_string();
        assert!(e.contains(why), "{input}: {e}");
    }
    assert_eq!(queue_len(&mut s), 0);
    assert!(s.jobs.is_empty(), "no job started");
    // disabled: no sequence open
    let e = call(&mut Session::default(), "export_variations", &json!({"sequences": [1], "preset": wav, "folder": folder, "overwrite": null}));
    assert!(e.is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn export_variations_always_asks() {
    assert_eq!(find("export_variations").map(|t| (t.approval, t.destructive, t.read_only)), Some((Approval::Ask, true, false)));
    for overwrite in [json!(null), json!(false), json!(true)] {
        let input = json!({"sequences": [1], "preset": "YouTube 1080p Full HD", "folder": "/tmp", "overwrite": overwrite});
        assert_eq!(approval_for("export_variations", &input), Approval::Ask);
    }
}

#[test]
fn export_queue_start_follow_rejects_hostile_ids() {
    let mut s = demo();
    let dir = scratch("queue-follow");
    let id = s
        .execute("export.queue.add", json!({"preset": "Waveform Audio 48 kHz 16-bit", "path": dir.join("a.wav").to_string_lossy(), "range": "entire"}))
        .unwrap()["added"][0]
        .as_u64()
        .unwrap();
    for follow in [json!([]), json!("1"), json!([999]), json!([-1]), json!([1.5]), json!({"id": id}), json!(vec![id; 257])] {
        assert!(s.execute("export.queue.start", json!({"follow": follow})).is_err(), "{follow}");
    }
    assert!(s.jobs.is_empty() && !s.export_queue.running, "nothing started");
    let r = s.execute("export.queue.start", json!({"follow": [id, id], "wait": true})).unwrap();
    let j = follow(&mut s, r["job"].as_u64().unwrap());
    assert!(j["result"]["error"].is_null(), "{j}");
    // a finished item cannot be followed again
    assert!(s.execute("export.queue.start", json!({"follow": [id]})).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
