# Skill: style from a reference video

Use this when the user gives an example ("make it look like this", "same vibe as my last video",
"match this creator's style") and wants their footage treated the same way.

## Pipeline

1. **Import the reference** with `import_media` if it isn't in the project yet (it goes to the
   "References" bin). Never place it on the user's timeline.
2. **Analyse it** with `analyze_media` (a job). You get a StyleProfile: shot lengths and cuts per
   minute, colour statistics (contrast, saturation, warm/cool, lifted blacks), loudness, speech
   rate, pause and filler rates, aspect and frame rate.
3. **Look at it** with `contact_sheet` on the reference item. Describe what the numbers can't:
   caption look (position, size relative to the frame height, case, highlight colour, box or
   outline), framing (tight or wide, punch-ins on emphasis), on-screen text, B-roll density.
4. **Explain the style back** to the user in three or four lines before changing anything, e.g.
   "Fast: a cut every 2.1 s, pauses trimmed to under 0.2 s. Warm, punchy grade with lifted
   blacks. Big centred captions, two words at a time, yellow highlight. Vertical 9:16, loud
   (−11 LUFS)." Ask whether that is what they want if anything is ambiguous.
5. **Apply it**:
   - colour: `match_grade` with the reference item, then `bake_lut` if the user wants a reusable
     look (it lands in their LUT list);
   - pacing: run the talking-head cleanup with silence settings derived from the profile (median
     pause of the reference ≈ the `padSeconds` × 2 you keep; a fast reference means
     `minSeconds` 0.3);
   - captions: `add_captions` (or the plan's `captions`) with the style you described;
   - loudness: `set_loudness` to the reference's integrated loudness, kept between −24 and
     −9 LUFS;
   - aspect: the plan's `output.aspect` when the reference is vertical or square.
6. **Report** what was matched and what could not be (for example, the grade match is statistical:
   it matches overall tone and colour, not individual objects; LUTs can't carry vignettes or
   sharpening).

Save the profile with `style.save` when the user wants to reuse it ("remember this as my
podcast style").
