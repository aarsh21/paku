#!/usr/bin/env node
// Native keyboard hold against an opt-in fake-transcriber example: no microphone.
import {spawn,execFileSync} from 'node:child_process';
import assert from 'node:assert/strict';
import {mkdirSync,openSync,readFileSync,writeFileSync,existsSync} from 'node:fs';
import {resolve,join} from 'node:path';
const out=resolve(process.argv[2]||'/tmp/paku-dictation-native');
mkdirSync(out,{recursive:true});
const children=[];
const pause=ms=>new Promise(r=>setTimeout(r,ms));
async function wait(fn,label){for(let i=0;i<300;i++){const v=fn();if(v)return v;await pause(100);}throw Error(`Timeout: ${label}`);}
function start(command,args,env,name,extra=[]){const log=openSync(join(out,name),'a');const p=spawn(command,args,{env,stdio:['ignore',log,log,...extra]});children.push(p);return p;}
let env;
try{
 const displayFile=join(out,'display');
 start('Xvfb',['-displayfd','3','-screen','0','1280x800x24','-ac','-nolisten','tcp'],process.env,'xvfb.log',[openSync(displayFile,'w')]);
 const display=await wait(()=>{const n=readFileSync(displayFile,'utf8').trim();return /^\d+$/.test(n)?`:${n}`:null;},'private X display');
 const runtime=join(out,'runtime');mkdirSync(runtime,{mode:0o700,recursive:true});
 env={...process.env,DISPLAY:display,XDG_RUNTIME_DIR:runtime,WAYLAND_DISPLAY:'',RUST_LOG:'info,paku_ui::dictation=debug'};
 const wayland=process.env.PAKU_DICTATION_NATIVE_WAYLAND!=='0';
 const x=args=>execFileSync(process.env.XDOTOOL||'xdotool',args,{env,encoding:'utf8'}).trim();
 if(wayland){
  const moduleRoot=process.env.PAKU_WESTON_MODULE_ROOT;
  const westonEnv={...env};if(moduleRoot)westonEnv.WESTON_MODULE_MAP=`x11-backend.so=${moduleRoot}/x11-backend.so`;
  start('weston',['--backend=x11','--renderer=pixman',`--shell=${process.env.PAKU_WESTON_SHELL||'kiosk-shell.so'}`,'--socket=paku-dictation-test','--no-config','--width=1200','--height=760'],westonEnv,'weston.log');
  await wait(()=>existsSync(join(runtime,'paku-dictation-test')),'private Weston');
  env.WAYLAND_DISPLAY='paku-dictation-test';
 }
 const app=start(process.env.PAKU_DICTATION_FIXTURE_BINARY||'target/debug/examples/dictation-fixture',[out],env,'fixture.log');
 const window=await wait(()=>{try{return x(wayland?['search','--onlyvisible','--class','weston']:['search','--onlyvisible','--pid',`${app.pid}`]).split('\n')[0];}catch{return null;}},'fixture window');
 x(['windowfocus','--sync',window]);await pause(1800);
 execFileSync('magick',['import','-display',display,'-window',window,join(out,'01-before.png')]);
 x(['keydown','ctrl']);x(['keydown','d']);
 if(process.env.PAKU_DICTATION_REPLAY_RELEASE_PAIRS==='1'){
  // Reproduce the diagnostic trace on the PRIVATE input path: repeat begins
  // after 250 ms, then key-up/key-down pairs separated by a few ms at ~40 Hz.
  await pause(250);
  for(let i=0;i<60;i++){x(['keyup','d']);await pause(3);x(['keydown','d']);await pause(18);}
  await pause(100);
 }else await pause(2000);
 const events=()=>readFileSync(join(out,'events.jsonl'),'utf8').trim().split('\n').filter(Boolean).map(s=>JSON.parse(s));
 assert.equal(events().filter(e=>e.event==='start').length,1,'one capture starts while held');
 assert.equal(events().filter(e=>['finish','drop'].includes(e.event)).length,0,'capture survives native repeats and animation');
 execFileSync('magick',['import','-display',display,'-window',window,join(out,'02-key-held.png')]);
 x(['keyup','d']);x(['keyup','ctrl']);await pause(700);
 execFileSync('magick',['import','-display',display,'-window',window,join(out,'03-key-released.png')]);
 const result=events();
 for(const event of ['start','finish','final','drop'])assert.equal(result.filter(e=>e.event===event).length,1,`exactly one ${event}`);
 assert.ok(result.find(e=>e.event==='finish').capture_ms>=1900,'finish only after physical key release');
 writeFileSync(join(out,'probe.json'),JSON.stringify({wayland,window,releasePairs:process.env.PAKU_DICTATION_REPLAY_RELEASE_PAIRS==='1',passed:true,events:result},null,2));
 console.log(`Native dictation input evidence: ${out}`);
}finally{
 if(env){try{execFileSync(process.env.XDOTOOL||'xdotool',['keyup','d','ctrl'],{env,stdio:'ignore'});}catch{}}
 for(const p of children.reverse())if(p.exitCode===null)p.kill('SIGTERM');
}
