import {readFile,writeFile,mkdir,copyFile,readdir} from "node:fs/promises";
import path from "node:path";
import assert from "node:assert/strict";
import {fileURLToPath} from "node:url";
const source=path.dirname(fileURLToPath(import.meta.url));
const output=path.resolve(process.argv[2]||path.join(source,"projects"));
const timeline=await readFile(path.join(source,"timeline.js"),"utf8");
assert(timeline.includes('window.__timelines["main"]=tl'));
const names=await readdir(path.join(source,"assets"));
for(const language of ["id","en"]){
  const suffix=language==="en"?"-en":"";
  const template=await readFile(path.join(source,`composition${suffix}.html`),"utf8");
  const scenes=await readFile(path.join(source,`scenes${suffix}.html`),"utf8");
  const intervals=[...scenes.matchAll(/data-start="([0-9.]+)" data-duration="([0-9.]+)"/g)].map(m=>[Number(m[1]),Number(m[2])]);
  let end=6.15;for(const [start,duration] of intervals){assert(Math.abs(end-start-.7)<.001,"Each scene needs 0.7 seconds of transition overlap");assert(duration>0);end=start+duration;}assert.equal(end,45);
  for(const [orientation,width,height] of [["landscape",1920,1080],["portrait",1080,1920]]){
    const dir=path.join(output,`${language}-${orientation}`);await mkdir(path.join(dir,"assets"),{recursive:true});
    const html=template.replaceAll("__ORIENTATION__",orientation).replaceAll("__WIDTH__",String(width)).replaceAll("__HEIGHT__",String(height)).replace("__SCENES__",scenes).replace("__TIMELINE__",timeline);
    assert(!/__[A-Z]+__/.test(html));assert(html.includes(`data-width="${width}" data-height="${height}"`));
    const audio=[...html.matchAll(/<audio[^>]*>/g)];assert(audio.length>0);assert(audio.every(m=>/ id="[^"]+"/.test(m[0])),"Every audio clip needs an ID");
    await writeFile(path.join(dir,"index.html"),html);
    await copyFile(path.join(source,"style.css"),path.join(dir,"style.css"));
    for(const name of names){
      const from=path.join(source,"assets",name),to=path.join(dir,"assets",name);
      const existing=await readFile(to).catch(e=>{if(e.code==="ENOENT")return null;throw e});
      if(!existing||!existing.equals(await readFile(from)))await copyFile(from,to);
    }
    for(const match of html.matchAll(/(?:src|href)="([^"]+)"/g)){if(!match[1].startsWith("http"))await readFile(path.join(dir,match[1]));}
    console.log(`${language}-${orientation}: ${width}x${height}, 45 seconds; timeline and assets verified`);
  }
}
