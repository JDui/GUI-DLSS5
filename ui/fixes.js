// Post-merge guard fixes for output-size preview invalidation.
// Loaded after app.js so it can replace the handlers installed there without duplicating the main UI logic.

async function rebuildOriginalPreviewForOutput() {
  if (!state.path) return;
  const args = outputArgs();
  let nextUrl = null;

  if (state.kind === 'video') {
    const frame = +$('frame').value;
    nextUrl = await invokePng('frame_png', {
      path: state.path,
      frame,
      maxSide: PREVIEW_MAX_SIDE,
      ...args,
    });
    state.loadedFrame = frame;
  } else if (state.kind === 'clipboard') {
    const image = await loadDataImage(state.sourceData);
    const requested = outputSize() || [image.naturalWidth, image.naturalHeight];
    const max = Math.max(requested[0], requested[1]);
    const scale = max > PREVIEW_MAX_SIDE ? PREVIEW_MAX_SIDE / max : 1;
    const width = Math.max(1, Math.round(requested[0] * scale));
    const height = Math.max(1, Math.round(requested[1] * scale));
    const canvas = document.createElement('canvas');
    canvas.width = width;
    canvas.height = height;
    const context = canvas.getContext('2d');
    context.imageSmoothingEnabled = width < image.naturalWidth || height < image.naturalHeight;
    context.imageSmoothingQuality = 'high';
    context.drawImage(image, 0, 0, width, height);
    nextUrl = URL.createObjectURL(await (await fetch(canvas.toDataURL('image/png'))).blob());
  } else {
    nextUrl = await invokePng('read_image_data', {
      path: state.path,
      maxSide: PREVIEW_MAX_SIDE,
      ...args,
    });
  }

  if (nextUrl) {
    deferRevoke(state.originalUrl);
    state.originalUrl = nextUrl;
    chooseDisplayed(true);
  }
}

async function applyOutputChange() {
  syncOutputInputs();
  try {
    await rebuildOriginalPreviewForOutput();
  } catch (error) {
    log(`原图预览刷新失败: ${error}`);
  }
  refresh(true);
}

$('out-width').onchange = applyOutputChange;
$('out-height').onchange = applyOutputChange;

$('upscale').onchange = async () => {
  updateUpscaleAvailability();
  syncOutputInputs();
  updateSizeNote();
  try {
    await rebuildOriginalPreviewForOutput();
  } catch (error) {
    log(`原图预览刷新失败: ${error}`);
  }
  refresh(true);
};

function applyRatio(k) {
  if (k > 1 && !vsrEnabled()) return;
  const base = sourceSize();
  if (!base) return;
  const target = fitOutput(base[0] * k, base[1] * k);
  if (!target) return;
  $('out-width').value = target[0];
  $('out-height').value = target[1];
  markRatio();
  rebuildOriginalPreviewForOutput()
    .catch(error => log(`原图预览刷新失败: ${error}`))
    .finally(() => refresh(true));
}

$('ratio-1').onclick = () => applyRatio(1);
$('ratio-2').onclick = () => applyRatio(2);
$('ratio-4').onclick = () => applyRatio(4);
