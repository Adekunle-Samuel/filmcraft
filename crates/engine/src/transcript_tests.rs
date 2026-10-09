use std::sync::Arc;

use serde_json::json;

use crate::Session;
use filmcraft_project::{ItemId, Transcript, Word};
use filmcraft_speech::FixedTranscriber;
use filmcraft_time::Tick;

/// The demo project with a fake transcriber whose transcript fits the first A1 clip's media:
/// "Hello um world." (Speaker 1), a 1.2 s pause, "Second speaker here." (Speaker 2).
fn session() -> (Session, ItemId, Tick) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    let a = &q.audio_tracks[0].items[0];
    let (item, sin) = (a.item, a.source_in);
    let sec = |x: f64| sin + Tick::from_seconds_f64(x);
    let mut t = Transcript { language: "en".into(), ..Default::default() };
    for (text, a, b, sp) in
        [("Hello", 0.2, 0.5, 0), ("um", 0.6, 0.9, 0), ("world.", 1.0, 1.4, 0), ("Second", 2.6, 3.0, 1), ("speaker", 3.0, 3.5, 1), ("here.", 3.5, 4.0, 1)]
    {
        let mut w = Word::new(text, sec(a), sec(b));
        w.speaker = Some(sp);
        t.words.push(w);
    }
    t.normalize();
    s.transcriber = Some(Arc::new(FixedTranscriber { transcript: t, id: "fixed".into() }));
    let start = a_start(&s);
    (s, item, start)
}

fn a_start(s: &Session) -> Tick {
    s.active_sequence().unwrap().audio_tracks[0].items[0].start
}

fn words(s: &mut Session) -> Vec<String> {
    let r = s.execute("transcript.inspect", json!({})).unwrap();
    r["words"].as_array().unwrap().iter().map(|w| w["text"].as_str().unwrap().to_string()).collect()
}

#[test]
fn generate_inspect_search_and_rename() {
    let (mut s, item, start) = session();
    assert!(s.execute("transcript.select", json!({"from": 0})).is_err(), "disabled before transcribing");
    let r = s.execute("transcript.generate", json!({"items": [item.0], "wait": true})).unwrap();
    assert_eq!(r["items"][0]["words"], 6, "{r}");
    assert_eq!(r["items"][0]["source"], "fixed");
    assert_eq!(s.project.transcripts[&item].words.len(), 6);

    let r = s.execute("transcript.inspect", json!({})).unwrap();
    assert_eq!(words(&mut s), ["Hello", "um", "world.", "Second", "speaker", "here."]);
    assert_eq!(r["words"][0]["start"].as_i64().unwrap(), (start + Tick::from_seconds_f64(0.2)).0, "mapped to sequence time");
    assert_eq!(r["paragraphs"].as_array().unwrap().len(), 2, "speaker change splits paragraphs: {}", r["paragraphs"]);
    assert_eq!(r["speakers"], json!(["Speaker 1", "Speaker 2"]));

    let r = s.execute("transcript.search", json!({"query": "second spea"})).unwrap();
    assert_eq!(r["matches"], json!([{"from": 3, "to": 4, "start": r["matches"][0]["start"], "end": r["matches"][0]["end"]}]));

    s.execute("transcript.renameSpeaker", json!({"speaker": "Speaker 2", "name": "Ann"})).unwrap();
    let r = s.execute("transcript.inspect", json!({})).unwrap();
    assert_eq!(r["words"][3]["speaker"], "Ann");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.transcripts[&item].speakers[1].name, "Speaker 2");
    s.execute("transcript.renameSpeaker", json!({"speaker": 0, "item": item.0, "name": "Bo"})).unwrap();
    assert_eq!(s.project.transcripts[&item].speakers[0].name, "Bo");
    assert!(s.execute("transcript.renameSpeaker", json!({"speaker": "Nobody", "name": "X"})).is_err());

    // transcribing is one undo step
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.project.transcripts.is_empty());
}

#[test]
fn select_extract_and_lift_by_words() {
    let (mut s, item, _) = session();
    s.execute("transcript.generate", json!({"items": [item.0], "wait": true})).unwrap();
    let rate = s.sequence_rate();
    let before = s.active_sequence().unwrap().duration();

    let r = s.execute("transcript.select", json!({"from": 3, "to": 5})).unwrap();
    let (a, b) = (Tick(r["start"].as_i64().unwrap()), Tick(r["end"].as_i64().unwrap()));
    let q = s.active_sequence().unwrap();
    assert_eq!(q.mark_in, Some(a));
    assert_eq!(q.mark_out, Some(b - rate.frame_duration()), "Out is the last frame inside");
    assert_eq!(rate.snap(a), a, "frame aligned");
    assert_eq!(s.playhead(), a);

    // Extract "um": the sequence gets shorter by the word's frames and the word is gone
    let r = s.execute("transcript.extract", json!({"from": 1})).unwrap();
    let cut = Tick(r["end"].as_i64().unwrap()) - Tick(r["start"].as_i64().unwrap());
    assert!(cut > Tick::ZERO);
    assert_eq!(s.active_sequence().unwrap().duration(), before - cut);
    assert!(!words(&mut s).contains(&"um".to_string()), "{:?}", words(&mut s));
    s.active_sequence().unwrap().check().unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active_sequence().unwrap().duration(), before);

    // Lift leaves a gap: same duration, word gone
    s.execute("transcript.lift", json!({"from": 1, "to": 1})).unwrap();
    assert_eq!(s.active_sequence().unwrap().duration(), before);
    assert_eq!(words(&mut s), ["Hello", "world.", "Second", "speaker", "here."]);
    assert!(s.execute("transcript.extract", json!({"from": 99})).is_err());
}

#[test]
fn remove_fillers_pauses_and_create_captions() {
    let (mut s, item, _) = session();
    s.execute("transcript.generate", json!({"items": [item.0], "wait": true})).unwrap();
    let before = s.active_sequence().unwrap().duration();
    let r = s.execute("transcript.removeFillers", json!({})).unwrap();
    assert_eq!(r["removed"], 1, "{r}");
    assert!(!words(&mut s).contains(&"um".to_string()));
    let r = s.execute("transcript.removePauses", json!({"minSeconds": 1.0, "keepSeconds": 0.1})).unwrap();
    assert_eq!(r["removed"], 1, "{r}");
    let after = s.active_sequence().unwrap().duration();
    assert!(after < before - Tick::from_seconds_f64(1.0), "the pause and the filler are gone");
    assert_eq!(words(&mut s).len(), 5);
    s.active_sequence().unwrap().check().unwrap();

    let r = s.execute("transcript.createCaptions", json!({"maxChars": 32})).unwrap();
    assert_eq!(r["captions"], 2, "one caption per speaker: {r}");
    let q = s.active_sequence().unwrap();
    let tr = &q.caption_tracks[0];
    assert_eq!(tr.captions[0].text, "Hello world.");
    assert_eq!(tr.captions[1].speaker.as_deref(), Some("Speaker 2"));
    q.check().unwrap();
}

#[test]
fn set_delete_and_models() {
    let (mut s, item, _) = session();
    let t = json!({"language": "en", "words": [
        {"text": "b", "start": 2000, "end": 3000, "speaker": 1},
        {"text": "a", "start": 0, "end": 1000},
    ]});
    let r = s.execute("transcript.set", json!({"item": item.0, "transcript": t})).unwrap();
    assert_eq!(r["words"], 2);
    let tr = &s.project.transcripts[&item];
    assert_eq!(tr.words[0].text, "a", "normalized: sorted");
    assert_eq!(tr.speakers.len(), 2, "missing speaker labels added");
    assert_eq!(tr.source, "imported");
    assert!(s.execute("transcript.set", json!({"item": 999_999, "transcript": {}})).is_err());
    assert!(s.execute("transcript.set", json!({"item": item.0, "transcript": {"words": 3}})).is_err());

    // survives save/load
    let bytes = filmcraft_format::encode(&s.project, true);
    let back = filmcraft_format::decode(&bytes).unwrap();
    assert_eq!(back.project.transcripts[&item].words.len(), 2);

    s.execute("transcript.delete", json!({"items": [item.0]})).unwrap();
    assert!(s.project.transcripts.is_empty());
    assert!(s.execute("transcript.delete", json!({})).is_err());

    let m = s.execute("transcript.models", json!({})).unwrap();
    assert_eq!(m["available"], filmcraft_speech::available());
    assert!(m["models"].as_array().unwrap().iter().any(|x| x["id"] == "whisper-base"));
}

#[test]
fn generate_without_a_transcriber() {
    let (mut s, item, _) = session();
    s.transcriber = None;
    let e = s.execute("transcript.generate", json!({"items": [item.0], "model": "nope"})).unwrap_err().to_string();
    // a build without speech-to-text says that first: the command is disabled (#97)
    let why = if filmcraft_speech::available() { "unknown speech model" } else { "not available in this build" };
    assert!(e.contains(why), "{e}");
    if !filmcraft_speech::available() {
        assert!(!s.is_enabled("sequence.transcribe"), "Transcribe Sequence follows transcript.generate");
        let e = s.execute("transcript.generate", json!({"items": [item.0], "wait": true})).unwrap_err().to_string();
        assert!(e.contains("whisper") && e.contains("not available"), "{e}");
        #[cfg(not(feature = "speech-download"))]
        assert!(s.execute("transcript.downloadModel", json!({})).unwrap_err().to_string().contains("not available"));
    }
    // defaults to the media of the open sequence's audio clips
    s.transcriber = Some(Arc::new(FixedTranscriber { transcript: Transcript::default(), id: "empty".into() }));
    assert!(s.is_enabled("sequence.transcribe"), "an installed recogniser enables Transcribe Sequence");
    let r = s.execute("transcript.generate", json!({"wait": true})).unwrap();
    assert!(r["items"].as_array().unwrap().len() >= 2, "{r}");
}

/// Poll until `job` finished (and its results were applied), at most 30 s.
fn finish_job(s: &mut Session, job: u64) -> serde_json::Value {
    let t0 = std::time::Instant::now();
    loop {
        s.poll_persistence();
        let j = s.jobs.iter().find(|j| j.id == job).unwrap().to_json();
        if j["finished"] == true && s.transcript_jobs.is_empty() {
            return j;
        }
        assert!(t0.elapsed() < std::time::Duration::from_secs(30), "job {job} never finished: {j}");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn generate_runs_as_a_background_job() {
    let (mut s, item, _) = session();
    let undo = s.history.undo.len();
    let r = s.execute("transcript.generate", json!({"items": [item.0]})).unwrap();
    let job = r["job"].as_u64().unwrap();
    assert_eq!(r["items"], 1, "{r}");
    // the same media can't be transcribed twice at once
    if !s.transcript_jobs.is_empty() {
        let e = s.execute("transcript.generate", json!({"items": [item.0]})).unwrap_err().to_string();
        assert!(e.contains("already being transcribed"), "{e}");
    }
    let j = finish_job(&mut s, job);
    assert_eq!(j["result"]["frames"], 6, "words reported: {j}");
    assert_eq!(j["done"], j["total"]);
    assert_eq!(s.project.transcripts[&item].words.len(), 6);
    assert_eq!(s.history.undo.len(), undo + 1, "one undo step");
    assert_eq!(s.history.undo.last().unwrap().0, "Transcribe");
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.project.transcripts.is_empty());
}

/// Reports progress until it is told to stop (or 20 s pass).
struct Slow;
impl filmcraft_speech::Transcriber for Slow {
    fn id(&self) -> String {
        "slow".into()
    }
    fn transcribe(
        &self,
        _: &[f32],
        _: &filmcraft_speech::Options,
        progress: filmcraft_speech::ProgressFn,
    ) -> Result<Transcript, filmcraft_speech::SpeechError> {
        for k in 0..4000 {
            if !progress((k as f32 / 4000.0).min(0.99), "Transcribing") {
                return Err(filmcraft_speech::SpeechError::Cancelled);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        Ok(Transcript::default())
    }
}

#[test]
fn generate_job_reports_progress_and_cancels() {
    let (mut s, item, _) = session();
    s.transcriber = Some(Arc::new(Slow));
    let r = s.execute("transcript.generate", json!({"items": [item.0]})).unwrap();
    let job = r["job"].as_u64().unwrap();
    // the recogniser's progress reaches the job
    let t0 = std::time::Instant::now();
    loop {
        let j = s.execute("jobs.list", json!({})).unwrap();
        let j = j.as_array().unwrap().iter().find(|x| x["id"] == job).unwrap().clone();
        if j["done"].as_u64().unwrap() > 0 && j["status"].as_str().unwrap().contains("Transcribing") {
            assert_eq!(j["total"], 1000);
            break;
        }
        assert!(t0.elapsed() < std::time::Duration::from_secs(20), "no progress: {j}");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    s.execute("jobs.cancel", json!({"job": job})).unwrap();
    let j = finish_job(&mut s, job);
    assert_eq!(j["result"]["error"], "stopped", "{j}");
    assert!(s.project.transcripts.is_empty(), "a cancelled job changes nothing");
    assert!(s.history.undo.is_empty());
}

struct Buggy;
impl filmcraft_speech::Transcriber for Buggy {
    fn id(&self) -> String {
        "buggy".into()
    }
    fn transcribe(&self, _: &[f32], _: &filmcraft_speech::Options, _: filmcraft_speech::ProgressFn) -> Result<Transcript, filmcraft_speech::SpeechError> {
        panic!("a recogniser bug")
    }
}

#[test]
fn a_panicking_recogniser_fails_the_job_not_the_app() {
    let (mut s, item, _) = session();
    s.transcriber = Some(Arc::new(Buggy));
    let r = s.execute("transcript.generate", json!({"items": [item.0]})).unwrap();
    let j = finish_job(&mut s, r["job"].as_u64().unwrap());
    assert!(j["result"]["error"].as_str().unwrap().contains("internal error"), "{j}");
    assert!(s.project.transcripts.is_empty());
    // waiting: the same guard turns it into an error
    let e = s.execute("transcript.generate", json!({"items": [item.0], "wait": true})).unwrap_err().to_string();
    assert!(e.contains("internal error"), "{e}");
    assert!(s.transcript_jobs.is_empty());
}

/// Wait for every transcription job and apply it.
fn settle(s: &mut Session) {
    let t0 = std::time::Instant::now();
    while !s.transcript_jobs.is_empty() && t0.elapsed() < std::time::Duration::from_secs(30) {
        std::thread::sleep(std::time::Duration::from_millis(5));
        s.poll_persistence();
    }
    assert!(s.transcript_jobs.is_empty());
}

#[test]
fn generate_hostile_params() {
    let (mut s, item, _) = session();
    for p in [
        json!({"items": [u64::MAX], "wait": true}),
        json!({"items": "x", "wait": true}),
        json!({"items": [item.0], "maxSpeakers": -5, "wait": true}),
        json!({"items": [item.0], "maxSpeakers": 1e300, "language": 7, "diarize": "yes", "wait": "no"}),
    ] {
        let _ = s.execute("transcript.generate", p);
        settle(&mut s);
    }
}

/// Records what it was asked to transcribe and returns a word every 0.25 s of the audio it got
/// (each 0.4 s long, so some cross the joins between regions).
#[derive(Default)]
struct Recorder {
    seen: std::sync::Mutex<Vec<(usize, Option<String>)>>,
}
impl filmcraft_speech::Transcriber for Recorder {
    fn id(&self) -> String {
        "recorder".into()
    }
    fn transcribe(
        &self,
        audio: &[f32],
        opts: &filmcraft_speech::Options,
        progress: filmcraft_speech::ProgressFn,
    ) -> Result<Transcript, filmcraft_speech::SpeechError> {
        self.seen.lock().unwrap().push((audio.len(), opts.initial_prompt.clone()));
        let end = filmcraft_speech::sample_tick(audio.len() as i64);
        let mut t = Transcript { language: "en".into(), source: "recorder".into(), ..Default::default() };
        let mut at = Tick::ZERO;
        while at < end {
            t.words.push(Word::new("w", at, at + Tick::from_seconds_f64(0.4)));
            at += Tick::from_seconds_f64(0.25);
        }
        progress(1.0, "Done");
        Ok(t)
    }
}

fn media_seconds(t: Tick) -> f64 {
    t.0 as f64 / filmcraft_time::TICKS_PER_SECOND as f64
}

#[test]
fn regions_transcribe_only_voiced_audio_and_map_back_to_media_time() {
    let (mut s, item, _) = session();
    let rec = Arc::new(Recorder::default());
    s.transcriber = Some(rec.clone());
    // overlapping regions merge once padded; times are media seconds
    let r = s.execute("transcript.generate", json!({"items": [item.0], "regions": [[3.0, 3.2], [1.0, 1.5], [1.6, 1.7]], "wait": true})).unwrap();
    assert_eq!(r["items"][0]["source"], "recorder", "{r}");
    // padded by 0.3 s: [0.7, 2.0] and [2.7, 3.5] → 2.1 s of audio, nothing else
    let (len, prompt) = rec.seen.lock().unwrap()[0].clone();
    assert_eq!(len, (2.1 * 16_000.0f64).round() as usize);
    assert_eq!(prompt, None, "no prompt by default");
    let words = &s.project.transcripts[&item].words;
    assert!(words.len() >= 8, "{}", words.len());
    for w in words {
        let (a, b) = (media_seconds(w.start), media_seconds(w.end));
        let inside = |lo: f64, hi: f64| a >= lo - 1e-9 && b <= hi + 1e-9 && a <= b;
        assert!(inside(0.7, 2.0) || inside(2.7, 3.5), "word {a}..{b} left its region");
    }
    assert!(words.iter().any(|w| media_seconds(w.start) >= 2.7), "the second region has words");
    assert_eq!(s.history.undo.last().unwrap().0, "Transcribe", "one undo step");
}

#[test]
fn keep_fillers_and_prompt_reach_the_recogniser() {
    let (mut s, item, _) = session();
    let rec = Arc::new(Recorder::default());
    s.transcriber = Some(rec.clone());
    for (p, want) in [
        (json!({"keepFillers": true}), Some(filmcraft_speech::FILLER_PROMPT.to_string())),
        (json!({"keepFillers": false}), None),
        (json!({"keepFillers": true, "prompt": "  FilmCraft, Lumetri. "}), Some("FilmCraft, Lumetri.".to_string())),
        (json!({"prompt": "   "}), None),
        (json!({"prompt": "x".repeat(50_000)}), Some("x".repeat(2000))),
    ] {
        let mut p = p;
        p["items"] = json!([item.0]);
        p["wait"] = json!(true);
        s.execute("transcript.generate", p.clone()).unwrap();
        assert_eq!(rec.seen.lock().unwrap().last().unwrap().1, want, "{p}");
    }
    assert!(s.execute("transcript.generate", json!({"items": [item.0], "prompt": 5, "wait": true})).is_err());
}

#[test]
fn remapped_words_stay_inside_their_regions() {
    use crate::transcript::{Span, remap_words};
    let sec = |x: f64| filmcraft_speech::seconds_tick(x);
    let spans = [Span { start: 16_000, len: 8_000 }, Span { start: 80_000, len: 16_000 }];
    // joined audio: 0..0.5 s is media 1.0..1.5, 0.5..1.5 s is media 5.0..6.0
    let mut words = vec![
        Word::new("a", sec(0.1), sec(0.2)),
        Word::new("b", sec(0.4), sec(0.7)), // crosses the join: cut at the first region's end
        Word::new("c", sec(0.5), sec(0.6)),
        Word::new("d", sec(1.4), sec(9.0)), // runs past the audio
        Word::new("e", sec(2.0), sec(2.1)), // past the audio: dropped
        Word::new("f", Tick(-50), sec(0.05)),
    ];
    remap_words(&mut words, &spans);
    let got: Vec<(String, f64, f64)> = words.iter().map(|w| (w.text.clone(), media_seconds(w.start), media_seconds(w.end))).collect();
    let close = |x: f64, y: f64| (x - y).abs() < 1e-6;
    let want = [("a", 1.1, 1.2), ("b", 1.4, 1.5), ("c", 5.0, 5.1), ("d", 5.9, 6.0), ("f", 1.0, 1.05)];
    assert_eq!(got.len(), want.len(), "{got:?}");
    for ((t, a, b), (wt, wa, wb)) in got.iter().zip(want) {
        assert!(t == wt && close(*a, wa) && close(*b, wb), "{got:?}");
    }
    let mut none = vec![Word::new("x", sec(0.1), sec(0.2))];
    remap_words(&mut none, &[]);
    assert!(none.is_empty());
}

#[test]
fn hostile_regions_are_refused_or_clamped() {
    use crate::transcript::{MAX_REGIONS, Span, region_spans};
    let len = 16_000 * 10; // 10 s
    for bad in [
        json!("1-2"),
        json!([[1.0]]),
        json!([[1.0, 2.0, 3.0]]),
        json!([["a", 2.0]]),
        json!([[2.0, 1.0]]),
        json!([[f64::MAX, -f64::MAX]]),
        json!([[100.0, 200.0]]),
        json!([]),
        json!(vec![[0.0, 1.0]; MAX_REGIONS + 1]),
    ] {
        assert!(region_spans(&bad, len).is_err(), "{bad}");
    }
    // huge and negative values clamp to the audio
    assert_eq!(region_spans(&json!([[-5.0, f64::MAX]]), len).unwrap(), [Span { start: 0, len }]);
    assert_eq!(region_spans(&json!([[9.9, 1e300]]), len).unwrap(), [Span { start: 16_000 * 96 / 10, len: 16_000 * 4 / 10 }]);
    // many regions: merged into what they cover
    let many: Vec<[f64; 2]> = (0..MAX_REGIONS).map(|i| [i as f64 * 0.001, i as f64 * 0.001 + 0.0005]).collect();
    assert_eq!(region_spans(&json!(many), len).unwrap(), [Span { start: 0, len }]);
    assert_eq!(region_spans(&json!([[1.0, 1.0]]), 0).ok(), None, "no audio at all");

    // through the command: errors, never panics, nothing stored
    let (mut s, item, _) = session();
    s.transcriber = Some(Arc::new(Recorder::default()));
    let other = s.active_sequence().unwrap().audio_tracks[0].items.iter().map(|i| i.item).find(|i| *i != item);
    for p in [
        json!({"items": [item.0], "regions": [[2.0, 1.0]], "wait": true}),
        json!({"items": [item.0], "regions": [[1e12, 2e12]], "wait": true}),
        json!({"items": [item.0], "regions": {"a": 1}, "wait": true}),
        json!({"items": [item.0], "regions": vec![[0.0, 1.0]; MAX_REGIONS + 1], "wait": true}),
        json!({"items": [item.0, other.map(|o| o.0).unwrap_or(u64::MAX)], "regions": [[1.0, 2.0]], "wait": true}),
    ] {
        assert!(s.execute("transcript.generate", p.clone()).is_err(), "{p}");
        assert!(s.project.transcripts.is_empty(), "{p}");
    }
    assert!(s.transcript_jobs.is_empty());
}
