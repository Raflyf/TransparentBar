# Verification — revised promotional films

All four revised exports completed with HyperFrames 0.8.185. FFprobe verified exactly 45 seconds, 2700 video frames at 60 fps, H.264, and stereo AAC at 48 kHz. FFmpeg full-file decode returned exit 0 for every film.

## Motion revision

The rejected version used long line rises, no content exit, and transitions outside the clips' active windows. Revised [timeline](<timeline.js>) uses short 14–48 px movement, explicit text/object exits, and 0.7-second overlap defined in both language templates. Clip-path interpolation was verified in a two-panel test before rendering. No full-screen blue transition plate, card wobble, or background blur remains. The Task Manager proof stays stationary so its highlight remains aligned.

## Visual and technical checks

- HyperFrames check: all four variants `ok: true`, browser ran, zero lint/runtime/layout/contrast errors. Each selected-time contrast pass checked 13 elements and passed 13. This is sampled coverage, not proof of every transition pixel.
- Eight scene frames extracted from each final MP4 and reviewed; separate landscape and portrait contact sheets include every scene. Dense transition frames inspected from the final landscape-ID and portrait-EN files, plus an 8-second encoded motion test.
- All audio tracks measured mean -16.1 dBFS and maximum -1.5 dBFS. No subjective listening claim is made; stream duration and signal levels were measured.
- Build self-check covers local asset existence, audio element IDs, output dimensions, 45-second coverage, and exact transition overlap.
- Remaining non-blocking warnings: nested scenes, repeated logo media, and one 4.25-second hold for reading the memory evidence. No warning-free claim. Dedicated automated motion scoring is disabled in this HyperFrames version; film judgment used rendered frame sequences instead.

## Evidence boundaries

The desktop capture and Task Manager figures are real source material. The 1.6 MB figure covers the host process in one snapshot, not the Explorer bridge or a benchmark. Startup menus are labeled illustrations; the English film explicitly notes the current application labels are Indonesian. No paid media generation or uploaded reference footage was used. Music and third-party rights are documented in [credits](<CREDITS.md>).

## Internal self-review

| Axis | Score | Evidence and remaining limitation |
| --- | --- | --- |
| Accuracy | 4/5 | Product claims source-checked; screenshot/wallpaper rights remain those of their respective owners. |
| Completeness | 4/5 | Four full exports and editable sources; aesthetic acceptance remains the owner's judgment. |
| Clarity | 4/5 | Large copy and visible caveats; app UI screenshots are necessarily small on a phone. |
| Actionability | 4/5 | Ready MP4s, credits, build script; reproducible rendering still requires Node, Chrome, FFmpeg, and HyperFrames. |
| Conciseness | 3/5 | Final asset set compact; production took avoidable repeated attempts before resolving clip timing. |

Overall: 3.8/5. Next production should prove one entrance, one exit, and an overlapping cut in motion before authoring the full timeline.
