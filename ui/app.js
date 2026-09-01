const invoke = window.__TAURI__.core.invoke;
const $ = id => document.getElementById(id);
const PREVIEW_MAX_SIDE = 2160;
let runtimeReady = Promise.resolve();
const state = { path:null, sourcePath:null, kind:null, info:null, sourceData:null, originalUrl:null, processedUrl:null, loadedFrame:-1, zoom:1, fit:1, panX:0, panY:0, splitX:null, dragging:null, request:0, busy:false };
const stage = $('stage'), preview = $('preview'), originalPreview = $('original-preview'), originalMask = $('original-mask'), abView = $('ab-view');
const abPanes = Array.from(abView.querySelectorAll('.ab-pane')), abOriginal = $('ab-original'), abProcessed = $('ab-processed');
const settings = () => ({ style:+$('style').value, intensity:+$('intensity').value, localTone:+$('tone').value, localStruct:+$('struct').value, skinStructure:+$('skin').value, useAutoMask:$('auto-mask').checked, uiCorrection:$('ui-correction').checked, outputView:0, outputMix:1, upscale:$('upscale').value, vsrQuality:+$('vsr-quality').value, encoder:$('encoder').value, encoderQuality:+$('encoder-quality').value, keepAudio:$('keep-audio').checked });
const upscaleArgs = () => ({ upscale:$('upscale').value, vsrQuality:+$('vsr-quality').value });
function log(message) { console.debug(`[DLSS5] ${message}`); }
function status(message) { $('status').textContent = message; }
async function invokePng(cmd, args) { const res = await invoke(cmd, args); const bytes = res instanceof Uint8Array ? res : new Uint8Array(res); return URL.createObjectURL(new Blob([bytes], { type:'image/png' })); }
async function urlToDataUri(url) { const blob = await (await fetch(url)).blob(); return new Promise(resolve => { const reader = new FileReader(); reader.onload = () => resolve(reader.result); reader.readAsDataURL(blob); }); }
// 延迟回收：导出/复制会异步加载正在显示的 URL，立即回收会中断在途加载
function deferRevoke(url) { if(url) setTimeout(() => URL.revokeObjectURL(url), 5000); }
function revokeMedia() { deferRevoke(state.originalUrl); deferRevoke(state.processedUrl); state.originalUrl=null; state.processedUrl=null; }
function syncControls(range, number) { $(range).oninput = () => { $(number).value = $(range).value; refresh(); }; $(number).onchange = () => { $(range).value = Math.max(0, Math.min(1, +$(number).value || 0)); refresh(); }; }
syncControls('intensity','intensity-num'); syncControls('tone','tone-num'); syncControls('struct','struct-num'); syncControls('skin','skin-num');
$('auto-mask').onchange=()=>refresh(); $('ui-correction').onchange=()=>refresh();
// 输出尺寸：自定义长宽 + X1/X2/X4 快速倍率；32..8192 且取偶（视频编码要求）
// 放大功能整体依赖 RTX VSR：关闭或不可用时输出不能超过原始分辨率
const OUTPUT_MIN_SIDE=32, OUTPUT_MAX_SIDE=8192;
function vsrEnabled(){ const option=$('upscale').querySelector('option[value=vsr]'); return $('upscale').value==='vsr'&&option&&!option.disabled; }
function outputCap(){ if(vsrEnabled())return{w:OUTPUT_MAX_SIDE,h:OUTPUT_MAX_SIDE}; const s=sourceSize(); return s?{w:s[0],h:s[1]}:{w:OUTPUT_MAX_SIDE,h:OUTPUT_MAX_SIDE}; }
function fitOutput(w,h) {
  let x=Math.round(+w), y=Math.round(+h);
  if(!(x>0)||!(y>0))return null;
  const cap=outputCap();
  let s=Math.min(1,cap.w/x,cap.h/y);
  s=Math.max(s,OUTPUT_MIN_SIDE/x,OUTPUT_MIN_SIDE/y);
  x=Math.round(Math.min(cap.w,Math.max(OUTPUT_MIN_SIDE,x*s)));
  y=Math.round(Math.min(cap.h,Math.max(OUTPUT_MIN_SIDE,y*s)));
  return [x-(x%2),y-(y%2)];
}
function outputSize() { if(!state.path)return null; return fitOutput($('out-width').value,$('out-height').value); }
function outputArgs() { const s=outputSize(); return {outputWidth:s?s[0]:null,outputHeight:s?s[1]:null}; }
function sourceSize() { const w=state.info?.width||originalPreview.naturalWidth||preview.naturalWidth, h=state.info?.height||originalPreview.naturalHeight||preview.naturalHeight; return w&&h?[w,h]:null; }
function markRatio() { const base=sourceSize(),w=+$('out-width').value,h=+$('out-height').value; [['ratio-1',1],['ratio-2',2],['ratio-4',4]].forEach(([id,k])=>{ const t=base?fitOutput(base[0]*k,base[1]*k):null; $(id).classList.toggle('active',!!t&&t[0]===w&&t[1]===h); }); }
function syncOutputInputs() { const s=outputSize(); if(s){$('out-width').value=s[0];$('out-height').value=s[1];} markRatio(); }
function updateSizeNote() { const s=sourceSize(), el=$('size-note'); if(el) el.textContent=s?`原始 ${s[0]}×${s[1]}`:''; }
function setRatio(k) { if(!vsrEnabled())return; const base=sourceSize(); if(!base)return; const t=fitOutput(base[0]*k,base[1]*k); if(!t)return; $('out-width').value=t[0]; $('out-height').value=t[1]; markRatio(); refresh(true); }
$('ratio-1').onclick=()=>setRatio(1); $('ratio-2').onclick=()=>setRatio(2); $('ratio-4').onclick=()=>setRatio(4);
$('out-width').onchange=$('out-height').onchange=()=>{syncOutputInputs();refresh(true);};
function updateUpscaleAvailability(){
  const on=vsrEnabled(), cap=outputCap();
  $('ratio-2').disabled=!on; $('ratio-4').disabled=!on;
  $('ratio-2').title=on?'按原始尺寸的 2 倍输出':'放大功能需要 RTX VSR';
  $('ratio-4').title=on?'按原始尺寸的 4 倍输出':'放大功能需要 RTX VSR';
  $('out-width').max=cap.w; $('out-height').max=cap.h;
}
$('upscale').onchange=()=>{updateUpscaleAvailability();syncOutputInputs();updateSizeNote();refresh(true);};
$('vsr-quality').onchange=()=>refresh();
document.querySelectorAll('.tabs .tab').forEach(button=>button.onclick=()=>{
  document.querySelectorAll('.tabs .tab').forEach(item=>item.classList.toggle('active',item===button));
  document.querySelectorAll('.tab-page').forEach(page=>page.hidden=page.id!=='tab-'+button.dataset.tab);
});
invoke('vsr_probe').then(available=>{if(!available)markVsrUnavailable();}).catch(()=>markVsrUnavailable());
function markVsrUnavailable(){
  const option=$('upscale').querySelector('option[value=vsr]');
  option.disabled=true;option.textContent='RTX VSR（不可用）';
  if($('upscale').value==='vsr')$('upscale').value='none';
  updateUpscaleAvailability();syncOutputInputs();updateSizeNote();refresh(true);
}
updateUpscaleAvailability();
function syncAbLayout() {
  const original = abOriginal, processed = abProcessed;
  const w = original.naturalWidth || processed.naturalWidth || 1;
  const h = original.naturalHeight || processed.naturalHeight || 1;
  [original, processed].forEach(image => {
    image.style.width = `${w}px`;
    image.style.height = `${h}px`;
  });
  return [w, h];
}
function syncCompareLayout() {
  const w = preview.naturalWidth || originalPreview.naturalWidth || 1;
  const h = preview.naturalHeight || originalPreview.naturalHeight || 1;
  [preview, originalPreview].forEach(image => {
    image.style.width = `${w}px`;
    image.style.height = `${h}px`;
  });
  return [w, h];
}
function displayedSize() { if ($('view').value === 'AB 视图') return syncAbLayout(); if ($('view').value === '对比') return syncCompareLayout(); const w = preview.naturalWidth || 1, h = preview.naturalHeight || 1; return [w,h]; }
function activeViewport() { return $('view').value === 'AB 视图' ? abPanes[0] : stage; }
function resetFit() { const [w,h] = displayedSize(), viewport = activeViewport(); const width = viewport.clientWidth || stage.clientWidth, height = viewport.clientHeight || stage.clientHeight; state.fit = Math.min(width / w, height / h, 1); state.zoom = state.fit; state.panX = (width - w * state.zoom) / 2; state.panY = (height - h * state.zoom) / 2; transform(); }
function transform() { const matrix = `translate(${state.panX}px,${state.panY}px) scale(${state.zoom})`; preview.style.transform = matrix; originalPreview.style.transform = matrix; abOriginal.style.transform = matrix; abProcessed.style.transform = matrix; abView.style.transform = 'none'; $('zoom').textContent = `${Math.round(state.zoom * 100)}% · 点击${Math.abs(state.zoom - 1) < .01 ? '适合窗口' : '100%'}`; }
function updateSplit() { const x = Math.max(0, Math.min(stage.clientWidth, state.splitX ?? stage.clientWidth / 2)); $('split-line').style.left = `${x}px`; originalMask.style.width = `${x}px`; }
function fitWhenReady(image) {
  const fit = () => requestAnimationFrame(() => { if ($('view').value === 'AB 视图') syncAbLayout(); else if ($('view').value === '对比') syncCompareLayout(); resetFit(); });
  if (image.complete && image.naturalWidth) fit();
  else image.addEventListener('load', fit, {once:true});
}
function chooseDisplayed(fit = false) {
  if (!state.originalUrl) return;
  const view = $('view').value, output = state.processedUrl || state.originalUrl;
  preview.style.display = 'none'; preview.style.width = ''; preview.style.height = ''; originalMask.style.display = 'none'; originalPreview.style.display = 'none'; originalPreview.style.width = ''; originalPreview.style.height = ''; abView.style.display = 'none'; $('split-line').style.display = 'none'; $('compare-left').style.display = 'none'; $('compare-right').style.display = 'none'; $('ab-option').style.display = view === 'AB 视图' ? 'inline-flex' : 'none';
  if (view === '原图') { preview.src = state.originalUrl; preview.style.display = 'block'; }
  else if (view === 'DLSS') { preview.src = output; preview.style.display = 'block'; }
  else if (view === '对比') { preview.src = output; originalPreview.src = state.originalUrl; syncCompareLayout(); preview.style.display = 'block'; originalMask.style.display = 'block'; originalPreview.style.display = 'block'; $('split-line').style.display = 'block'; $('compare-left').style.display = 'block'; $('compare-right').style.display = 'block'; updateSplit(); }
  else { abOriginal.src = state.originalUrl; abProcessed.src = output; abView.className = $('ab-layout').value; syncAbLayout(); abView.style.display = 'flex'; }
  $('empty').style.display = 'none'; if (fit) fitWhenReady(view === 'AB 视图' ? $('ab-original') : preview); else transform();
}
function loadDataImage(data) { return new Promise((resolve,reject) => { const image=new Image(); image.onload=()=>resolve(image); image.onerror=reject; image.src=data; }); }
async function currentImageData() {
  const view = $('view').value; if (view === '原图' || !state.processedUrl) return urlToDataUri(state.originalUrl); if (view === 'DLSS') return urlToDataUri(state.processedUrl);
  const original = await loadDataImage(state.originalUrl), processed = await loadDataImage(state.processedUrl), ab = view === 'AB 视图', vertical = ab && $('ab-layout').value === 'vertical';
  const w=original.naturalWidth, h=original.naturalHeight; const canvas=document.createElement('canvas'); canvas.width=ab&&!vertical ? w*2 : w; canvas.height=ab&&vertical ? h*2 : h; const c=canvas.getContext('2d'); c.drawImage(original,0,0,w,h);
  if(ab) c.drawImage(processed,vertical?0:w,vertical?h:0,w,h); else { const split=Math.max(0,Math.min(w,((state.splitX ?? stage.clientWidth/2)-state.panX)/state.zoom)); c.save();c.beginPath();c.rect(split,0,w-split,h);c.clip();c.drawImage(processed,0,0,w,h);c.restore(); }
  return canvas.toDataURL('image/png');
}
function scaleClipboard(data) { return loadDataImage(data).then(image => { const max=Math.max(image.naturalWidth,image.naturalHeight); if(max<=PREVIEW_MAX_SIDE) return data; const s=PREVIEW_MAX_SIDE/max, canvas=document.createElement('canvas'); canvas.width=Math.round(image.naturalWidth*s); canvas.height=Math.round(image.naturalHeight*s); canvas.getContext('2d').drawImage(image,0,0,canvas.width,canvas.height); return canvas.toDataURL('image/png'); }); }
let loadingPath='', loadingStarted=0;
async function initializeRuntime() {
  const select=$('runtime'), gpuName=$('gpu-name'); select.disabled=true; status('正在检测显卡…');
  try {
    const gpu=await invoke('gpu_info');
    gpuName.textContent=gpu.name;
    gpuName.title=gpu.name;
    gpuName.classList.toggle('unavailable',!gpu.detected);
    if(['30','40','50'].includes(gpu.runtime)) select.value=gpu.runtime;
    select.title=gpu.detected?`已识别：${gpu.name}`:'未识别到支持的 RTX 显卡，当前使用默认 RTX 50';
    if(gpu.detected){log(`已识别 ${gpu.name}，自动选择 RTX ${gpu.runtime}`);status(`已自动选择 RTX ${gpu.runtime}`);}
    else {log(`显卡识别失败：${gpu.name}`);status('显卡未识别，默认使用 RTX 50');}
  } catch(e) {
    gpuName.textContent='未识别到显卡';
    gpuName.title='显卡识别失败';
    gpuName.classList.add('unavailable');
    log(`显卡识别: ${e}`); select.title='显卡识别失败，当前使用默认 RTX 50'; status('显卡识别失败，默认使用 RTX 50');
  } finally { select.disabled=false; }
}
async function loadPath(path) {
  await runtimeReady;
  stopPlayback();
  status('正在读取素材…');
  const now=Date.now();
  if(path===loadingPath&&now-loadingStarted<500)return;
  loadingPath=path;loadingStarted=now;
  clearTimeout(refreshTimer);
  state.request++;
  refreshQueued=false;
  refreshQueuedFit=false;
  const info=await invoke('media_info',{path}); revokeMedia(); Object.assign(state,{path:info.path,sourcePath:info.sourcePath,kind:info.kind,info,sourceData:null,splitX:null,loadedFrame:-1});
  const initial=fitOutput(info.width,info.height)||[info.width,info.height];
  $('out-width').value=initial[0]; $('out-height').value=initial[1]; markRatio(); updateSizeNote();
  if(info.kind==='video') {
    status('正在生成首帧预览…');
    state.originalUrl=await invokePng('frame_png',{path:info.path,frame:0,maxSide:PREVIEW_MAX_SIDE,...outputArgs(),...upscaleArgs()});
    state.loadedFrame=0;
  } else {
    state.originalUrl=await invokePng('read_image_data',{path:info.path,maxSide:PREVIEW_MAX_SIDE,...outputArgs()});
  }
  $('source').textContent=`${path.split(/[\\/]/).pop()} · ${info.kind==='video'?'视频':'图片'} · ${info.width}×${info.height}`; $('frame').max=Math.max(0,info.frames-1); $('frame').value=0; $('frame-label').textContent=`帧 0 / ${Math.max(0,info.frames-1)}`; $('export-full').textContent=info.kind==='video'?'导出 DLSS 视频':'导出 DLSS 图片';
  chooseDisplayed(true);
  status('正在生成 DLSS 预览…'); await refresh(true,0);
}
function normalizeDroppedPath(value) { if(typeof value!=='string') return ''; const raw=value.trim(); if(!raw) return ''; if(!raw.toLowerCase().startsWith('file://')) return raw; try { const url=new URL(raw); let path=decodeURIComponent(url.pathname); if(/^\/[A-Za-z]:/.test(path)) path=path.slice(1); if(url.hostname&&url.hostname!=='localhost') path=`\\\\${url.hostname}${path}`; return path; } catch(_) { return raw; } }
function droppedPaths(payload) { const value=payload?.paths??payload; return Array.isArray(value)?value:(value?[value]:[]); }
let lastDropPath='', lastDropAt=0;
async function loadDroppedPath(value) { const path=normalizeDroppedPath(value); if(!path){status('无法读取拖入素材');return;} const now=Date.now(); if(path===lastDropPath&&now-lastDropAt<500)return; lastDropPath=path;lastDropAt=now; try { status('正在读取拖入素材…'); await loadPath(path); } catch(e) { log(`拖放: ${e}`); status('拖放失败'); } }
function setDropActive(active) { stage.classList.toggle('drop-active',active); }
let dropPollBusy=false, dropPollFailureLogged=false;
async function pollNativeDrop() {
  if(dropPollBusy)return;
  dropPollBusy=true;
  try {
    const result=await invoke('poll_drop');
    setDropActive(Boolean(result.active));
    const paths=droppedPaths(result.paths);
    if(paths[0])await loadDroppedPath(paths[0]);
  } catch(e) {
    if(!dropPollFailureLogged){log(`拖放监听: ${e}`);dropPollFailureLogged=true;}
  } finally { dropPollBusy=false; }
}
setInterval(pollNativeDrop,120);
pollNativeDrop();
let exportBusy=false, exportPollFailureLogged=false;
const exportButtons=[$('export-current'),$('export-full')];
function renderExportProgress(progress) {
  const wrap=$('export-progress-wrap'), bar=$('export-progress'), label=$('export-progress-label');
  const total=Math.max(0,Number(progress?.total)||0), current=Math.max(0,Number(progress?.current)||0);
  if(!progress?.active&&!progress?.message&&current===0){wrap.hidden=true;return;}
  wrap.hidden=false;
  if(total>0){bar.max=total;bar.value=Math.min(current,total);}else{bar.removeAttribute('value');}
  const percent=total>0?` ${Math.round(Math.min(1,current/total)*100)}%`:'';
  label.textContent=`${progress.message||'正在导出…'}${percent}`;
}
function showExportProgress(current,total,message) { renderExportProgress({active:true,current,total,message}); }
async function pollExportProgress() {
  try {
    renderExportProgress(await invoke('poll_export_progress'));
  } catch(e) {
    if(!exportPollFailureLogged){log(`导出进度: ${e}`);exportPollFailureLogged=true;}
  }
}
setInterval(pollExportProgress,120);
pollExportProgress();
function setExportBusy(busy) { exportBusy=busy; exportButtons.forEach(button=>button.disabled=busy); }
async function runExport(total, task) {
  setExportBusy(true);
  showExportProgress(0,total,'准备导出…');
  try {
    const result=await task();
    showExportProgress(total,total,'导出完成');
    return result;
  } catch(e) {
    showExportProgress(0,total,'导出失败');
    throw e;
  } finally { setExportBusy(false); }
}
let refreshTimer, playHandle=null, playStartedAt=0, playStartedFrame=0;
let refreshQueued=false, refreshQueuedFit=false;
function refresh(fit=false, delay=90) {
  clearTimeout(refreshTimer);
  const request=++state.request;
  refreshQueued=true;
  refreshQueuedFit=refreshQueuedFit||fit;
  return new Promise(resolve => refreshTimer=setTimeout(async()=>{
    await runtimeReady;
    refreshQueued=false;
    if(!state.path){refreshQueuedFit=false;resolve();return;}
    if(state.busy){refreshQueued=true;resolve();return;}
    state.busy=true;
    const renderPath=state.path;
    const renderKind=state.kind;
    const renderFrame=+$('frame').value;
    const renderFit=refreshQueuedFit;
    refreshQueuedFit=false;
    try {
      status('正在刷新轻量预览…');
      let processedUrl, originalUrl;
      if(renderKind==='video') {
        processedUrl=await invokePng('render_frame_png',{path:renderPath,frame:renderFrame,runtime:$('runtime').value,settings:settings(),maxSide:PREVIEW_MAX_SIDE,...outputArgs()});
        if(renderFrame!==state.loadedFrame) originalUrl=await invokePng('frame_png',{path:renderPath,frame:renderFrame,maxSide:PREVIEW_MAX_SIDE,...outputArgs()});
      } else if(renderKind==='clipboard') {
        processedUrl=await invokePng('process_image_data',{data:state.sourceData,runtime:$('runtime').value,settings:settings(),maxSide:PREVIEW_MAX_SIDE,...outputArgs()});
      } else {
        processedUrl=await invokePng('process_image',{path:renderPath,runtime:$('runtime').value,settings:settings(),maxSide:PREVIEW_MAX_SIDE,...outputArgs()});
      }
      if(request===state.request&&renderPath===state.path) {
        if(originalUrl){ deferRevoke(state.originalUrl); state.originalUrl=originalUrl; state.loadedFrame=renderFrame; }
        deferRevoke(state.processedUrl);
        state.processedUrl=processedUrl;
        chooseDisplayed(renderFit);
        status('就绪');
      } else {
        URL.revokeObjectURL(processedUrl);
        if(originalUrl)URL.revokeObjectURL(originalUrl);
      }
    } catch(e) {
      if(request===state.request&&renderPath===state.path){log(`DLSS: ${e}`);status('预览失败');}
    } finally {
      state.busy=false;
      const rerender=refreshQueued;
      const nextFit=refreshQueuedFit;
      refreshQueued=false;
      refreshQueuedFit=false;
      if(rerender)refresh(nextFit,0);
      resolve();
    }
  },delay));
}
$('open').onclick=async()=>{try{const path=await invoke('choose_media');if(path)await loadPath(path);}catch(e){log(String(e));status('导入失败');}};
$('paste').onclick=async()=>{try{const item=(await navigator.clipboard.read()).find(i=>i.types.some(t=>t.startsWith('image/')));if(!item)throw Error('剪贴板中没有图片');const type=item.types.find(t=>t.startsWith('image/'));const blob=await item.getType(type),reader=new FileReader();reader.onload=async()=>{const scaled=await scaleClipboard(reader.result);const pasted=await loadDataImage(scaled);revokeMedia();Object.assign(state,{path:'剪贴板图片',kind:'clipboard',info:{kind:'image',frames:1,width:pasted.naturalWidth,height:pasted.naturalHeight},sourceData:scaled,originalUrl:URL.createObjectURL(await (await fetch(scaled)).blob()),splitX:null,loadedFrame:-1});$('out-width').value=pasted.naturalWidth;$('out-height').value=pasted.naturalHeight;markRatio();updateSizeNote();$('source').textContent='已粘贴图片（轻量预览）';await refresh(true);};reader.readAsDataURL(blob);}catch(e){log(`粘贴: ${e}`);status('粘贴失败');}};
$('view').onchange=()=>chooseDisplayed(true); $('ab-layout').onchange=()=>chooseDisplayed(true); $('style').onchange=()=>refresh(); $('runtime').onchange=()=>{log('运行时变更需重启应用后生效。');refresh();};
  $('zoom').onclick=()=>{if(!state.path)return;if(Math.abs(state.zoom-1)<.01)resetFit();else{const [w,h]=displayedSize(),viewport=activeViewport();state.zoom=1;state.panX=(viewport.clientWidth-w)/2;state.panY=(viewport.clientHeight-h)/2;transform();}};
 function stagePoint(event) { const rect=stage.getBoundingClientRect(); return {x:Math.max(0,Math.min(stage.clientWidth,event.clientX-rect.left)),y:Math.max(0,Math.min(stage.clientHeight,event.clientY-rect.top))}; }
 function abAnchor(event) { const pane=abPanes.find(item=>{const rect=item.getBoundingClientRect();return event.clientX>=rect.left&&event.clientX<=rect.right&&event.clientY>=rect.top&&event.clientY<=rect.bottom;})||abPanes[0]; const rect=pane.getBoundingClientRect(); const x=rect.width?Math.max(0,Math.min(1,(event.clientX-rect.left)/rect.width)):0.5; const y=rect.height?Math.max(0,Math.min(1,(event.clientY-rect.top)/rect.height)):0.5; return {x:x*abPanes[0].clientWidth,y:y*abPanes[0].clientHeight}; }
 function zoomAt(event) { const old=state.zoom, zoom=Math.max(.1,Math.min(6,old*(event.deltaY<0?1.15:1/1.15))); const point=$('view').value==='AB 视图'?abAnchor(event):stagePoint(event); state.panX=point.x-(point.x-state.panX)*(zoom/old); state.panY=point.y-(point.y-state.panY)*(zoom/old); state.zoom=zoom; transform(); }
 stage.addEventListener('wheel',e=>{if(!state.path)return;e.preventDefault();zoomAt(e);},{passive:false});
 stage.addEventListener('pointerdown',event=>{if(event.button===0&&$('view').value==='对比'){state.splitX=stagePoint(event).x;state.dragging={kind:'split'};stage.setPointerCapture(event.pointerId);updateSplit();return;}if(event.button===1||event.button===2){state.dragging={kind:'pan',x:event.clientX,y:event.clientY,px:state.panX,py:state.panY};stage.setPointerCapture(event.pointerId);}});stage.addEventListener('pointermove',event=>{if(state.dragging?.kind==='split'){state.splitX=stagePoint(event).x;updateSplit();}else if(state.dragging?.kind==='pan'){state.panX=state.dragging.px+event.clientX-state.dragging.x;state.panY=state.dragging.py+event.clientY-state.dragging.y;transform();}});stage.addEventListener('pointerup',event=>{if(stage.hasPointerCapture(event.pointerId))stage.releasePointerCapture(event.pointerId);state.dragging=null;});stage.addEventListener('pointercancel',event=>{if(stage.hasPointerCapture(event.pointerId))stage.releasePointerCapture(event.pointerId);state.dragging=null;});stage.oncontextmenu=e=>e.preventDefault();
function stopPlayback(){if(playHandle!==null){cancelAnimationFrame(playHandle);playHandle=null;}$('play').textContent='▶ 播放';}
function playbackTick(now){
  if(playHandle===null)return;
  const total=+$('frame').max+1, fps=state.info?.fps||30;
  const target=(playStartedFrame+Math.floor((now-playStartedAt)*fps/1000))%Math.max(1,total);
  if(!state.busy&&target!==+$('frame').value){$('frame').value=target;$('frame-label').textContent=`帧 ${target} / ${$('frame').max}`;refresh(false,0);}
  playHandle=requestAnimationFrame(playbackTick);
}
$('frame').oninput=async event=>{if(event.isTrusted)stopPlayback();$('frame-label').textContent=`帧 ${$('frame').value} / ${$('frame').max}`;await refresh(false,40);};
$('play').onclick=()=>{if(!state.info||state.kind!=='video')return;if(playHandle!==null){stopPlayback();return;}playStartedAt=performance.now();playStartedFrame=+$('frame').value;$('play').textContent='⏸ 暂停';playHandle=requestAnimationFrame(playbackTick);};
$('export-current').onclick=async()=>{if(!state.path||exportBusy)return;try{const destination=await invoke('choose_export',{video:false});if(!destination)return;await runExport(1,async()=>invoke('save_data_png',{data:await currentImageData(),destination}));log(`已导出当前画面: ${destination}`);status('当前画面已导出');}catch(e){log(`当前画面导出失败: ${e}`);status('导出失败');}};
$('export-full').onclick=async()=>{if(!state.path||exportBusy)return;try{const destination=await invoke('choose_export',{video:state.kind==='video'});if(!destination)return;const total=state.kind==='video'?Math.max(1,state.info?.frames||1):1;await runExport(total,async()=>{const size=outputSize();status(size?`正在按 ${size[0]}×${size[1]} 导出…`:'正在按原始分辨率导出…');if(state.kind==='video'){const frames=await invoke('export_video',{path:state.path,destination,runtime:$('runtime').value,settings:settings(),...outputArgs()});log(`已导出 ${frames} 帧: ${destination}`);}else if(state.kind==='clipboard'){await invoke('save_data_png',{data:await urlToDataUri(state.processedUrl||state.originalUrl),destination,...outputArgs(),...upscaleArgs()});log(`已导出: ${destination}`);}else{await invoke('save_png',{path:state.path,destination,runtime:$('runtime').value,settings:settings(),...outputArgs()});log(`已导出: ${destination}`);}});status('导出完成');}catch(e){log(`导出失败: ${e}`);status('导出失败');}};
$('copy').onclick=async()=>{if(!state.path)return;try{await navigator.clipboard.write([new ClipboardItem({'image/png':await(await fetch(await currentImageData())).blob()})]);status('当前画面已复制');}catch(e){log(`复制失败: ${e}`);}};
document.addEventListener('paste',()=>$('paste').click());
new ResizeObserver(()=>{if(state.path&&Math.abs(state.zoom-state.fit)<.01)resetFit();updateSplit();}).observe(stage);
runtimeReady=initializeRuntime();

// 参数悬停说明
const PARAM_TIPS={
  style:'整体风格预设，决定神经网络输出的总体倾向。\n默认：标准 DLSS 渲染。\n自然：更贴近原片观感，改动更克制。\n电影：对比与氛围感更强的风格化处理。\n风格 3：额外的实验风格，效果因素材而异。\n建议用 AB 视图对比不同风格。',
  intensity:'DLSS 效果的整体权重。\n调大：更接近完整的 DLSS 处理效果。\n调小：逐渐向原图回退，0 时基本不处理。\n想在“增强”与“保真”之间折中时优先调它。',
  tone:'局部色调映射的权重，影响画面各区域自身的明暗与色彩。\n调大：局部明暗层次与色彩变化更充分，画面更通透。\n调小：局部明暗更贴近原片，整体趋于平淡。\n过高可能出现局部明暗跳跃。',
  struct:'局部边缘与纹理的重建权重。\n调大：边缘更锐利、纹理细节更突出。\n调小：细节表现更接近原片，画面更柔和，噪点感更轻。\n过高可能放大噪点或产生过锐边缘。',
  skin:'皮肤区域的专用结构权重，独立于全局结构。\n调大：皮肤纹理（毛孔、发丝等）更清晰。\n调小：皮肤更平滑，近似轻度磨皮，过低可能显得不自然。',
  autoMask:'开关：让网络自动识别应当少处理或不处理的区域\n（如文字、界面、平坦天空等），降低字幕、HUD 被改写的概率。\n关闭时全画面统一处理，效果更均匀，但界面类内容更易受影响。',
  uiCorrection:'开关：针对画面中的界面元素（字幕、HUD、水印等）\n做修正与保留，减轻神经渲染对这类规则图形的涂抹。\n对不含界面元素的素材基本无影响。'
};
const paramTip=document.createElement('div');paramTip.id='param-tip';document.body.appendChild(paramTip);
function showParamTip(info){const text=PARAM_TIPS[info.dataset.param];if(!text)return;paramTip.textContent=text;paramTip.style.display='block';const r=info.getBoundingClientRect(),tw=paramTip.offsetWidth,th=paramTip.offsetHeight;let x=r.left-tw-10;if(x<8)x=Math.min(window.innerWidth-tw-8,Math.max(8,r.left));const y=Math.max(8,Math.min(window.innerHeight-th-8,r.top+r.height/2-th/2));paramTip.style.left=`${x}px`;paramTip.style.top=`${y}px`;}
document.addEventListener('mouseover',e=>{const info=e.target.closest('.info');if(info)showParamTip(info);else if(paramTip.style.display==='block')paramTip.style.display='none';});
document.addEventListener('mouseleave',()=>paramTip.style.display='none');
