# TransparentBar promotional films

Four separate 45-second films: Indonesian and English, each in landscape 1920×1080 and portrait 1080×1920. 60 fps H.264 video with stereo 48 kHz AAC audio. No narrator. Portrait is an authored layout, not a crop of landscape.

## Files

- `exports/revised/TransparentBar-Landscape-ID.mp4`
- `exports/revised/TransparentBar-Portrait-ID.mp4`
- `exports/revised/TransparentBar-Landscape-EN.mp4`
- `exports/revised/TransparentBar-Portrait-EN.mp4`

Exports are delivered separately and excluded from Git. Editable HTML, CSS, GSAP timeline, local fonts, icon components, screenshot assets, and soundtrack are included in the source ZIP.

## Rebuild and preview

Requires Node.js, FFmpeg, and the Chrome browser used by HyperFrames. Rendered with HyperFrames 0.8.185 and GSAP 3.14.2. No cloud rendering, accounts, paid media generation, or external asset requests during capture.

```sh
node build.mjs
npx --yes hyperframes@0.8.185 preview projects/en-landscape
npx --yes hyperframes@0.8.185 check projects/en-landscape
npx --yes hyperframes@0.8.185 render projects/en-landscape -f 60 -q delivery -w 2 -o exports/revised/TransparentBar-Landscape-EN.mp4
```

Repeat with `id-landscape`, `id-portrait`, and `en-portrait`. Edit Indonesian copy in `composition.html` / `scenes.html`, English in `composition-en.html` / `scenes-en.html`, layout in `style.css`, and motion in `timeline.js`. `node build.mjs` asserts 45-second scene coverage with 0.7-second overlaps, audio IDs, dimensions, and local asset existence.

## Story

| Seconds | Content |
| --- | --- |
| 0–6 | Animated product mark and wallpaper promise |
| 6–14 | Real desktop capture and enlarged taskbar details |
| 14–20 | Native Rust, no webview, account, or telemetry |
| 20–28 | Optional startup, off by default; labeled tray illustration |
| 28–35 | Qualified Task Manager memory snapshot |
| 35–40 | Installer and portable formats |
| 40–45 | Repository CTA, product name, music attribution |

## Publishing and attribution

Keep the music credit when publishing. Scott Buckley requires attribution in YouTube video descriptions, even though it also appears in the film. Copy the text from [CREDITS.md](CREDITS.md). The MIT product license does not override third-party font, icon, music, GSAP, screenshot, or trademark rights.

Desktop and Task Manager imagery are real user-provided captures; camera movement, cropping, and framing are presentation. Startup menu is an illustration, not live screen recording. English version translates this illustration and explicitly notes current application labels are Indonesian.

1.6 MB is the displayed host-process value in one capture, not a benchmark or upper limit. Explorer bridge memory is excluded. Screen text keeps this qualification. No claims of universal future Windows support, trusted signing, or guaranteed zero resource use.
