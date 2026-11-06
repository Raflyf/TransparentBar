const portrait = document.documentElement.classList.contains("portrait");
const tl = gsap.timeline({ paused: true });
const open = "inset(0% 0% 0% 0%)";
const concealed = "inset(0% 0% 100% 0%)";
const cuts = [6, 14, 20, 28, 35, 40];
const ids = ["intro", "desktop", "native", "startup", "memory", "downloads", "close"];

function textIn(target, time, x = 0, y = 26, duration = .68, stagger = .075) {
  tl.set(target, { x, y, clipPath: concealed }, 0);
  tl.fromTo(target, { x, y, clipPath: concealed }, { x: 0, y: 0, clipPath: open, duration, stagger, ease: "power3.out", immediateRender: false }, time);
}
function objectIn(target, time, x = 0, y = 34, scale = .976) {
  tl.set(target, { x, y, scale, clipPath: "inset(0% 100% 0% 0%)" }, 0);
  tl.fromTo(target, { x, y, scale, clipPath: "inset(0% 100% 0% 0%)" }, { x: 0, y: 0, scale: 1, clipPath: open, duration: .82, ease: "power3.out", immediateRender: false }, time);
}
function textOut(target, time, x = -18, y = -14) {
  tl.to(target, { x, y, clipPath: "inset(100% 0% 0% 0%)", duration: .38, stagger: .035, ease: "power2.in" }, time);
}
function objectOut(target, time, x = -30, y = 0) {
  tl.to(target, { x, y, scale: .988, clipPath: "inset(0% 0% 0% 100%)", duration: .55, ease: "power2.inOut" }, time);
}

tl.to({}, { duration: 45 }, 0);
ids.forEach((id, i) => {
  tl.set("#" + id, { zIndex: i + 1, clipPath: i === 0 ? open : "inset(0% 100% 0% 0%)" }, 0);
  if (i > 0) {
    // Clip windows overlap by 0.7 s; GSAP never animates a not-yet-mounted scene.
    tl.fromTo("#" + id, { clipPath: "inset(0% 100% 0% 0%)" }, { clipPath: open, duration: .65, ease: "power3.inOut", immediateRender: false }, cuts[i - 1] - .55);
  }
});

textIn("#intro .brand", .08, -14, 0, .55);
textIn("#intro .mask>span", .16, 0, 32, .78, .085);
objectIn("#intro .hero-mark", .24, 48, 0, .97);
textIn("#intro .intro-note, #intro .intro-foot", .66, 0, 18, .62, .11);
tl.set("#intro .mark-blue", { x: 290 }, 0);
tl.to("#intro .mark-blue", { x: -60, duration: 1.1, ease: "power3.inOut" }, 1.2);
tl.to("#intro .mark-blue", { x: -180, duration: .9, ease: "power3.inOut" }, 3.65);
textOut("#intro .mask>span, #intro .intro-note, #intro .intro-foot", 5.12);
objectOut("#intro .hero-mark", 5.38, -42);
textOut("#intro .brand", 5.4, 0, -12);

textIn("#desktop .desktop-title .mask>span", 5.73, 24, 0, .68);
objectIn("#desktop .desktop-window", 5.92, 45, 0, .98);
textIn("#desktop .desktop-copy, #desktop .desktop-caption", 6.24, 0, 18, .58, .1);
tl.to("#desktop .desktop-window", { scale: 1.012, duration: 2.5, ease: "sine.inOut" }, 7.2);
textOut("#desktop .desktop-title .mask>span, #desktop .desktop-copy, #desktop .desktop-caption", 9.75);
tl.set("#desktop .desktop-detail", { visibility: "hidden", clipPath: "inset(42% 100% 24% 0%)" }, 0);
tl.set("#desktop .desktop-detail", { visibility: "visible" }, 10.02);
tl.to("#desktop .desktop-detail", { clipPath: open, duration: .72, ease: "power3.inOut" }, 10.02);
textIn("#desktop .desktop-detail .mask>span", 10.29, 26, 0, .63);
objectIn("#desktop .detail-apps", 10.28, 36, 0, .988);
objectIn("#desktop .detail-tray", 10.48, 32, 0, .988);
textIn("#desktop .desktop-detail>p", 10.74, 0, 18, .58);
textOut("#desktop .desktop-detail .mask>span, #desktop .desktop-detail>p", 13.16);
objectOut("#desktop .detail-apps, #desktop .detail-tray", 13.36, -34);

textIn("#native .mask>span", 13.75, 0, 30, .72);
objectIn("#native .native-strip", 13.85, 0, 0, 1);
textIn("#native .native-statements p", 14.55, 30, 0, .65, .38);
textIn("#native .native-note", 15.68, 0, 18, .62);
textOut("#native .native-note, #native .native-statements p", 18.94, -24, 0);
textOut("#native .mask>span", 19.23);
objectOut("#native .native-strip", 19.4, -28);

textIn("#startup .mask>span", 19.76, 0, 28, .7);
textIn("#startup .startup-copy", 20.02, 0, 18, .58);
objectIn("#startup .tray-demo", 20.02, 28, 12, .98);
textIn("#startup .startup-state, #startup .startup-note", 20.8, 0, 16, .57, .08);
tl.set("#startup .menu-check img", { yPercent: 110 }, 0);
tl.set("#startup .demo-cursor", { x: 80, y: 40 }, 0);
tl.to("#startup .demo-cursor", { x: portrait ? -230 : -70, y: portrait ? -175 : -222, duration: .72, ease: "power3.inOut" }, 21.62);
tl.to("#startup .demo-cursor", { scale: .9, duration: .075, repeat: 1, yoyo: true }, 22.32);
tl.to("#startup .menu-check img", { yPercent: 0, duration: .24, ease: "power3.out" }, 22.38);
tl.to("#startup .state-off, #startup .state-on", { y: -100, duration: .32, ease: "power3.inOut" }, 22.38);
tl.to("#startup .demo-cursor", { scale: .9, duration: .075, repeat: 1, yoyo: true }, 25.04);
tl.to("#startup .menu-check img", { yPercent: 110, duration: .22, ease: "power2.in" }, 25.1);
tl.to("#startup .state-off, #startup .state-on", { y: 0, duration: .32, ease: "power3.inOut" }, 25.1);
textOut("#startup .mask>span, #startup .startup-copy, #startup .startup-state, #startup .startup-note", 27.12);
objectOut("#startup .tray-demo", 27.37, -28);

textIn("#memory .mask>span", 27.76, 0, 28, .68);
textIn("#memory .memory-figure", 28.03, 24, 0, .83);
objectIn("#memory .task-proof", 27.99, 34, 0, .987);
textIn("#memory .memory-label, #memory .memory-note", 28.48, 0, 18, .58, .11);
tl.set("#memory .task-highlight", { scaleX: 0 }, 0);
tl.to("#memory .task-highlight", { scaleX: 1, duration: .65, ease: "power3.inOut" }, 29.02);
textOut("#memory .mask>span, #memory .memory-figure, #memory .memory-label, #memory .memory-note", 34.06);
objectOut("#memory .task-proof", 34.35, -30);

textIn("#downloads .mask>span", 34.77, 26, 0, .66);
objectIn("#downloads .install-file", 34.92, 0, 24, 1);
objectIn("#downloads .portable-file", 35.15, 38, 0, .98);
textIn("#downloads .download-platform", 35.55, 0, 18, .6);
textOut("#downloads .mask>span, #downloads .download-platform", 39.12);
objectOut("#downloads .install-file, #downloads .portable-file", 39.28, -32);

objectIn("#close .close-mark", 39.7, 24, 0, .94);
textIn("#close .mask>span", 39.89, 0, 26, .72);
textIn("#close .close-promise", 40.13, 0, 18, .6);
objectIn("#close .close-cta", 40.38, 24, 0, 1);
textIn("#close .close-url, #close .close-license, #close .music-credit", 40.77, 0, 16, .6, .06);
window.__timelines = window.__timelines || {};
window.__timelines["main"]=tl;
tl.seek(0);
