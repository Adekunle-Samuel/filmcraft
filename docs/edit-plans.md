# Edit plans

An **edit plan** describes a whole edit in one JSON document: which words, ranges, fillers, pauses
and silences to cut, whether to add captions and markers, and where the result goes. An assistant
(or any agent or script) writes the plan; FilmCraft validates it, shows what it would do, and
applies it as **one undo step**, by default into a **new sequence** so the source stays untouched.
The LLM never edits the timeline directly.

Types and the compiler live in `crates/edit/src/plan.rs` (pure, no I/O); the commands live in
`crates/engine/src/edit_plan.rs`.

## Commands

| Command | Params | Result |
|---|---|---|
| `plan.validate` | `{"plan": EditPlan \| str}` | `{ok, errors: [str], warnings: [str]}`. Never fails on a bad plan; it lists every problem by field (`cuts.removeWords[2].to: word 99 is out of range (the transcript has 20 words)`). |
| `plan.preview` | `{"plan": EditPlan \| str}` | Each removal (`start`/`end`/`duration` seconds, `startTick`/`endTick`, `reason`, `kind`, and the transcript `text` inside it), `before`/`after`/`removedSeconds`, `segments`, `words {total, kept}`, the estimated `captions` count, `markers`, `warnings`, `skipped`, the output `name` and `mode`, and `sourceHash`. Changes nothing. |
| `plan.apply` | `{"plan": EditPlan \| str, "sourceHash": str}` | `{sequence, name, removedS, durationS, removals, captions, markers, warnings, skipped}`. One undo step. |
| `plan.applyVariations` | `{"plans": [EditPlan \| str] (1–6), "sourceHash": str}` | `{sequences: [<as plan.apply>]}`: one new sequence per plan, named `<name> — v1…vN`, all in one undo step. Every plan must edit the same source and go into a new sequence. |

All four are disabled while the project has no sequence. `plan` may be the plan object or its
JSON text.

**Stale previews.** `sourceHash` is a stable FNV-1a hash of the source sequence (serialised) and
of the transcripts its words come from. `plan.apply` recomputes it and refuses with "the source
sequence or its transcript changed since plan.preview" when it differs: word indices and times
in the plan would otherwise point at the wrong material. Preview again and apply the new hash.

**Applying** copies the source sequence into a new project item next to it (`output.mode:
"inPlace"` edits the source instead), ripple-deletes the compiled removals on every unlocked track
(sequence markers follow the cuts), adds captions built from the *edited* sequence's transcript on
a new caption track, adds the plan's markers at their edited times, and opens the sequence. Any
error rolls the whole step back.

## Format

camelCase keys; unknown keys are an error; at most 2 MiB of JSON. Only `version` (always `1`)
and `title` are required: `{"version":1,"title":"x"}` is a valid plan that changes nothing.

```json
{
  "version": 1,
  "title": "Tight interview",
  "rationale": "Cut the tangent about parking and the dead air.",
  "source": {"sequence": 12},
  "output": {"mode": "newSequence", "name": "Interview — tight", "aspect": "16:9"},
  "cuts": {
    "removeWords": [{"from": 120, "to": 164, "reason": "off-topic: parking"}],
    "removeRanges": [{"startS": 0.0, "endS": 2.5, "reason": "dead head"}],
    "keepOnly": null
  },
  "cleanup": {
    "fillers": [],
    "pauses": {"minS": 1.0, "keepS": 0.15},
    "silences": [{"startS": 41.2, "endS": 43.9}],
    "untranscribed": []
  },
  "captions": {"maxChars": 32, "lines": 2, "burnIn": false},
  "grade": null,
  "audio": {"targetLufs": -16},
  "markers": [{"word": 165, "name": "Second topic"}, {"timeS": 90.0, "name": "Outro"}],
  "targetDurationS": 180,
  "export": null
}
```

| Field | Meaning |
|---|---|
| `source.sequence` | The sequence to edit (default: the active one). |
| `output.mode` | `newSequence` (default) or `inPlace`. `output.name` defaults to `<source> — <title>`. |
| `cuts.removeWords` | Inclusive sequence-transcript word indices, as `transcript.inspect` lists them; `to` defaults to `from`. Each needs a `reason`. |
| `cuts.removeRanges` | Sequence seconds, clamped to the sequence. Each needs a `reason`. |
| `cuts.keepOnly` | Word spans to keep; every run of words between them is removed with the pauses around it. |
| `cleanup.fillers` | Remove filler words: the listed words/phrases, or the default list (um, uh, erm, …) when empty; absent = keep fillers. |
| `cleanup.pauses` | Shorten pauses of at least `minS` (default 1.0), keeping `keepS` (default 0.15) next to each word. |
| `cleanup.silences` | Silences to remove (from `audio.detectSilence`; the caller passes them). |
| `cleanup.untranscribed` | Voiced sounds with no transcribed word to remove (opt-in). |
| `captions` | Add captions (`maxChars` 1–500, default 42; `lines` 1–4, default 2). |
| `markers` | Each has exactly one of `timeS` (source seconds) or `word`, and a `name`. |
| `targetDurationS` | A result more than 5 % off is a warning with the numbers. |

Not applied yet (reported in `skipped` and as warnings, never a failure): `output.aspect`,
`captions.burnIn` / `style` / `template`, `grade`, `audio`, and `export` (`plan.apply` never
exports; run `file.exportMedia` after approval).

## How cuts are compiled

1. Every number must be finite and every word index must exist; `validate` lists all problems.
   Limits: 10 000 cuts, 1 000 markers, 256 fillers, a 200-character title.
2. Word cuts go through `transcript::word_range` (frame-snapped), fillers through
   `find_fillers` + `filler_ranges`, pauses through `find_pauses`, `keepOnly` becomes the
   complement removals.
3. A `removeRanges` boundary that falls inside a word moves **outward** to the word's edge, so the
   partly covered word is cut whole (with a warning).
4. Silences, untranscribed sounds and every other cut never take a word that is kept: the kept
   words are carved out of them, so their boundaries land in the gaps between words (with a
   warning for silences and untranscribed sounds).
5. Boundaries are snapped to frames where that keeps every kept word whole.
6. Overlapping cuts are merged (reasons joined). A kept fragment shorter than 0.3 s between two
   cuts, with no kept word in it, is cut too (with a warning).

The result never puts a cut boundary strictly inside a kept word; a property test feeds the
compiler random hostile plans (NaN, infinities, out-of-range indices, overlapping spans) to check
that and that it never panics.
