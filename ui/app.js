const invoke = window.__TAURI__.core.invoke;
const $ = id => document.getElementById(id);
const t = (key, variables) => i18n.t(key, variables);
$('gpu-name').textContent = t('gpu.detecting');
$('source').textContent = t('source.empty');
$('status').textContent = t('status.ready');
const DEFAULT_RENDER_SETTINGS = { style:2, skinStructure:0.1, useAutoMask:true, postPerPass:false };
$('style').value = DEFAULT_RENDER_SETTINGS.style;
$('skin').value = $('skin-num').value = DEFAULT_RENDER_SETTINGS.skinStructure;
$('auto-mask').checked = DEFAULT_RENDER_SETTINGS.useAutoMask;
$('post-per-pass').checked = DEFAULT_RENDER_SETTINGS.postPerPass;
const PREVIEW_MAX_SIDE = 4520;
$('out-width').parentElement.title = t('output.customTitle', {maxSide:PREVIEW_MAX_SIDE});
let runtimeReady = Promise.resolve();
const state = { path:null, sourcePath:null, kind:null, info:null, sourceData:null, originalUrl:null, processedUrl:null, loadedFrame:-1, zoom:1, fit:1, panX:0, panY:0, splitX:null, dragging:null, request:0, busy:false, quickOriginal:false };
const stage = $('stage'), preview = $('preview'), originalPreview = $('original-preview'), originalMask = $('original-mask'), abView = $('ab-view');
const abPanes = Array.from(abView.querySelectorAll('.ab-pane')), abOriginal = $('ab-original'), abProcessed = $('ab-processed');
const settings = () => ({ multiPass:$('multi-pass').checked, passCount:+$('pass-count').value, style:+$('style').value, intensity:+$('intensity').value, localTone:+$('tone').value, localStruct:+$('struct').value, skinStructure:+$('skin').value, useAutoMask:$('auto-mask').checked, uiCorrection:$('ui-correction').checked, outputView:0, outputMix:1, brightness:+$('post-brightness').value, contrast:+$('post-contrast').value, saturation:+$('post-saturation').value, postPerPass:$('post-per-pass').checked, upscale:$('upscale').value, vsrQuality:+$('vsr-quality').value, interpolation:+$('interp').value, encoder:$('encoder').value, encoderQuality:+$('encoder-quality').value, keepAudio:$('keep-audio').checked });
const upscaleArgs = () => ({ upscale:$('upscale').value, vsrQuality:+$('vsr-quality').value });
function log(message) { console.debug(`[DLSS5] ${message}`); }
let currentStatus = () => t('status.ready');
function status(message) { currentStatus = typeof message === 'function' ? message : () => message; $('status').textContent = currentStatus(); }
function statusT(key, variables) { status(() => t(key, variables)); }
async function invokePng(cmd, args) { const res = await invoke(cmd, args); const bytes = res instanceof Uint8Array ? res : new Uint8Array(res); return URL.createObjectURL(new Blob([bytes], { type:'image/png' })); }
async function urlToDataUri(url) { const blob = await (await fetch(url)).blob(); return new Promise(resolve => { const reader = new FileReader(); reader.onload = () => resolve(reader.result); reader.readAsDataURL(blob); }); }
// 延迟回收：导出/复制会异步加载正在显示的 URL，立即回收会中断在途加载
function deferRevoke(url) { if(url) setTimeout(() => URL.revokeObjectURL(url), 5000); }
function revokeMedia() { deferRevoke(state.originalUrl); deferRevoke(state.processedUrl); state.originalUrl=null; state.processedUrl=null; }
function syncControls(range, number, max=1) { $(range).oninput = () => { $(number).value = $(range).value; refresh(); }; $(number).onchange = () => { $(range).value = Math.max(0, Math.min(max, +$(number).value || 0)); refresh(); }; }
syncControls('intensity','intensity-num'); syncControls('tone','tone-num'); syncControls('struct','struct-num'); syncControls('skin','skin-num');
syncControls('post-brightness','post-brightness-num',2); syncControls('post-contrast','post-contrast-num',2); syncControls('post-saturation','post-saturation-num',2);
$('post-per-pass').onchange=()=>refresh();
$('auto-mask').onchange=()=>refresh(); $('ui-correction').onchange=()=>refresh();
$('multi-pass').onchange=()=>{ $('pass-control').hidden=!$('multi-pass').checked; refresh(); };
function syncPassCount(value) {
  const count=Math.max(1,Math.min(5,Math.round(Number(value)||1)));
  $('pass-count').value=count;
  $('pass-count-num').value=count;
  refresh();
}
$('pass-count').oninput=()=>syncPassCount($('pass-count').value);
$('pass-count-num').onchange=()=>syncPassCount($('pass-count-num').value);
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
function updateSizeNote() { const s=sourceSize(), el=$('size-note'); if(el) el.textContent=s?t('output.sizeNote',{width:s[0],height:s[1]}):''; }
function updateInterpNote() {
  const fps = state.info?.fps || 0;
  const note = $('interp-note');
  if (note) note.textContent = fps > 0 ? t('interp.fpsNote',{fps: fps >= 100 ? fps.toFixed(0) : fps.toFixed(2)}) : '';
}
function setRatio(k) { if(!vsrEnabled())return; const base=sourceSize(); if(!base)return; const t=fitOutput(base[0]*k,base[1]*k); if(!t)return; $('out-width').value=t[0]; $('out-height').value=t[1]; markRatio(); refresh(true); }
$('ratio-1').onclick=()=>setRatio(1); $('ratio-2').onclick=()=>setRatio(2); $('ratio-4').onclick=()=>setRatio(4);
$('out-width').onchange=$('out-height').onchange=()=>{syncOutputInputs();refresh(true);};
function updateUpscaleAvailability(){
  const on=vsrEnabled(), cap=outputCap();
  $('ratio-2').disabled=!on; $('ratio-4').disabled=!on;
  $('ratio-2').title=on?t('output.ratio2'):t('upscale.vsrRequired');
  $('ratio-4').title=on?t('output.ratio4'):t('upscale.vsrRequired');
  $('out-width').max=cap.w; $('out-height').max=cap.h;
}
$('upscale').onchange=()=>{updateUpscaleAvailability();syncOutputInputs();updateSizeNote();refresh(true);};
$('vsr-quality').onchange=()=>refresh();
function syncEncoderControls() {
  const lossless = $('encoder').value === 'h265_nvenc_lossless';
  const quality = $('encoder-quality');
  quality.disabled = lossless;
  quality.title = t(lossless ? 'encoder.losslessHint' : 'encoder.qualityHint');
  $('encoder-quality-note').textContent = t(lossless ? 'encoder.losslessHint' : 'encoder.qualityHint');
}
$('encoder').onchange=()=>syncEncoderControls();
syncEncoderControls();
document.querySelectorAll('.tabs .tab').forEach(button=>button.onclick=()=>{
  document.querySelectorAll('.tabs .tab').forEach(item=>item.classList.toggle('active',item===button));
  document.querySelectorAll('.tab-page').forEach(page=>page.hidden=page.id!=='tab-'+button.dataset.tab);
});
invoke('vsr_probe').then(available=>{if(!available)markVsrUnavailable();}).catch(()=>markVsrUnavailable());
function markVsrUnavailable(){
  const option=$('upscale').querySelector('option[value=vsr]');
  option.disabled=true;option.textContent=t('upscale.vsrUnavailable');
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
function displayedSize() { if ($('view').value === 'ab') return syncAbLayout(); if ($('view').value === 'compare') return syncCompareLayout(); const w = preview.naturalWidth || 1, h = preview.naturalHeight || 1; return [w,h]; }
function activeViewport() { return $('view').value === 'ab' ? abPanes[0] : stage; }
function resetFit() { const [w,h] = displayedSize(), viewport = activeViewport(); const width = viewport.clientWidth || stage.clientWidth, height = viewport.clientHeight || stage.clientHeight; state.fit = Math.min(width / w, height / h, 1); state.zoom = state.fit; state.panX = (width - w * state.zoom) / 2; state.panY = (height - h * state.zoom) / 2; transform(); }
function transform() { const matrix = `translate(${state.panX}px,${state.panY}px) scale(${state.zoom})`; preview.style.transform = matrix; originalPreview.style.transform = matrix; abOriginal.style.transform = matrix; abProcessed.style.transform = matrix; abView.style.transform = 'none'; $('zoom').textContent = `${Math.round(state.zoom * 100)}% · ${t(Math.abs(state.zoom - 1) < .01 ? 'preview.clickFit' : 'preview.click100')}`; }
function updateSplit() { const x = Math.max(0, Math.min(stage.clientWidth, state.splitX ?? stage.clientWidth / 2)); $('split-line').style.left = `${x}px`; originalMask.style.width = `${x}px`; }
function fitWhenReady(image) {
  const fit = () => requestAnimationFrame(() => { if ($('view').value === 'ab') syncAbLayout(); else if ($('view').value === 'compare') syncCompareLayout(); resetFit(); });
  if (image.complete && image.naturalWidth) fit();
  else image.addEventListener('load', fit, {once:true});
}
function chooseDisplayed(fit = false) {
  if (!state.originalUrl) return;
  const view = $('view').value, output = state.processedUrl || state.originalUrl;
  if (view !== 'quick') state.quickOriginal = false;
  preview.style.display = 'none'; preview.style.width = ''; preview.style.height = ''; originalMask.style.display = 'none'; originalPreview.style.display = 'none'; originalPreview.style.width = ''; originalPreview.style.height = ''; abView.style.display = 'none'; $('split-line').style.display = 'none'; $('compare-left').style.display = 'none'; $('compare-right').style.display = 'none'; $('ab-option').style.display = view === 'ab' ? 'inline-flex' : 'none';
  stage.classList.toggle('ab-vertical', view === 'ab' && $('ab-layout').value === 'vertical');
  if (view === 'quick') { preview.src = state.quickOriginal ? state.originalUrl : output; preview.style.display = 'block'; $('compare-left').style.display = 'block'; $('compare-left').textContent = state.quickOriginal ? t('compare.original') : t('compare.dlss'); }
  else if (view === 'compare') { preview.src = output; originalPreview.src = state.originalUrl; syncCompareLayout(); preview.style.display = 'block'; originalMask.style.display = 'block'; originalPreview.style.display = 'block'; $('split-line').style.display = 'block'; $('compare-left').textContent = t('compare.original'); $('compare-left').style.display = 'block'; $('compare-right').textContent = t('compare.dlss'); $('compare-right').style.display = 'block'; updateSplit(); }
  else { abOriginal.src = state.originalUrl; abProcessed.src = output; abView.className = $('ab-layout').value; syncAbLayout(); abView.style.display = 'flex'; $('compare-left').textContent = t('compare.original'); $('compare-left').style.display = 'block'; $('compare-right').textContent = t('compare.dlss'); $('compare-right').style.display = 'block'; }
  $('empty').style.display = 'none'; if (fit) fitWhenReady(view === 'ab' ? $('ab-original') : preview); else transform();
}
function loadDataImage(data) { return new Promise((resolve,reject) => { const image=new Image(); image.onload=()=>resolve(image); image.onerror=reject; image.src=data; }); }
async function currentImageData() {
  const view = $('view').value; if (view === 'quick') return urlToDataUri(state.quickOriginal || !state.processedUrl ? state.originalUrl : state.processedUrl); if (!state.processedUrl) return urlToDataUri(state.originalUrl);
  const original = await loadDataImage(state.originalUrl), processed = await loadDataImage(state.processedUrl), ab = view === 'ab', vertical = ab && $('ab-layout').value === 'vertical';
  const w=original.naturalWidth, h=original.naturalHeight; const canvas=document.createElement('canvas'); canvas.width=ab&&!vertical ? w*2 : w; canvas.height=ab&&vertical ? h*2 : h; const c=canvas.getContext('2d'); c.drawImage(original,0,0,w,h);
  if(ab) c.drawImage(processed,vertical?0:w,vertical?h:0,w,h); else { const split=Math.max(0,Math.min(w,((state.splitX ?? stage.clientWidth/2)-state.panX)/state.zoom)); c.save();c.beginPath();c.rect(split,0,w-split,h);c.clip();c.drawImage(processed,0,0,w,h);c.restore(); }
  return canvas.toDataURL('image/png');
}
function scaleClipboard(data) { return loadDataImage(data).then(image => { const max=Math.max(image.naturalWidth,image.naturalHeight); if(max<=PREVIEW_MAX_SIDE) return data; const s=PREVIEW_MAX_SIDE/max, canvas=document.createElement('canvas'); canvas.width=Math.round(image.naturalWidth*s); canvas.height=Math.round(image.naturalHeight*s); canvas.getContext('2d').drawImage(image,0,0,canvas.width,canvas.height); return canvas.toDataURL('image/png'); }); }
let loadingPath='', loadingStarted=0;
async function initializeRuntime() {
  const select=$('runtime'), gpuName=$('gpu-name'); select.disabled=true; statusT('status.detectingGpu');
  try {
    const gpu=await invoke('gpu_info');
    gpuName.textContent=gpu.name;
    gpuName.dataset.detectedName=gpu.name;
    gpuName.title=gpu.name;
    gpuName.classList.toggle('unavailable',!gpu.detected);
    if(['30','40','50'].includes(gpu.runtime)) select.value=gpu.runtime;
    select.title=gpu.detected?t('gpu.detectedTitle',{name:gpu.name}):t('gpu.notDetectedTitle');
    if(gpu.detected){log(`已识别 ${gpu.name}，自动选择 RTX ${gpu.runtime}`);statusT('status.gpuSelected',{runtime:gpu.runtime});}
    else {log(`显卡识别失败：${gpu.name}`);statusT('status.gpuNotDetected');}
  } catch(e) {
    gpuName.textContent=t('gpu.notDetected');
    delete gpuName.dataset.detectedName;
    gpuName.title=t('gpu.failureTitle');
    gpuName.classList.add('unavailable');
    log(`显卡识别: ${e}`); select.title=t('gpu.notDetectedTitle'); statusT('status.gpuFailure');
  } finally { select.disabled=false; }
}
async function loadPath(path) {
  await runtimeReady;
  stopPlayback();
  statusT('status.reading');
  const now=Date.now();
  if(path===loadingPath&&now-loadingStarted<500)return;
  loadingPath=path;loadingStarted=now;
  clearTimeout(refreshTimer);
  state.request++;
  refreshQueued=false;
  refreshQueuedFit=false;
  const info=await invoke('media_info',{path}); revokeMedia(); Object.assign(state,{path:info.path,sourcePath:info.sourcePath,kind:info.kind,info,sourceData:null,splitX:null,loadedFrame:-1});
  const initial=fitOutput(info.width,info.height)||[info.width,info.height];
  $('out-width').value=initial[0]; $('out-height').value=initial[1]; markRatio(); updateSizeNote(); updateInterpNote();
  if(info.kind==='video') {
    statusT('status.firstPreview');
    state.originalUrl=await invokePng('frame_png',{path:info.path,frame:0,maxSide:PREVIEW_MAX_SIDE,...outputArgs(),...upscaleArgs()});
    state.loadedFrame=0;
  } else {
    state.originalUrl=await invokePng('read_image_data',{path:info.path,maxSide:PREVIEW_MAX_SIDE,...outputArgs()});
  }
  $('source').textContent=t('source.loaded',{name:path.split(/[\\/]/).pop(),type:info.kind==='video'?t('media.video'):t('media.image'),width:info.width,height:info.height}); $('frame').max=Math.max(0,info.frames-1); $('frame').value=0; $('frame-label').textContent=t('frame.label',{current:0,total:Math.max(0,info.frames-1)}); $('export-full').textContent=t(info.kind==='video'?'footer.exportVideo':'footer.exportImage');
  chooseDisplayed(true);
  statusT('status.preview'); await refresh(true,0);
}
function normalizeDroppedPath(value) { if(typeof value!=='string') return ''; const raw=value.trim(); if(!raw) return ''; if(!raw.toLowerCase().startsWith('file://')) return raw; try { const url=new URL(raw); let path=decodeURIComponent(url.pathname); if(/^\/[A-Za-z]:/.test(path)) path=path.slice(1); if(url.hostname&&url.hostname!=='localhost') path=`\\\\${url.hostname}${path}`; return path; } catch(_) { return raw; } }
function droppedPaths(payload) { const value=payload?.paths??payload; return Array.isArray(value)?value:(value?[value]:[]); }
let lastDropPath='', lastDropAt=0;
async function loadDroppedPath(value) { const path=normalizeDroppedPath(value); if(!path){statusT('status.readDropFailed');return;} const now=Date.now(); if(path===lastDropPath&&now-lastDropAt<500)return; lastDropPath=path;lastDropAt=now; try { statusT('status.reading'); await loadPath(path); } catch(e) { log(`拖放: ${e}`); statusT('status.dropFailed'); } }
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
let exportBusy=false, exportPaused=false, exportPollFailureLogged=false;
const exportButtons=[$('export-current'),$('export-full')];
let latestExportProgress=null;
function localizeExportProgressMessage(message) {
  const text=String(message||'');
  const fixed={
    '准备导出…':'status.prepareExport',
    '导出完成':'status.exportDone',
    '导出已取消':'status.exportCancelled',
    '已暂停':'status.exportPaused',
    '正在继续导出…':'status.exportResume'
  };
  if(fixed[text])return t(fixed[text]);
  const encoder=text.match(/^使用 (.+)，正在导出 (\d+)×(\d+)…$/);
  if(encoder)return t('status.exportEncoderProgress',{encoder:encoder[1],width:encoder[2],height:encoder[3]});
  const frame=text.match(/^正在导出第 (\d+) \/ (\d+) 帧$/);
  if(frame)return t('status.exportFrame',{current:frame[1],total:frame[2]});
  const failed=text.match(/^导出失败：(.+)$/);
  if(failed)return t('status.exportFailedDetail',{error:failed[1]});
  return text||t('status.exporting');
}
function renderExportProgress(progress) {
  latestExportProgress=progress;
  const wrap=$('export-progress-wrap'), bar=$('export-progress'), label=$('export-progress-label');
  const total=Math.max(0,Number(progress?.total)||0), current=Math.max(0,Number(progress?.current)||0);
  if(!progress?.active&&!progress?.message&&current===0){wrap.hidden=true;return;}
  wrap.hidden=false;
  if(total>0){bar.max=total;bar.value=Math.min(current,total);}else{bar.removeAttribute('value');}
  const percent=total>0?` ${Math.round(Math.min(1,current/total)*100)}%`:'';
  const message=progress.messageKey?t(progress.messageKey,progress.messageVariables):localizeExportProgressMessage(progress.message);
  label.textContent=`${message}${percent}`;
}
function showExportProgress(current,total,messageKey,messageVariables) {
  renderExportProgress({active:true,current,total,messageKey,messageVariables});
}
async function pollExportProgress() {
  try {
    renderExportProgress(await invoke('poll_export_progress'));
  } catch(e) {
    if(!exportPollFailureLogged){log(`导出进度: ${e}`);exportPollFailureLogged=true;}
  }
}
setInterval(pollExportProgress,120);
pollExportProgress();
function setExportBusy(busy) { exportBusy=busy; exportButtons.forEach(button=>button.disabled=busy); $('export-pause').hidden=!busy; $('export-cancel').hidden=!busy; if(!busy){exportPaused=false;$('export-pause').textContent=t('progress.pause');} }
async function runExport(total, task) {
  setExportBusy(true);
  showExportProgress(0,total,'status.prepareExport');
  try {
    const result=await task();
    showExportProgress(total,total,'status.exportDone');
    return result;
  } catch(e) {
    showExportProgress(0,total,'status.exportFailed');
    throw e;
  } finally { setExportBusy(false); }
}
const exportPauseButton = $('export-pause');
exportPauseButton.onclick=async()=>{ if(!exportBusy)return; const paused=!exportPaused; try { await invoke('export_set_paused',{paused}); exportPaused=paused; exportPauseButton.textContent=paused?t('progress.resume'):t('progress.pause'); statusT(paused?'status.exportPaused':'status.exportResume'); } catch(e) { log(`导出暂停/继续: ${e}`); } };
$('export-cancel').onclick=()=>{ if(!exportBusy)return; statusT('status.cancelExport'); invoke('export_cancel').catch(e=>log(`取消导出: ${e}`)); };
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
      statusT('status.refreshPreview');
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
        statusT('status.ready');
      } else {
        URL.revokeObjectURL(processedUrl);
        if(originalUrl)URL.revokeObjectURL(originalUrl);
      }
    } catch(e) {
      if(request===state.request&&renderPath===state.path){log(`DLSS: ${e}`);statusT('status.previewFailed');}
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
$('open').onclick=async()=>{try{const path=await invoke('choose_media');if(path)await loadPath(path);}catch(e){log(String(e));statusT('status.importFailed');}};
$('paste').onclick=async()=>{try{const item=(await navigator.clipboard.read()).find(i=>i.types.some(t=>t.startsWith('image/')));if(!item)throw Error(t('status.clipboardNoImage'));const type=item.types.find(t=>t.startsWith('image/'));const blob=await item.getType(type),reader=new FileReader();reader.onload=async()=>{const scaled=await scaleClipboard(reader.result);const pasted=await loadDataImage(scaled);revokeMedia();Object.assign(state,{path:'clipboard-image',kind:'clipboard',info:{kind:'image',frames:1,width:pasted.naturalWidth,height:pasted.naturalHeight},sourceData:scaled,originalUrl:URL.createObjectURL(await (await fetch(scaled)).blob()),splitX:null,loadedFrame:-1});$('out-width').value=pasted.naturalWidth;$('out-height').value=pasted.naturalHeight;markRatio();updateSizeNote();$('source').textContent=t('source.pasted');await refresh(true);};reader.readAsDataURL(blob);}catch(e){log(`粘贴: ${e}`);statusT('status.pasteFailed');}};
const VIEW_HINT_KEYS={ quick:'hint.quick', compare:'hint.compare', ab:'hint.ab' };
function syncViewHint(){ $('hint').textContent=t(VIEW_HINT_KEYS[$('view').value]||'hint.quick'); }
const viewSwitch=$('view-switch'), viewThumb=$('view-thumb');
function syncViewSwitch() {
  const current=$('view').value;
  viewSwitch.querySelectorAll('.view-opt').forEach(button=>button.classList.toggle('active',button.dataset.view===current));
  const active=viewSwitch.querySelector('.view-opt.active');
  if(active){viewThumb.style.left=`${active.offsetLeft}px`;viewThumb.style.width=`${active.offsetWidth}px`;}
}
viewSwitch.querySelectorAll('.view-opt').forEach(button=>button.onclick=()=>{
  if($('view').value===button.dataset.view)return;
  $('view').value=button.dataset.view;
  $('view').dispatchEvent(new Event('change'));
});
window.addEventListener('resize',syncViewSwitch);
syncViewSwitch();
  $('view').onchange=()=>{syncViewHint();syncViewSwitch();chooseDisplayed(true);}; $('ab-layout').onchange=()=>chooseDisplayed(true); $('style').onchange=()=>refresh(); $('runtime').onchange=()=>{log('运行时变更需重启应用后生效。');statusT('status.runtimeRestart');refresh();};
syncViewHint();
  $('zoom').onclick=()=>{if(!state.path)return;if(Math.abs(state.zoom-1)<.01)resetFit();else{const [w,h]=displayedSize(),viewport=activeViewport();state.zoom=1;state.panX=(viewport.clientWidth-w)/2;state.panY=(viewport.clientHeight-h)/2;transform();}};
 function stagePoint(event) { const rect=stage.getBoundingClientRect(); return {x:Math.max(0,Math.min(stage.clientWidth,event.clientX-rect.left)),y:Math.max(0,Math.min(stage.clientHeight,event.clientY-rect.top))}; }
 function abAnchor(event) { const pane=abPanes.find(item=>{const rect=item.getBoundingClientRect();return event.clientX>=rect.left&&event.clientX<=rect.right&&event.clientY>=rect.top&&event.clientY<=rect.bottom;})||abPanes[0]; const rect=pane.getBoundingClientRect(); const x=rect.width?Math.max(0,Math.min(1,(event.clientX-rect.left)/rect.width)):0.5; const y=rect.height?Math.max(0,Math.min(1,(event.clientY-rect.top)/rect.height)):0.5; return {x:x*abPanes[0].clientWidth,y:y*abPanes[0].clientHeight}; }
 function zoomAt(event) { const old=state.zoom, zoom=Math.max(.1,Math.min(6,old*(event.deltaY<0?1.15:1/1.15))); const point=$('view').value==='ab'?abAnchor(event):stagePoint(event); state.panX=point.x-(point.x-state.panX)*(zoom/old); state.panY=point.y-(point.y-state.panY)*(zoom/old); state.zoom=zoom; transform(); }
 stage.addEventListener('wheel',e=>{if(!state.path)return;e.preventDefault();zoomAt(e);},{passive:false});
 stage.addEventListener('pointerdown',event=>{if(event.button===0&&$('view').value==='compare'){state.splitX=stagePoint(event).x;state.dragging={kind:'split'};stage.setPointerCapture(event.pointerId);updateSplit();return;}if(event.button===0&&$('view').value==='quick'){state.quickOriginal=true;preview.src=state.originalUrl;$('compare-left').textContent=t('compare.original');$('compare-left').style.display='block';state.dragging={kind:'quick'};stage.setPointerCapture(event.pointerId);return;}if(event.button===1||event.button===2){state.dragging={kind:'pan',x:event.clientX,y:event.clientY,px:state.panX,py:state.panY};stage.setPointerCapture(event.pointerId);}});stage.addEventListener('pointermove',event=>{if(state.dragging?.kind==='split'){state.splitX=stagePoint(event).x;updateSplit();}else if(state.dragging?.kind==='pan'){state.panX=state.dragging.px+event.clientX-state.dragging.x;state.panY=state.dragging.py+event.clientY-state.dragging.y;transform();}});function endQuickCompare(){if(state.dragging?.kind!=='quick')return;state.quickOriginal=false;preview.src=state.processedUrl||state.originalUrl;$('compare-left').textContent=t('compare.dlss');}stage.addEventListener('pointerup',event=>{if(stage.hasPointerCapture(event.pointerId))stage.releasePointerCapture(event.pointerId);endQuickCompare();state.dragging=null;});stage.addEventListener('pointercancel',event=>{if(stage.hasPointerCapture(event.pointerId))stage.releasePointerCapture(event.pointerId);endQuickCompare();state.dragging=null;});stage.oncontextmenu=e=>e.preventDefault();
 function stopPlayback(){if(playHandle!==null){cancelAnimationFrame(playHandle);playHandle=null;}$('play').textContent=t('play.play');}
function playbackTick(now){
  if(playHandle===null)return;
  const total=+$('frame').max+1, fps=state.info?.fps||30;
  const target=(playStartedFrame+Math.floor((now-playStartedAt)*fps/1000))%Math.max(1,total);
  if(!state.busy&&target!==+$('frame').value){$('frame').value=target;$('frame-label').textContent=t('frame.label',{current:target,total:$('frame').max});refresh(false,0);}
  playHandle=requestAnimationFrame(playbackTick);
}
$('frame').oninput=async event=>{if(event.isTrusted)stopPlayback();$('frame-label').textContent=t('frame.label',{current:$('frame').value,total:$('frame').max});await refresh(false,40);};
 $('play').onclick=()=>{if(!state.info||state.kind!=='video')return;if(playHandle!==null){stopPlayback();return;}playStartedAt=performance.now();playStartedFrame=+$('frame').value;$('play').textContent=t('play.pause');playHandle=requestAnimationFrame(playbackTick);};
$('export-current').onclick=async()=>{if(!state.path||exportBusy)return;try{const destination=await invoke('choose_export',{video:false});if(!destination)return;await runExport(1,async()=>invoke('save_data_png',{data:await currentImageData(),destination}));log(`已导出当前画面: ${destination}`);statusT('status.currentExported');}catch(e){log(`当前画面导出失败: ${e}`);statusT('status.exportFailed');}};
$('export-full').onclick=async()=>{if(!state.path||exportBusy)return;try{const destination=await invoke('choose_export',{video:state.kind==='video'});if(!destination)return;const total=state.kind==='video'?Math.max(1,state.info?.frames||1):1;await runExport(total,async()=>{const size=outputSize();status(size?()=>t('status.exportSize',{width:size[0],height:size[1]}):()=>t('status.exportOriginalSize'));if(state.kind==='video'){const frames=await invoke('export_video',{path:state.path,destination,runtime:$('runtime').value,settings:settings(),...outputArgs()});log(`已导出 ${frames} 帧: ${destination}`);}else if(state.kind==='clipboard'){await invoke('save_data_png',{data:await urlToDataUri(state.processedUrl||state.originalUrl),destination,...outputArgs(),...upscaleArgs()});log(`已导出: ${destination}`);}else{await invoke('save_png',{path:state.path,destination,runtime:$('runtime').value,settings:settings(),...outputArgs()});log(`已导出: ${destination}`);}});statusT('status.exportDone');}catch(e){log(`导出失败: ${e}`);const text=String(e);statusT(text.includes('取消')||text.toLowerCase().includes('cancel')?'status.exportCancelled':'status.exportFailed');}};
$('copy').onclick=async()=>{if(!state.path)return;try{await navigator.clipboard.write([new ClipboardItem({'image/png':await(await fetch(await currentImageData())).blob()})]);statusT('status.copied');}catch(e){log(`复制失败: ${e}`);statusT('status.copyFailed');}};
document.addEventListener('paste',()=>$('paste').click());
new ResizeObserver(()=>{if(state.path&&Math.abs(state.zoom-state.fit)<.01)resetFit();updateSplit();}).observe(stage);
runtimeReady=initializeRuntime();

// 参数悬停说明
const paramTip=document.createElement('div');paramTip.id='param-tip';document.body.appendChild(paramTip);
function showParamTip(info){const text=t(`tips.${info.dataset.param}`);if(!text)return;paramTip.textContent=text;paramTip.style.display='block';const r=info.getBoundingClientRect(),tw=paramTip.offsetWidth,th=paramTip.offsetHeight;let x=r.left-tw-10;if(x<8)x=Math.min(window.innerWidth-tw-8,Math.max(8,r.left));const y=Math.max(8,Math.min(window.innerHeight-th-8,r.top+r.height/2-th/2));paramTip.style.left=`${x}px`;paramTip.style.top=`${y}px`;}
document.addEventListener('mouseover',e=>{const info=e.target.closest('.info');if(info)showParamTip(info);else if(paramTip.style.display==='block')paramTip.style.display='none';});
document.addEventListener('mouseleave',()=>paramTip.style.display='none');

// ===== 批量处理 =====
let batchRows = [];
let batchDir = '';
let batchRunning = false;
let batchPollTimer = null;

function renderBatchRows() {
  const list = $('batch-list');
  list.innerHTML = '';
  if (!batchRows.length) {
    const empty = document.createElement('div');
    empty.className = 'batch-empty';
    empty.textContent = t('batch.empty');
    list.appendChild(empty);
    return;
  }
  batchRows.forEach((r, i) => {
    const row = document.createElement('div');
    row.className = 'batch-row';
    row.id = `batch-row-${i}`;
    row.innerHTML = `<span class="row-name" title="${r.path}">${r.name}</span>` +
      `<progress class="row-bar" max="100" value="0" hidden></progress>` +
      `<span class="row-status">${t('batch.pending')}</span>`;
    list.appendChild(row);
  });
}

function applyBatchState(b) {
  b.jobs.forEach((job, i) => {
    const row = $(`batch-row-${i}`);
    if (!row) return;
    const bar = row.querySelector('.row-bar');
    const label = row.querySelector('.row-status');
    label.classList.remove('ok', 'bad');
    if (job.status === 'running') {
      bar.hidden = false;
      bar.value = job.frames ? (job.current / job.frames) * 100 : 0;
      label.textContent = t('batch.running',{current:job.current,frames:job.frames,fps:job.fps.toFixed(1)});
    } else if (job.status === 'done') {
      bar.hidden = true;
      label.textContent = t('batch.done');
      label.classList.add('ok');
    } else if (job.status === 'failed') {
      bar.hidden = true;
      label.textContent = t('batch.failed');
      label.classList.add('bad');
      label.title = job.error || '';
    } else if (job.status === 'cancelled') {
      bar.hidden = true;
      label.textContent = t('batch.cancelled');
    } else {
      bar.hidden = true;
      label.textContent = t('batch.pending');
    }
  });
  if (b.cancelled) statusT('status.batchCancelled');
}

function stopBatchPoll() { if (batchPollTimer) { clearInterval(batchPollTimer); batchPollTimer = null; } }

function finishBatchPoll() {
  stopBatchPoll();
  batchRunning = false;
  $('batch-start').disabled = false;
  $('batch-add').disabled = false;
  $('batch-cancel').hidden = true;
  invoke('batch_state').then(b => { if ($('batch-modal').hidden === false) applyBatchState(b); }).catch(() => {});
}

function startBatchPoll() {
  stopBatchPoll();
  batchPollTimer = setInterval(async () => {
    try {
      const b = await invoke('batch_state');
      if ($('batch-modal').hidden === false) applyBatchState(b);
      if (!b.running) finishBatchPoll();
    } catch (e) { log(`批量: ${e}`); }
  }, 200);
}

function batchSummary() {
  const ratio = (vsrEnabled() && sourceSize() && outputSize()) ? $('out-width').value / sourceSize()[0] : 1;
  const ratioText = ratio > 1 ? ` X${+ratio.toFixed(2)}` : '';
  const up = vsrEnabled() ? t('batch.vsr',{ratio:ratioText,quality:$('vsr-quality').value}) : t('batch.off');
  const interp = { '1': t('interp.off'), '2': '2x', '3': '3x', '4': '4x' }[$('interp').value] || t('interp.off');
  const encoderValue = $('encoder').value;
  const enc = { 'h264_nvenc': t('encoder.h264Nvenc'), 'h265_nvenc': t('encoder.h265Nvenc'), 'h265_nvenc_lossless': t('encoder.h265Lossless'), 'h264_x264': t('encoder.h264Cpu'), 'h265_x265': t('encoder.h265Cpu') }[encoderValue] || encoderValue;
  const styleText = $('style').selectedOptions[0] ? $('style').selectedOptions[0].textContent : t('style.default');
  const postOn = $('post-brightness').value !== '1' || $('post-contrast').value !== '1' || $('post-saturation').value !== '1';
  const postText = postOn ? t('batch.post',{brightness:$('post-brightness').value,contrast:$('post-contrast').value,saturation:$('post-saturation').value}) : '';
  const pass = $('multi-pass').checked ? t('batch.pass',{count:$('pass-count').value}) : '';
  return [
    t('batch.upscale',{value:up + t('batch.interp',{interp})}),
    t('batch.encoder',{value:enc,quality:encoderValue==='h265_nvenc_lossless'?t('encoder.lossless'):$('encoder-quality').value,audio:$('keep-audio').checked?t('batch.audioOn'):t('batch.audioOff')}),
    t('batch.dlss',{style:styleText,intensity:$('intensity').value,pass,post:postText}),
  ].join('\n');
}
$('batch-open').onclick = () => { $('batch-modal').hidden = false; $('batch-summary').textContent = batchSummary(); renderBatchRows(); if (batchRunning) startBatchPoll(); };
$('batch-close').onclick = () => {
  if (batchRunning && !confirm(t('batch.confirmClose'))) return;
  if (batchRunning) invoke('batch_cancel');
  $('batch-modal').hidden = true;
  stopBatchPoll();
};
$('batch-add').onclick = async () => {
  try {
    const paths = await invoke('choose_media_multi');
    if (!paths) return;
    for (const p of paths) {
      if (!batchRows.some(r => r.path === p)) batchRows.push({ path: p, name: p.substring(Math.max(p.lastIndexOf('\\'), p.lastIndexOf('/')) + 1) });
    }
    renderBatchRows();
  } catch (e) { log(`批量: ${e}`); }
};
$('batch-dir').onclick = async () => {
  try {
    const dir = await invoke('choose_directory');
    if (dir) { batchDir = dir; $('batch-dir-label').textContent = dir; $('batch-dir-label').title = dir; }
  } catch (e) { log(`批量: ${e}`); }
};
$('batch-start').onclick = async () => {
  if (batchRunning) return;
  if (!batchRows.length) { statusT('status.chooseVideos'); return; }
  if (!batchDir) { statusT('status.chooseOutputDir'); return; }
  batchRunning = true;
  $('batch-start').disabled = true;
  $('batch-add').disabled = true;
  $('batch-cancel').hidden = false;
  startBatchPoll();
  try {
    await invoke('batch_export', {
      paths: batchRows.map(r => r.path),
      outputDir: batchDir,
      runtime: $('runtime').value,
      settings: settings(),
      outputRatio: (vsrEnabled() && sourceSize() && outputSize()) ? $('out-width').value / sourceSize()[0] : null,
    });
  } catch (e) {
    log(`批量: ${e}`);
    statusT('status.batchExportFailed',{error:e});
  }
  finishBatchPoll();
};
$('batch-cancel').onclick = () => {
  invoke('batch_cancel');
  statusT('status.cancelBatch');
};

function refreshLocalizedUi() {
  $('out-width').parentElement.title = t('output.customTitle', {maxSide:PREVIEW_MAX_SIDE});
  $('gpu-name').textContent = $('gpu-name').dataset.detectedName || ($('gpu-name').classList.contains('unavailable') ? t('gpu.notDetected') : t('gpu.detecting'));
  $('source').textContent = state.path && state.info
    ? (state.kind === 'clipboard'
      ? t('source.pasted')
      : t('source.loaded',{name:state.path.split(/[\\/]/).pop(),type:state.kind==='video'?t('media.video'):t('media.image'),width:state.info.width,height:state.info.height}))
    : t('source.empty');
  $('frame-label').textContent=t('frame.label',{current:$('frame').value,total:$('frame').max});
  $('export-full').textContent=state.kind==='video'?t('footer.exportVideo'):(state.kind?t('footer.exportImage'):t('footer.exportVideo'));
  $('batch-dir-label').textContent=batchDir||t('batch.unselected');
  $('batch-dir-label').title=batchDir||'';
  const detectedGpu=$('gpu-name').dataset.detectedName;
  $('runtime').title=detectedGpu?t('gpu.detectedTitle',{name:detectedGpu}):t('gpu.notDetectedTitle');
    $('status').textContent=currentStatus();
    if (latestExportProgress) renderExportProgress(latestExportProgress);
  $('export-pause').textContent=exportPaused?t('progress.resume'):t('progress.pause');
  syncEncoderControls();
  const vsrOption=$('upscale').querySelector('option[value=vsr]');
  if (vsrOption?.disabled) vsrOption.textContent=t('upscale.vsrUnavailable');
  updateUpscaleAvailability();
  syncViewHint();
  syncViewSwitch();
  if (state.originalUrl) chooseDisplayed(false);
  if (paramTip.style.display==='block') {
    const visibleInfo=document.querySelector('.info:hover');
    if (visibleInfo) showParamTip(visibleInfo);
  }
  if (!$('batch-modal').hidden) {
    $('batch-summary').textContent=batchSummary();
    renderBatchRows();
    invoke('batch_state').then(applyBatchState).catch(() => {});
  }
}
i18n.onChange(refreshLocalizedUi);
