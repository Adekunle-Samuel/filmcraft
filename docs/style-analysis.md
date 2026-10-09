# Style from a reference

FilmCraft can measure the "treatment" of a reference video (its pacing, cut rhythm, colour,
loudness, speech rhythm and format) so that an agent can describe it and reproduce it on other
footage.

## `media.analyze`: the StyleProfile

`media.analyze {item, maxFrames?=240 (1–600), wait?=false}` analyses a media item or subclip in a
background job (`jobs.list` shows its progress; `jobs.cancel` stops it and nothing is cached). It
decodes up to `maxFrames` frames, evenly spaced over the item, at about 256 px wide. When the
item is shorter than that, every frame is used. The result is cached on the session by item id
and returned by `media.analysis {item}`. It is never written to the project file.

The profile (`filmcraft_render::style::StyleProfile`, JSON in camelCase):

```jsonc
{
  "version": 1,
  "item": 12, "name": "reference.mp4",
  "analyzedFrames": 240,
  "sampleIntervalSeconds": 0.75,      // cut times are accurate to this
  "format": { "width": 1080, "height": 1920, "aspect": "9:16", "fps": 29.97,
              "durationSeconds": 180.0, "hasVideo": true, "hasAudio": true },
  "shots": { "count": 61, "medianSeconds": 2.3, "p10Seconds": 0.9, "p90Seconds": 5.6,
             "meanSeconds": 2.95, "cutsPerMinute": 20.0,
             "cutTimes": [1.5, …],      // seconds from the start of the analysed range
             "cutTicks": [381024000000, …] },  // the same cuts as exact media times
  "color": {                           // the whole piece, shots weighted by length
    "lumaP5": 0.06, "lumaP50": 0.42, "lumaP95": 0.93, "contrast": 0.87,
    "contrastLabel": "high",           // low < 0.5 ≤ medium ≤ 0.8 < high
    "saturation": 0.31, "chroma": 0.064,
    "saturationLabel": "natural",      // muted (chroma < 0.03) | natural | vivid (> 0.09)
    "warmth": 0.018, "tint": -0.004,   // midtone Oklab b and a
    "temperatureLabel": "warm",        // warm (> 0.012) | neutral | cool (< −0.012)
    "liftedBlacks": false,             // p5 > 0.1
    "crushedBlacks": false,            // p5 ≤ 0.02
    "crushedWhites": false,            // p95 < 0.8 (highlights held down)
    "clippedWhites": false,            // p95 ≥ 0.98
    "tones": { "shadows": {"share", "l", "a", "b"}, "midtones": {…}, "highlights": {…} }
  },
  "keyframes": [                       // one per shot, at most 24, spread over the shots
    { "shot": 0, "time": 190512000000, "seconds": 0.75,
      "color": { "lumaP5", "lumaP50", "lumaP95", "saturation", "chroma",
                 "cast": [a, b], "tones": {…} } }
  ],
  "audio": { "integratedLufs": -14.2, "loudnessRangeLu": 6.1, "truePeakDbtp": -1.0,
             "sampleRate": 48000, "channels": 2, "analyzedSeconds": 180.0 },
  "speech": { "words": 512, "wordsPerMinute": 170.7, "medianPauseSeconds": 0.12,
              "pauseShare": 0.08, "longPauses": 21, "fillers": 4, "fillersPerMinute": 1.3 },
  "notes": []                          // what could not be measured, and why
}
```

- **Shots.** Consecutive analysed frames are compared with the Scene Edit Detection measure
  (`filmcraft_render::scene`, sensitivity 50). With sparse sampling, a cut is reported at the
  first analysed frame of the new shot, and shots shorter than about ¼ s can be missed or merged.
- **Colour.** Each shot's middle analysed frame is measured on the SDR display signal. Luma
  percentiles use Rec. 709 weights on the sRGB-encoded values (0 black … 1 white). `saturation`
  is the mean HSV saturation. `chroma` and the `tones` are the Oklab statistics that Apply Match
  uses (`color_match::stats`): for the shadows, midtones and highlights, their share of the
  frame and their mean L, a and b. `cast` is the midtone (a, b): +a is red, −a green, +b yellow,
  −b blue.
- **Audio.** ITU-R BS.1770 / EBU R128 integrated loudness, loudness range and true peak of the
  item's own sound (up to 8 channels). `null` values mean silence.
- **Speech** needs a transcript of the item (`transcript.generate` or `transcript.set`). Rates
  are per minute of the analysed duration. `pauseShare` is the fraction of that duration spent in
  gaps of at least 0.5 s between words. Fillers are the default filler words
  (`filmcraft_edit::transcript::DEFAULT_FILLERS`).

## The style library

`style.save {name, item}` writes a cached profile to `<data dir>/styles/<name>.json` atomically.
`style.list` returns every saved style with its profile; unreadable files are listed under
`errors`. `style.delete {name}` removes one. Names are trimmed and must be 1–64 characters, with
no path separators, no control characters, none of `: * ? " < > |`, and no leading dot.

## Matching a grade to a reference: `lumetri.matchToItem`

`lumetri.matchToItem {clips?, item, samples?=6 (1–12), faceDetection?=true}` grades the clips
(default: the selected video clips) to look like a reference media item, subclip or sequence.
`samples` evenly spaced frames of the reference, and of each clip with its own Lumetri switched
off, are tiled into one mosaic each. The Apply Match solver (`filmcraft_render::color_match`)
then fits the three colour wheels, their lightness and Basic saturation so that the clip mosaic's
Oklab tonal statistics match the reference's. Every clip is written in one undo step. A clip
without Lumetri Color gets one in the same step. The result reports each clip's statistics
distance before and after.

## Baking a grade: `lumetri.bakeLut`

`lumetri.bakeLut {clip, size?=33 (17|33|65), name}` turns the clip's Lumetri Color grade into a
`size`³ 3D LUT, `<data dir>/luts/<name>.cube` (written atomically; the name follows the style
naming rules). The LUT is added to the project's LUT library as `lut.import` does, so `lut.list`
shows it and `lumetri.setLook` / `lumetri.setInputLut` apply it. The whole lattice is graded as
one image by the renderer's own Lumetri code (`filmcraft_render::lut_bake`), so at the lattice
points the LUT is exact. Between them it is tetrahedral interpolation.

- **Input:** SDR Rec. 709, display-referred: FilmCraft's sRGB-encoded grading signal, 0–1,
  which the Input LUT and Creative Look slots read. An HDR sequence gets a warning.
- **Baked:** Basic Correction (including its Input LUT), Creative (look or Look LUT, faded film,
  vibrance, split toning), Curves, Color Wheels and the HSL Secondary key and correction.
  Animated parameters are baked at the playhead.
- **Skipped, with a warning:** Vignette (depends on position), Creative Sharpen and the HSL
  Secondary Denoise / Blur refinement (both read neighbouring pixels), and masks. Only Lumetri
  Color instances are baked; other effects on the clip are ignored.

The file is the user's own output, not a repository asset.

## Honest limits

- Matching is a statistical Oklab match of tonal ranges, not shot-aware colour transfer: it
  doesn't pair a shot with the reference shot that shows the same content, and it doesn't
  segment skin, sky or other objects (skin protection is a hue-band weight).
- A LUT bake drops spatial effects (see above) and is SDR only.

- The statistics describe a frame's tonal distribution, not its content. Two very different
  pictures can share a profile.
- Speech statistics are only as good as the transcript.
