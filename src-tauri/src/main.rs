#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod passes;

use base64::{engine::general_purpose::STANDARD, Engine};
use image::{
    codecs::png::PngEncoder, imageops::FilterType, ExtendedColorType, ImageEncoder, RgbaImage,
};
use libloading::Library;
use serde::{Deserialize, Serialize};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::{
    collections::{HashMap, VecDeque},
    ffi::OsStr,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdout, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Mutex,
    },
};
use tauri::{ipc::Response, Manager};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const PREVIEW_CACHE_FRAMES: usize = 12;
const SEQUENTIAL_DECODE_LIMIT: u32 = 90;
const MAX_PREVIEW_SIDE: u32 = 4520;

type InitFn = unsafe extern "C" fn(i32, i32, i32, *const u16, *const u16) -> i32;
type CreateFn = unsafe extern "C" fn(i32, i32, i32) -> i32;
type ProcessFn = unsafe extern "C" fn(*mut u8, *mut f32, *mut f32, *mut u8, i32) -> i32;
type OptionsFn = unsafe extern "C" fn(i32, i32, f32, f32, f32, f32, i32, i32, i32, i32, f32, f32);
type ResizeFn = unsafe extern "C" fn(i32, i32, i32) -> i32;
type ShutdownFn = unsafe extern "C" fn();

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
struct RenderSettings {
    multi_pass: bool,
    pass_count: i32,
    style: i32,
    intensity: f32,
    local_tone: f32,
    local_struct: f32,
    skin_structure: f32,
    use_auto_mask: bool,
    ui_correction: bool,
    output_view: i32,
    output_mix: f32,
    brightness: f32,
    contrast: f32,
    saturation: f32,
    post_per_pass: bool,
    upscale: String,
    vsr_quality: i32,
    encoder: String,
    encoder_quality: i32,
    keep_audio: bool,
}

impl Default for RenderSettings {
    fn default() -> Self {
        Self {
            multi_pass: false,
            pass_count: 2,
            style: 2,
            intensity: 1.0,
            local_tone: 1.0,
            local_struct: 1.0,
            skin_structure: 0.1,
            use_auto_mask: true,
            ui_correction: false,
            output_view: 0,
            output_mix: 1.0,
            brightness: 1.0,
            contrast: 1.0,
            saturation: 1.0,
            post_per_pass: true,
            upscale: "vsr".into(),
            vsr_quality: 4,
            encoder: "h265_nvenc".into(),
            encoder_quality: 23,
            keep_audio: true,
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct MediaInfo {
    path: String,
    source_path: String,
    kind: String,
    width: u32,
    height: u32,
    frames: u32,
    fps: f64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct GpuInfo {
    name: String,
    runtime: String,
    detected: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DropPoll {
    active: bool,
    paths: Vec<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportProgress {
    active: bool,
    current: u32,
    total: u32,
    message: String,
}

struct Host {
    _library: Library,
    init: InitFn,
    create: CreateFn,
    process: ProcessFn,
    options: OptionsFn,
    resize: ResizeFn,
    shutdown: ShutdownFn,
    runtime: String,
    width: i32,
    height: i32,
    // 运动矢量与深度恒为零输入（无引擎数据），随会话按尺寸复用，避免逐帧分配 25MB
    motion: Vec<f32>,
    depth: Vec<f32>,
    output: Vec<u8>,
    pass_input: Vec<u8>,
    ready: bool,
}

impl Host {
    unsafe fn load(root: &Path, runtime: &str) -> Result<Self, String> {
        let library = Library::new(root.join("dlssnr_host.dll"))
            .map_err(|e| format!("无法加载 dlssnr_host.dll: {e}"))?;
        let init = *library
            .get::<InitFn>(b"dlssnr_init\0")
            .map_err(|e| e.to_string())?;
        let create = *library
            .get::<CreateFn>(b"dlssnr_create_feature\0")
            .map_err(|e| e.to_string())?;
        let process = *library
            .get::<ProcessFn>(b"dlssnr_process\0")
            .map_err(|e| e.to_string())?;
        let options = *library
            .get::<OptionsFn>(b"dlssnr_set_options\0")
            .map_err(|e| e.to_string())?;
        let resize = *library
            .get::<ResizeFn>(b"dlssnr_resize\0")
            .map_err(|e| e.to_string())?;
        let shutdown = *library
            .get::<ShutdownFn>(b"dlssnr_shutdown\0")
            .map_err(|e| e.to_string())?;
        Ok(Self {
            _library: library,
            init,
            create,
            process,
            options,
            resize,
            shutdown,
            runtime: runtime.into(),
            width: 0,
            height: 0,
            motion: Vec::new(),
            depth: Vec::new(),
            output: Vec::new(),
            pass_input: Vec::new(),
            ready: false,
        })
    }

    unsafe fn configure(&self, settings: &RenderSettings) {
        (self.options)(
            1,
            settings.style,
            settings.intensity,
            settings.local_tone,
            settings.local_struct,
            settings.skin_structure,
            i32::from(settings.use_auto_mask),
            i32::from(settings.ui_correction),
            0,
            2,
            1.0,
            1.0,
        );
    }

    unsafe fn ensure(
        &mut self,
        root: &Path,
        runtime: &str,
        width: i32,
        height: i32,
        settings: &RenderSettings,
    ) -> Result<(), String> {
        if self.ready && self.runtime != runtime {
            return Err("DLSS 会话已创建；切换 30/40/50 运行时后请重启应用。".into());
        }
        self.configure(settings);
        if !self.ready {
            let dll = runtime_path(root, runtime)?;
            let log = root.join("dlss_run.log");
            let dll_w = wide(&dll);
            let log_w = wide(&log);
            if (self.init)(width, height, 1, dll_w.as_ptr(), log_w.as_ptr()) == 0 {
                return Err("DLSS 初始化失败；请检查显卡、驱动与 dlss_run.log。".into());
            }
            if (self.create)(width, height, 1) == 0 {
                return Err("DLSS Feature 18 创建失败。此运行时或显卡可能不受支持。".into());
            }
            self.runtime = runtime.into();
            self.width = width;
            self.height = height;
            self.ready = true;
        } else if self.width != width || self.height != height {
            if (self.resize)(width, height, 1) == 0 {
                return Err("DLSS 尺寸切换失败。".into());
            }
            self.width = width;
            self.height = height;
        }
        let pixels = (width.max(0) as usize) * (height.max(0) as usize);
        if self.motion.len() != pixels * 2 {
            self.motion = vec![0_f32; pixels * 2];
            self.depth = vec![0_f32; pixels];
            self.output = vec![0_u8; pixels * 4];
        }
        Ok(())
    }

    unsafe fn render(
        &mut self,
        root: &Path,
        runtime: &str,
        input: &[u8],
        width: u32,
        height: u32,
        settings: &RenderSettings,
        reset: bool,
    ) -> Result<&[u8], String> {
        self.ensure(root, runtime, width as i32, height as i32, settings)?;
        if input.len() != self.output.len() {
            return Err("帧数据长度与 DLSS 会话不符。".into());
        }
        let pass_count = if settings.multi_pass {
            settings.pass_count
        } else {
            1
        };
        let process = self.process;
        let motion = self.motion.as_mut_ptr();
        let depth = self.depth.as_mut_ptr();
        passes::process_passes(
            input,
            &mut self.output,
            &mut self.pass_input,
            pass_count,
            reset,
            |source, destination, reset| {
                process(
                    source.as_ptr() as *mut u8,
                    motion,
                    depth,
                    destination.as_mut_ptr(),
                    i32::from(reset),
                ) != 0
            },
        )?;
        Ok(self.output.as_slice())
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        unsafe {
            (self.shutdown)();
        }
    }
}

// RTX VSR（RTX Video SDK，NGX Feature 16）：真正的网络超分，用于放大步骤
type VsInitFn = unsafe extern "C" fn(*const u16) -> i32;
type VsUpscaleFn = unsafe extern "C" fn(*const u8, i32, i32, *mut u8, i32, i32, i32) -> i32;
type VsShutdownFn = unsafe extern "C" fn();

struct VsrHost {
    _library: Library,
    upscale: VsUpscaleFn,
    shutdown: VsShutdownFn,
}

impl VsrHost {
    unsafe fn open(root: &Path) -> Result<Self, String> {
        let library = Library::new(root.join("rtx_vsr_host.dll"))
            .map_err(|e| format!("无法加载 rtx_vsr_host.dll: {e}"))?;
        let init = *library
            .get::<VsInitFn>(b"vsr_init\0")
            .map_err(|e| e.to_string())?;
        let upscale = *library
            .get::<VsUpscaleFn>(b"vsr_upscale\0")
            .map_err(|e| e.to_string())?;
        let shutdown = *library
            .get::<VsShutdownFn>(b"vsr_shutdown\0")
            .map_err(|e| e.to_string())?;
        let dir = wide(root);
        if init(dir.as_ptr()) == 0 {
            return Err(
                "RTX VSR 不可用：需要 RTX 20 系及以上显卡与 550+ 驱动，且 nvngx_vsr.dll 完整。"
                    .into(),
            );
        }
        Ok(Self {
            _library: library,
            upscale,
            shutdown,
        })
    }

    unsafe fn upscale(
        &self,
        image: &RgbaImage,
        tw: u32,
        th: u32,
        quality: i32,
    ) -> Option<RgbaImage> {
        let (w, h) = image.dimensions();
        let input = image.as_raw();
        let mut out = vec![0_u8; (tw as usize) * (th as usize) * 4];
        if (self.upscale)(
            input.as_ptr(),
            w as i32,
            h as i32,
            out.as_mut_ptr(),
            tw as i32,
            th as i32,
            quality,
        ) == 1
        {
            RgbaImage::from_raw(tw, th, out)
        } else {
            None
        }
    }
}

impl Drop for VsrHost {
    fn drop(&mut self) {
        unsafe {
            (self.shutdown)();
        }
    }
}

struct AppState {
    root: PathBuf,
    temp_dir: PathBuf,
    host: Mutex<Option<Host>>,
    vsr: Mutex<Option<VsrHost>>,
    vsr_disabled: AtomicBool,
    media: Mutex<HashMap<String, MediaInfo>>,
    decoder: Mutex<Option<VideoDecoder>>,
    temp_counter: AtomicU64,
    gpu: GpuInfo,
    drop_active: AtomicBool,
    pending_drop_paths: Mutex<Vec<PathBuf>>,
    export_progress: Mutex<ExportProgress>,
    export_paused: AtomicBool,
    export_cancel: AtomicBool,
    batch: Mutex<BatchProgress>,
}

struct VideoDecoder {
    path: String,
    width: u32,
    height: u32,
    max_side: u32,
    next_frame: u32,
    child: Child,
    stdout: ChildStdout,
    cache: VecDeque<(u32, Vec<u8>)>,
}

impl Drop for VideoDecoder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for AppState {
    fn drop(&mut self) {
        if let Ok(mut decoder) = self.decoder.lock() {
            drop(decoder.take());
        }
        let _ = std::fs::remove_dir_all(&self.temp_dir);
    }
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}
fn runtime_path(root: &Path, runtime: &str) -> Result<PathBuf, String> {
    let name = match runtime {
        "30" => "nvngx_dlssnr_30.dll",
        "40" => "nvngx_dlssnr_40.dll",
        _ => "nvngx_dlssnr.dll",
    };
    let current = std::env::current_dir().unwrap_or_default();
    let candidates = [
        root.join(name),
        root.parent()
            .map(|path| path.join(name))
            .unwrap_or_default(),
        current.join(name),
        current.join("_up_").join(name),
    ];
    candidates
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| {
            format!("缺少 RTX {runtime} 运行时：{name}。请确认安装目录中的运行时文件完整。")
        })
}

fn gpu_runtime(name: &str) -> Option<&'static str> {
    let name = name.to_ascii_uppercase();
    ["50", "40", "30"].into_iter().find(|generation| {
        name.contains(&format!("RTX {generation}")) || name.contains(&format!("RTX{generation}"))
    })
}

fn detect_gpu_info() -> GpuInfo {
    let output = Command::new("nvidia-smi")
        .args(["--query-gpu=name", "--format=csv,noheader,nounits"])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    let names: Vec<String> = output
        .ok()
        .filter(|output| output.status.success())
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let runtime = names
        .iter()
        .find_map(|name| gpu_runtime(name))
        .unwrap_or("50")
        .to_owned();
    let detected = names.iter().any(|name| gpu_runtime(name).is_some());
    GpuInfo {
        name: names
            .first()
            .cloned()
            .unwrap_or_else(|| "未检测到 NVIDIA GPU".into()),
        runtime,
        detected,
    }
}

fn begin_export_progress(state: &AppState, total: Option<u32>) -> Result<(), String> {
    let mut progress = state
        .export_progress
        .lock()
        .map_err(|_| "导出进度锁定失败")?;
    if progress.active {
        return Err("已有导出任务正在进行。".into());
    }
    *progress = ExportProgress {
        active: true,
        current: 0,
        total: total.unwrap_or(1).max(1),
        message: "准备导出…".into(),
    };
    state.export_paused.store(false, Ordering::Release);
    state.export_cancel.store(false, Ordering::Release);
    Ok(())
}

fn set_export_progress(state: &AppState, active: bool, current: u32, total: u32, message: String) {
    if let Ok(mut progress) = state.export_progress.lock() {
        progress.active = active;
        progress.current = current;
        progress.total = total.max(1);
        progress.message = message;
    }
}

fn finish_export_progress<T>(state: &AppState, result: &Result<T, String>, current: u32) {
    match result {
        Ok(_) => set_export_progress(state, false, current, current.max(1), "导出完成".into()),
        Err(error) => set_export_progress(
            state,
            false,
            current,
            current.max(1),
            format!("导出失败：{error}"),
        ),
    }
}

fn root_dir(app: &tauri::AppHandle) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_default();
    if cwd.join("dlssnr_host.dll").is_file() {
        cwd
    } else {
        app.path().resource_dir().unwrap_or(cwd)
    }
}
fn tool(name: &str) -> Result<Command, String> {
    let mut command = Command::new(name);
    command.creation_flags(CREATE_NO_WINDOW);
    Ok(command)
}
fn probe_u32(value: &serde_json::Value) -> Option<u32> {
    value
        .as_u64()
        .map(|v| v as u32)
        .or_else(|| value.as_str().and_then(|v| v.parse::<u32>().ok()))
}

fn probe_fps(stream: &serde_json::Value) -> f64 {
    for key in ["avg_frame_rate", "r_frame_rate"] {
        if let Some(rate) = stream[key].as_str() {
            let mut it = rate.split('/');
            let numerator = it.next().and_then(|v| v.parse::<f64>().ok());
            let denominator = it.next().and_then(|v| v.parse::<f64>().ok()).unwrap_or(1.0);
            if let Some(numerator) = numerator {
                if numerator.is_finite()
                    && denominator.is_finite()
                    && numerator > 0.0
                    && denominator > 0.0
                {
                    return numerator / denominator;
                }
            }
        }
    }
    30.0
}

fn probe_f64(value: &serde_json::Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|v| v.parse::<f64>().ok()))
        .filter(|v| v.is_finite() && *v > 0.0)
}

fn video_probe(path: &str) -> Result<(u32, u32, u32, f64), String> {
    let out = tool("ffprobe")?
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height,r_frame_rate,avg_frame_rate,nb_frames,duration:format=duration",
            "-of",
            "json",
            path,
        ])
        .output()
        .map_err(|e| format!("无法启动 FFprobe: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    let value =
        serde_json::from_slice::<serde_json::Value>(&out.stdout).map_err(|e| e.to_string())?;
    let stream = value["streams"].get(0).cloned().ok_or("没有视频流")?;
    let width = stream["width"].as_u64().ok_or("无效视频宽度")? as u32;
    let height = stream["height"].as_u64().ok_or("无效视频高度")? as u32;
    let fps = probe_fps(&stream);
    let duration =
        probe_f64(&stream["duration"]).or_else(|| probe_f64(&value["format"]["duration"]));
    let frames = probe_u32(&stream["nb_frames"])
        .or_else(|| duration.map(|seconds| (seconds * fps).round().max(1.0) as u32))
        .unwrap_or(1)
        .max(1);
    Ok((width, height, frames, fps))
}

fn preview_dimensions(width: u32, height: u32, max_side: u32) -> (u32, u32) {
    let max_side = max_side.clamp(320, MAX_PREVIEW_SIDE);
    if width.max(height) <= max_side {
        (width, height)
    } else {
        let scale = max_side as f64 / width.max(height) as f64;
        (
            (width as f64 * scale).round().max(1.0) as u32,
            (height as f64 * scale).round().max(1.0) as u32,
        )
    }
}

// 输出尺寸约束：32..=8192 且取偶（视频 yuv420p 编码要求），越界时按比例整体缩放
fn sanitize_output(width: Option<u32>, height: Option<u32>) -> Option<(u32, u32)> {
    let (width, height) = (width?, height?);
    if width == 0 || height == 0 {
        return None;
    }
    let (wf, hf) = (f64::from(width), f64::from(height));
    let scale = 1.0f64
        .min(8192.0 / wf)
        .min(8192.0 / hf)
        .max(32.0 / wf)
        .max(32.0 / hf);
    let w = ((wf * scale).round() as u32).clamp(32, 8192);
    let h = ((hf * scale).round() as u32).clamp(32, 8192);
    Some((w & !1, h & !1))
}

fn preview_target_dimensions_for_size(
    width: u32,
    height: u32,
    max_side: u32,
    target: Option<(u32, u32)>,
) -> (u32, u32) {
    target
        .map(|(w, h)| preview_dimensions(w, h, max_side))
        .unwrap_or_else(|| preview_dimensions(width, height, max_side))
}

fn preview_target_dimensions(
    image: &RgbaImage,
    max_side: u32,
    target: Option<(u32, u32)>,
) -> (u32, u32) {
    let (width, height) = image.dimensions();
    preview_target_dimensions_for_size(width, height, max_side, target)
}

// DLSS 侧预览帧：纯放大且启用 RTX VSR 时走 GPU 网络超分；
// 放大功能仅在 VSR 可用时开放，未启用 VSR 的放大请求保持在源分辨率
fn prepare_preview(
    state: &AppState,
    image: RgbaImage,
    max_side: u32,
    target: Option<(u32, u32)>,
    vsr: bool,
    quality: i32,
) -> RgbaImage {
    let (tw, th) = preview_target_dimensions(&image, max_side, target);
    let (w, h) = image.dimensions();
    if (tw, th) == (w, h) {
        return image;
    }
    if tw > w && th > h {
        return if vsr {
            upscale_frame(state, image, (tw, th), quality)
        } else {
            image
        };
    }
    image::imageops::resize(&image, tw, th, FilterType::CatmullRom)
}

// 原图侧预览帧：放大使用最近邻（诚实的像素放大，不添加任何信息），缩小用 CatmullRom
fn preview_original(image: RgbaImage, max_side: u32, target: Option<(u32, u32)>) -> RgbaImage {
    let (tw, th) = preview_target_dimensions(&image, max_side, target);
    let (w, h) = image.dimensions();
    if (tw, th) == (w, h) {
        return image;
    }
    if tw > w && th > h {
        image::imageops::resize(&image, tw, th, FilterType::Nearest)
    } else {
        image::imageops::resize(&image, tw, th, FilterType::CatmullRom)
    }
}

// 预览放大允许容错：VSR 单帧失败时回退最近邻，并销毁当前 VSR 会话以便下次重建。
fn upscale_frame(
    state: &AppState,
    image: RgbaImage,
    target: (u32, u32),
    quality: i32,
) -> RgbaImage {
    let (tw, th) = target;
    let (w, h) = image.dimensions();
    if (tw, th) == (w, h) {
        return image;
    }
    if !state.vsr_disabled.load(Ordering::Acquire) {
        if let Ok(mut slot) = state.vsr.lock() {
            if slot.is_none() {
                match unsafe { VsrHost::open(&state.root) } {
                    Ok(host) => *slot = Some(host),
                    Err(error) => {
                        state.vsr_disabled.store(true, Ordering::Release);
                        eprintln!("[DLSS5] {error}");
                    }
                }
            }
            if let Some(host) = slot.as_ref() {
                if let Some(upscaled) = unsafe { host.upscale(&image, tw, th, quality) } {
                    return upscaled;
                }
                eprintln!("[DLSS5] RTX VSR 放大失败，本次预览回退最近邻");
                drop(slot.take());
            }
        }
    }
    image::imageops::resize(&image, tw, th, FilterType::Nearest)
}

// 正式导出使用严格 VSR：任何初始化/推理失败都返回错误，不允许静默退化为普通插值。
fn upscale_frame_strict(
    state: &AppState,
    image: RgbaImage,
    target: (u32, u32),
    quality: i32,
) -> Result<RgbaImage, String> {
    let (tw, th) = target;
    let (w, h) = image.dimensions();
    if (tw, th) == (w, h) {
        return Ok(image);
    }
    if state.vsr_disabled.load(Ordering::Acquire) {
        return Err("RTX VSR 当前不可用，已取消导出以避免静默使用普通插值。".into());
    }
    let mut slot = state.vsr.lock().map_err(|_| "VSR 会话锁定失败")?;
    if slot.is_none() {
        match unsafe { VsrHost::open(&state.root) } {
            Ok(host) => *slot = Some(host),
            Err(error) => {
                state.vsr_disabled.store(true, Ordering::Release);
                return Err(error);
            }
        }
    }
    let result = slot
        .as_ref()
        .and_then(|host| unsafe { host.upscale(&image, tw, th, quality) });
    match result {
        Some(upscaled) => Ok(upscaled),
        None => {
            drop(slot.take());
            Err(format!(
                "RTX VSR 放大失败（{w}×{h} → {tw}×{th}，质量 {quality}）；导出已停止，没有回退为最近邻。"
            ))
        }
    }
}

// 导出用重采样：放大必须由 RTX VSR 完成；缩小使用 Lanczos3。
fn resize_to_target_strict(
    state: &AppState,
    image: RgbaImage,
    target: Option<(u32, u32)>,
    vsr: bool,
    quality: i32,
) -> Result<RgbaImage, String> {
    let (w, h) = image.dimensions();
    match target {
        Some((tw, th)) if (tw, th) != (w, h) => {
            if tw > w && th > h {
                if vsr {
                    upscale_frame_strict(state, image, (tw, th), quality)
                } else {
                    Err("请求的输出尺寸大于源分辨率，但 RTX VSR 未启用。".into())
                }
            } else {
                Ok(image::imageops::resize(
                    &image,
                    tw,
                    th,
                    FilterType::Lanczos3,
                ))
            }
        }
        _ => Ok(image),
    }
}

fn active_pass_count(settings: &RenderSettings) -> i32 {
    if settings.multi_pass {
        settings.pass_count.clamp(1, 5)
    } else {
        1
    }
}

fn should_defer_vsr_until_after_first_pass(
    settings: &RenderSettings,
    source: (u32, u32),
    target: Option<(u32, u32)>,
) -> bool {
    matches!(target, Some((tw, th)) if settings.upscale == "vsr" && active_pass_count(settings) > 1 && tw > source.0 && th > source.1)
}

fn render_dlss_frame(
    state: &AppState,
    runtime: &str,
    image: RgbaImage,
    settings: &RenderSettings,
    reset: bool,
) -> Result<RgbaImage, String> {
    let (w, h) = image.dimensions();
    let input = image.into_raw();
    let output = {
        let mut host = state.host.lock().map_err(|_| "DLSS 会话锁定失败")?;
        if host.is_none() {
            *host = Some(unsafe { Host::load(&state.root, runtime)? });
        }
        unsafe {
            host.as_mut()
                .unwrap()
                .render(&state.root, runtime, &input, w, h, settings, reset)?
                .to_vec()
        }
    };
    RgbaImage::from_raw(w, h, output).ok_or("无效 DLSS 输出".into())
}

/// 亮度/对比度/饱和度后处理：对 RGBA8 图像做逐像素调整。
fn post_process(image: &mut RgbaImage, brightness: f32, contrast: f32, saturation: f32) {
    // 三个参数均为默认值时跳过逐像素处理，避免不必要的开销
    if (brightness - 1.0).abs() < f32::EPSILON
        && (contrast - 1.0).abs() < f32::EPSILON
        && (saturation - 1.0).abs() < f32::EPSILON
    {
        return;
    }
    let bo = brightness - 1.0;
    for px in image.pixels_mut() {
        let ch = &mut px.0;
        let luma = 0.299 * ch[0] as f32 + 0.587 * ch[1] as f32 + 0.114 * ch[2] as f32;
        for c in 0..3 {
            let mut v = ch[c] as f32;
            v = luma + (v - luma) * saturation;
            v = (v - 128.0) * contrast + 128.0;
            v += bo * 255.0;
            ch[c] = v.clamp(0.0, 255.0) as u8;
        }
    }
}

fn render_with_pass_order(
    state: &AppState,
    runtime: &str,
    image: RgbaImage,
    settings: &RenderSettings,
    reset: bool,
    post_first_vsr_target: Option<(u32, u32)>,
    strict_vsr: bool,
) -> Result<RgbaImage, String> {
    let pass_count = active_pass_count(settings);
    let post = |img: &mut RgbaImage| {
        post_process(
            img,
            settings.brightness,
            settings.contrast,
            settings.saturation,
        );
    };
    let Some(target) = post_first_vsr_target else {
        let mut result = render_dlss_frame(state, runtime, image, settings, reset)?;
        post(&mut result);
        return Ok(result);
    };
    if pass_count == 1 {
        let mut result = render_dlss_frame(state, runtime, image, settings, reset)?;
        post(&mut result);
        return Ok(result);
    }

    let mut first_pass = settings.clone();
    first_pass.multi_pass = false;
    first_pass.pass_count = 1;
    let mut first_output = render_dlss_frame(state, runtime, image, &first_pass, true)?;
    if settings.post_per_pass {
        post(&mut first_output);
    }
    let upscaled = if strict_vsr {
        upscale_frame_strict(state, first_output, target, settings.vsr_quality)?
    } else {
        upscale_frame(state, first_output, target, settings.vsr_quality)
    };
    let remaining = pass_count - 1;
    let mut remaining_passes = settings.clone();
    remaining_passes.multi_pass = remaining > 1;
    remaining_passes.pass_count = remaining;
    let mut result = render_dlss_frame(state, runtime, upscaled, &remaining_passes, true)?;
    // 末轮始终应用：post_per_pass 关闭时仅在这里应用一次
    post(&mut result);
    Ok(result)
}

fn spawn_decoder(
    info: &MediaInfo,
    width: u32,
    height: u32,
    max_side: u32,
    start_frame: u32,
) -> Result<VideoDecoder, String> {
    let mut command = tool("ffmpeg")?;
    command.args(["-v", "error"]);
    if start_frame > 0 {
        command.args([
            "-ss",
            &(start_frame as f64 / info.fps.max(0.001)).to_string(),
        ]);
    }
    let mut child = command
        .args([
            "-i",
            &info.path,
            "-an",
            "-vf",
            &format!("scale={width}:{height}:flags=fast_bilinear"),
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("无法启动 FFmpeg 预览解码器: {e}"))?;
    let stdout = child.stdout.take().ok_or("无法读取视频预览数据")?;
    Ok(VideoDecoder {
        path: info.path.clone(),
        width,
        height,
        max_side,
        next_frame: start_frame,
        child,
        stdout,
        cache: VecDeque::new(),
    })
}

fn decode_video_frame(
    state: &AppState,
    path: &str,
    frame: u32,
    max_side: u32,
    target: Option<(u32, u32)>,
    vsr_mode: bool,
) -> Result<(Vec<u8>, u32, u32), String> {
    let info = state
        .media
        .lock()
        .map_err(|_| "媒体信息缓存锁定失败")?
        .get(path)
        .cloned()
        .or_else(|| {
            video_probe(path)
                .ok()
                .map(|(width, height, frames, fps)| MediaInfo {
                    path: path.into(),
                    source_path: path.into(),
                    kind: "video".into(),
                    width,
                    height,
                    frames,
                    fps,
                })
        })
        .ok_or("无法读取视频信息")?;
    let frame = frame.min(info.frames.saturating_sub(1));
    let (pw, ph) = target
        .map(|(w, h)| preview_dimensions(w, h, max_side))
        .unwrap_or_else(|| preview_dimensions(info.width, info.height, max_side));
    // VSR 模式且预览目标大于源时，解码保持原始分辨率，把放大交给 GPU
    let bypass = vsr_mode && pw > info.width && ph > info.height;
    let (target_w, target_h) = if bypass {
        (info.width, info.height)
    } else {
        (pw, ph)
    };
    let mut slot = state.decoder.lock().map_err(|_| "视频解码器锁定失败")?;
    let reusable = slot.as_ref().is_some_and(|decoder| {
        decoder.path == path
            && decoder.max_side == max_side
            && decoder.width == target_w
            && decoder.height == target_h
    });
    if !reusable {
        drop(slot.take());
    }
    if let Some(decoder) = slot.as_ref() {
        if let Some((_, bytes)) = decoder.cache.iter().find(|(cached, _)| *cached == frame) {
            return Ok((bytes.clone(), decoder.width, decoder.height));
        }
    }
    let restart = slot.as_ref().is_some_and(|decoder| {
        frame < decoder.next_frame
            || frame > decoder.next_frame.saturating_add(SEQUENTIAL_DECODE_LIMIT)
    });
    if restart {
        drop(slot.take());
    }
    if slot.is_none() {
        let start_frame = if frame > SEQUENTIAL_DECODE_LIMIT {
            frame
        } else {
            0
        };
        *slot = Some(spawn_decoder(
            &info,
            target_w,
            target_h,
            max_side,
            start_frame,
        )?);
    }
    let decoder = slot.as_mut().unwrap();
    let frame_bytes = (decoder.width * decoder.height * 4) as usize;
    while decoder.next_frame <= frame {
        let current = decoder.next_frame;
        let mut bytes = vec![0_u8; frame_bytes];
        decoder
            .stdout
            .read_exact(&mut bytes)
            .map_err(|_| format!("FFmpeg 未返回完整的第 {current} 帧"))?;
        decoder.cache.push_back((current, bytes));
        while decoder.cache.len() > PREVIEW_CACHE_FRAMES {
            decoder.cache.pop_front();
        }
        decoder.next_frame += 1;
    }
    let bytes = decoder
        .cache
        .iter()
        .find(|(cached, _)| *cached == frame)
        .map(|(_, bytes)| bytes.clone())
        .ok_or("视频帧未进入预览缓存")?;
    Ok((bytes, decoder.width, decoder.height))
}

fn bake_gif(state: &AppState, source: &str) -> Result<String, String> {
    let id = state.temp_counter.fetch_add(1, Ordering::Relaxed);
    let destination = state.temp_dir.join(format!("gif_{id}.mp4"));
    let status = tool("ffmpeg")?
        .args([
            "-y",
            "-v",
            "error",
            "-i",
            source,
            "-an",
            "-vf",
            "pad=ceil(iw/2)*2:ceil(ih/2)*2",
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-crf",
            "26",
            "-pix_fmt",
            "yuv420p",
            "-movflags",
            "+faststart",
        ])
        .arg(&destination)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("无法启动 GIF 烘焙: {e}"))?;
    if !status.status.success() {
        return Err(format!(
            "GIF 烘焙失败: {}",
            String::from_utf8_lossy(&status.stderr).trim()
        ));
    }
    Ok(destination.to_string_lossy().into_owned())
}

fn rgba_png(bytes: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    PngEncoder::new(&mut out)
        .write_image(bytes, width, height, ExtendedColorType::Rgba8)
        .map_err(|e| e.to_string())?;
    Ok(out)
}
fn image_data_uri(data: &str) -> Result<RgbaImage, String> {
    let encoded = data.split_once(',').ok_or("无效图片数据")?.1;
    let bytes = STANDARD.decode(encoded).map_err(|e| e.to_string())?;
    Ok(image::load_from_memory(&bytes)
        .map_err(|e| e.to_string())?
        .to_rgba8())
}
fn process_rgba(
    state: &AppState,
    image: RgbaImage,
    runtime: String,
    settings: RenderSettings,
    max_side: u32,
    target: Option<(u32, u32)>,
) -> Result<Vec<u8>, String> {
    let vsr_target = preview_target_dimensions(&image, max_side, target);
    let defer_vsr =
        should_defer_vsr_until_after_first_pass(&settings, image.dimensions(), Some(vsr_target));
    let image = prepare_preview(
        state,
        image,
        max_side,
        target,
        settings.upscale == "vsr" && !defer_vsr,
        settings.vsr_quality,
    );
    let rendered = render_with_pass_order(
        state,
        &runtime,
        image,
        &settings,
        true,
        defer_vsr.then_some(vsr_target),
        false,
    )?;
    let (w, h) = rendered.dimensions();
    rgba_png(&rendered.into_raw(), w, h)
}

#[tauri::command]
fn choose_media() -> Option<String> {
    rfd::FileDialog::new()
        .add_filter(
            "媒体",
            &[
                "png", "jpg", "jpeg", "bmp", "webp", "tif", "tiff", "gif", "mp4", "avi", "mov",
                "mkv", "webm", "wmv", "m4v",
            ],
        )
        .pick_file()
        .map(|p| p.to_string_lossy().into_owned())
}

#[tauri::command]
fn choose_export(video: bool) -> Option<String> {
    let dialog = rfd::FileDialog::new();
    let selected = if video {
        dialog
            .add_filter("MP4 视频", &["mp4"])
            .set_file_name("dlss_output.mp4")
            .save_file()
    } else {
        dialog
            .add_filter("PNG 图片", &["png"])
            .set_file_name("dlss_output.png")
            .save_file()
    };
    selected.map(|p| p.to_string_lossy().into_owned())
}

#[tauri::command]
fn gpu_info(state: tauri::State<'_, AppState>) -> GpuInfo {
    state.gpu.clone()
}

// 探测 RTX VSR 是否可用；成功时宿主保持已创建状态供后续放大调用
#[tauri::command]
fn vsr_probe(state: tauri::State<'_, AppState>) -> Result<bool, String> {
    if state.vsr_disabled.load(Ordering::Acquire) {
        return Ok(false);
    }
    let mut slot = state.vsr.lock().map_err(|_| "VSR 会话锁定失败")?;
    if slot.is_none() {
        match unsafe { VsrHost::open(&state.root) } {
            Ok(host) => *slot = Some(host),
            Err(error) => {
                state.vsr_disabled.store(true, Ordering::Release);
                return Err(error);
            }
        }
    }
    Ok(slot.is_some())
}

#[tauri::command]
fn poll_drop(state: tauri::State<'_, AppState>) -> Result<DropPoll, String> {
    let mut pending = state
        .pending_drop_paths
        .lock()
        .map_err(|_| "拖放路径缓存锁定失败")?;
    let paths = pending
        .drain(..)
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
    Ok(DropPoll {
        active: state.drop_active.load(Ordering::Acquire),
        paths,
    })
}

#[tauri::command]
fn poll_export_progress(state: tauri::State<'_, AppState>) -> Result<ExportProgress, String> {
    state
        .export_progress
        .lock()
        .map_err(|_| "导出进度锁定失败".into())
        .map(|progress| progress.clone())
}

#[tauri::command]
fn export_set_paused(state: tauri::State<'_, AppState>, paused: bool) -> Result<(), String> {
    state.export_paused.store(paused, Ordering::Release);
    if let Ok(mut progress) = state.export_progress.lock() {
        if progress.active {
            progress.message = if paused {
                "已暂停".into()
            } else {
                "正在继续导出…".into()
            };
        }
    }
    Ok(())
}

#[tauri::command]
fn export_cancel(state: tauri::State<'_, AppState>) -> Result<(), String> {
    state.export_cancel.store(true, Ordering::Release);
    state.export_paused.store(false, Ordering::Release);
    Ok(())
}

#[tauri::command]
async fn media_info(state: tauri::State<'_, AppState>, path: String) -> Result<MediaInfo, String> {
    let source_path = path;
    let path_buf = PathBuf::from(&source_path);
    let ext = path_buf
        .extension()
        .and_then(OsStr::to_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    if let Ok(mut decoder) = state.decoder.lock() {
        drop(decoder.take());
    }
    if ["png", "jpg", "jpeg", "bmp", "webp", "tif", "tiff"].contains(&ext.as_str()) {
        let (width, height) =
            image::image_dimensions(&path_buf).map_err(|e| format!("无法读取图片: {e}"))?;
        return Ok(MediaInfo {
            path: source_path.clone(),
            source_path,
            kind: "image".into(),
            width,
            height,
            frames: 1,
            fps: 1.0,
        });
    }
    let playback_path = if ext == "gif" {
        bake_gif(&state, &source_path)?
    } else {
        source_path.clone()
    };
    let (width, height, frames, fps) = video_probe(&playback_path)?;
    let info = MediaInfo {
        path: playback_path.clone(),
        source_path,
        kind: "video".into(),
        width,
        height,
        frames,
        fps,
    };
    state
        .media
        .lock()
        .map_err(|_| "媒体信息缓存锁定失败")?
        .insert(playback_path, info.clone());
    Ok(info)
}

#[tauri::command]
async fn process_image(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    path: String,
    runtime: String,
    settings: RenderSettings,
    max_side: u32,
    output_width: Option<u32>,
    output_height: Option<u32>,
) -> Result<Response, String> {
    let image = image::open(&path)
        .map_err(|e| format!("无法读取图片: {e}"))?
        .to_rgba8();
    let _ = app;
    Ok(Response::new(process_rgba(
        &state,
        image,
        runtime,
        settings,
        max_side,
        sanitize_output(output_width, output_height),
    )?))
}

#[tauri::command]
async fn process_image_data(
    state: tauri::State<'_, AppState>,
    data: String,
    runtime: String,
    settings: RenderSettings,
    max_side: u32,
    output_width: Option<u32>,
    output_height: Option<u32>,
) -> Result<Response, String> {
    Ok(Response::new(process_rgba(
        &state,
        image_data_uri(&data)?,
        runtime,
        settings,
        max_side,
        sanitize_output(output_width, output_height),
    )?))
}

#[tauri::command]
async fn save_data_png(
    state: tauri::State<'_, AppState>,
    data: String,
    destination: String,
    output_width: Option<u32>,
    output_height: Option<u32>,
    upscale: Option<String>,
    vsr_quality: Option<i32>,
) -> Result<(), String> {
    begin_export_progress(&state, Some(1))?;
    let vsr = upscale.as_deref() == Some("vsr");
    let quality = vsr_quality.unwrap_or(4);
    let result = image_data_uri(&data).and_then(|image| {
        let image = resize_to_target_strict(
            &state,
            image,
            sanitize_output(output_width, output_height),
            vsr,
            quality,
        )?;
        image.save(&destination).map_err(|e| e.to_string())
    });
    finish_export_progress(&state, &result, u32::from(result.is_ok()));
    result
}

#[tauri::command]
async fn read_image_data(
    path: String,
    max_side: u32,
    output_width: Option<u32>,
    output_height: Option<u32>,
) -> Result<Response, String> {
    let image = image::open(path)
        .map_err(|e| format!("无法读取图片: {e}"))?
        .to_rgba8();
    let image = preview_original(
        image,
        max_side,
        sanitize_output(output_width, output_height),
    );
    let (w, h) = image.dimensions();
    Ok(Response::new(rgba_png(&image.into_raw(), w, h)?))
}

#[tauri::command]
async fn save_png(
    state: tauri::State<'_, AppState>,
    path: String,
    destination: String,
    runtime: String,
    settings: RenderSettings,
    output_width: Option<u32>,
    output_height: Option<u32>,
) -> Result<(), String> {
    begin_export_progress(&state, Some(1))?;
    let result: Result<(), String> = (|| {
        let image = image::open(&path)
            .map_err(|e| format!("无法读取图片: {e}"))?
            .to_rgba8();
        let target = sanitize_output(output_width, output_height);
        let defer_vsr =
            should_defer_vsr_until_after_first_pass(&settings, image.dimensions(), target);
        let image = if defer_vsr {
            image
        } else {
            resize_to_target_strict(
                &state,
                image,
                target,
                settings.upscale == "vsr",
                settings.vsr_quality,
            )?
        };
        let post_first_vsr_target = if defer_vsr { target } else { None };
        let rendered = render_with_pass_order(
            &state,
            &runtime,
            image,
            &settings,
            true,
            post_first_vsr_target,
            true,
        )?;
        rendered
            .save(&destination)
            .map_err(|e| format!("无法写入 PNG: {e}"))?;
        Ok(())
    })();
    finish_export_progress(&state, &result, u32::from(result.is_ok()));
    result
}

// 解码后按需把帧升到预览目标尺寸：原图侧（nearest）用最近邻，单 Pass 的 DLSS 侧走 RTX VSR；
// 多重 Pass 则保留源分辨率，交由调用方在第 1 次 DLSS 后执行 VSR。
#[allow(clippy::too_many_arguments)]
fn decoded_preview_frame(
    state: &AppState,
    path: &str,
    frame: u32,
    max_side: u32,
    target: Option<(u32, u32)>,
    vsr: bool,
    defer_vsr: bool,
    nearest: bool,
    quality: i32,
) -> Result<RgbaImage, String> {
    let (bytes, dw, dh) = decode_video_frame(
        &state,
        path,
        frame,
        max_side,
        target,
        vsr || defer_vsr || nearest,
    )?;
    let image = RgbaImage::from_raw(dw, dh, bytes).ok_or("无效视频帧")?;
    let (pw, ph) = preview_target_dimensions_for_size(dw, dh, max_side, target);
    if (dw, dh) == (pw, ph) {
        return Ok(image);
    }
    if pw > dw && ph > dh {
        if nearest {
            Ok(image::imageops::resize(&image, pw, ph, FilterType::Nearest))
        } else if defer_vsr {
            Ok(image)
        } else {
            Ok(upscale_frame(state, image, (pw, ph), quality))
        }
    } else {
        Ok(image::imageops::resize(
            &image,
            pw,
            ph,
            FilterType::CatmullRom,
        ))
    }
}

#[tauri::command]
async fn frame_png(
    state: tauri::State<'_, AppState>,
    path: String,
    frame: u32,
    max_side: u32,
    output_width: Option<u32>,
    output_height: Option<u32>,
) -> Result<Response, String> {
    let image = decoded_preview_frame(
        &state,
        &path,
        frame,
        max_side,
        sanitize_output(output_width, output_height),
        false,
        false,
        true,
        0,
    )?;
    let (w, h) = image.dimensions();
    Ok(Response::new(rgba_png(&image.into_raw(), w, h)?))
}

#[tauri::command]
async fn render_frame_png(
    state: tauri::State<'_, AppState>,
    path: String,
    frame: u32,
    runtime: String,
    settings: RenderSettings,
    max_side: u32,
    output_width: Option<u32>,
    output_height: Option<u32>,
) -> Result<Response, String> {
    let target = sanitize_output(output_width, output_height);
    let (source_w, source_h) = state
        .media
        .lock()
        .map_err(|_| "媒体信息锁定失败")?
        .get(&path)
        .map(|info| (info.width, info.height))
        .ok_or("媒体尚未载入")?;
    let vsr_target = preview_target_dimensions_for_size(source_w, source_h, max_side, target);
    let defer_vsr =
        should_defer_vsr_until_after_first_pass(&settings, (source_w, source_h), Some(vsr_target));
    let image = decoded_preview_frame(
        &state,
        &path,
        frame,
        max_side,
        target,
        settings.upscale == "vsr",
        defer_vsr,
        false,
        settings.vsr_quality,
    )?;
    let rendered = render_with_pass_order(
        &state,
        &runtime,
        image,
        &settings,
        true,
        defer_vsr.then_some(vsr_target),
        false,
    )?;
    let (w, h) = rendered.dimensions();
    Ok(Response::new(rgba_png(&rendered.into_raw(), w, h)?))
}

// ffmpeg stderr 留尾：去掉空字节，最多保留末尾 400 字符
fn recent_text(mut bytes: Vec<u8>) -> String {
    bytes.retain(|b| *b != 0);
    let text = String::from_utf8_lossy(&bytes).trim().to_string();
    let total = text.chars().count();
    if total > 400 {
        format!("…{}", text.chars().skip(total - 400).collect::<String>())
    } else {
        text
    }
}

// 预检 NVENC 编码器是否可用（驱动/并发限制），失败时调用方回退 CPU 编码
fn nvenc_available(encoder: &str) -> bool {
    let Ok(mut command) = tool("ffmpeg") else {
        return false;
    };
    command
        .args([
            "-hide_banner",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "nullsrc=s=320x180:d=0.2",
            "-frames:v",
            "5",
            "-c:v",
            encoder,
            "-preset",
            "p5",
            "-rc",
            "vbr",
            "-cq",
            "23",
            "-b:v",
            "0",
            "-f",
            "null",
            "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

// 解析编码器设置：NVENC 不可用时回退对应的 CPU 编码器，返回 (ffmpeg 编码器名, 是否硬件)
fn resolve_encoder(requested: &str) -> (&'static str, bool) {
    match requested {
        "h265_nvenc" => {
            if nvenc_available("hevc_nvenc") {
                ("hevc_nvenc", true)
            } else {
                ("libx265", false)
            }
        }
        "h264_x264" => ("libx264", false),
        "h265_x265" => ("libx265", false),
        _ => {
            if nvenc_available("h264_nvenc") {
                ("h264_nvenc", true)
            } else {
                ("libx264", false)
            }
        }
    }
}

const BATCH_CANCELLED: &str = "已取消";

// 批量处理：队列中单个任务的进度（前端经 batch_state 轮询渲染）
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchJobProgress {
    name: String,
    status: String,
    current: u32,
    frames: u32,
    fps: f64,
    error: String,
}

#[derive(Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct BatchProgress {
    running: bool,
    cancelled: bool,
    jobs: Vec<BatchJobProgress>,
}

fn unique_destination(dir: &std::path::Path, stem: &str) -> std::path::PathBuf {
    let mut candidate = dir.join(format!("{stem}_dlss.mp4"));
    let mut n = 1;
    while candidate.exists() {
        candidate = dir.join(format!("{stem}_dlss_{n}.mp4"));
        n += 1;
    }
    candidate
}

// 单个视频的完整导出管线，单个导出与批量处理共用。
// on_progress 汇报 (当前帧, 总帧数, 状态描述)；is_cancelled 返回 true 时中止，
// 删除半成品文件并返回 Err(BATCH_CANCELLED)。
#[allow(clippy::too_many_arguments)]
fn export_one_video(
    state: &AppState,
    path: &str,
    destination: &str,
    runtime: &str,
    settings: &RenderSettings,
    output_width: Option<u32>,
    output_height: Option<u32>,
    on_progress: &mut dyn FnMut(u32, u32, String),
    is_cancelled: &dyn Fn() -> bool,
) -> Result<u32, String> {
    let (source_w, source_h, total_frames, fps) = video_probe(path)?;
    let vsr_mode = settings.upscale == "vsr";
    let (mut w, mut h) =
        sanitize_output(output_width, output_height).unwrap_or((source_w, source_h));
    if !(vsr_mode && w > source_w && h > source_h) {
        // 放大功能整体依赖 RTX VSR：未启用时导出不超过源分辨率
        w = w.min(source_w);
        h = h.min(source_h);
    }
    // VSR 模式且在放大时：解码保持源分辨率，放大交给 GPU，其余交给 FFmpeg scale
    let bypass = vsr_mode && w > source_w && h > source_h;
    let defer_vsr = bypass && active_pass_count(&settings) > 1;
    let (decode_w, decode_h) = if bypass { (source_w, source_h) } else { (w, h) };
    let quality = settings.encoder_quality.clamp(0, 51);
    let quality_s = quality.to_string();
    let (encoder, hw) = resolve_encoder(&settings.encoder);
    // NVENC 分辨率上限：H.264 4096×4096，H.265 8192×8192
    if hw && encoder == "h264_nvenc" && (w > 4096 || h > 4096) {
        return Err(format!(
            "H.264 (NVENC) 最高支持 4096×4096，当前输出 {w}×{h}；请在编码器设置改用 H.265 或降低输出尺寸。"
        ));
    }
    if hw && encoder == "hevc_nvenc" && (w > 8192 || h > 8192) {
        return Err(format!(
            "H.265 (NVENC) 最高支持 8192×8192，当前输出 {w}×{h}。"
        ));
    }
    let label = match encoder {
        "h264_nvenc" => "H.264 NVENC".to_string(),
        "hevc_nvenc" => "H.265 NVENC".to_string(),
        "libx264" => "H.264 x264 (CPU)".to_string(),
        _ => "H.265 x265 (CPU)".to_string(),
    };
    on_progress(0, total_frames, format!("使用 {label}，正在导出 {w}×{h}…"));
    let result: Result<u32, String> = (|| {
        let frame_bytes = (decode_w * decode_h * 4) as usize;
        let scale_filter = format!("scale={w}:{h}:flags=lanczos");
        let mut command = tool("ffmpeg")?;
        command.args(["-v", "error", "-i", &path]);
        if !bypass && (w, h) != (source_w, source_h) {
            command.args(["-vf", &scale_filter]);
        }
        let mut decode = command
            .args(["-f", "rawvideo", "-pix_fmt", "rgba", "-"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("无法启动 FFmpeg 解码器: {e}"))?;
        // 音轨按设置附带（统一转 AAC 写入 MP4），关闭"保持音频"时输出纯视频。
        let mut encode = tool("ffmpeg")?
            .args([
                "-y",
                "-v",
                "error",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgba",
                "-s",
                &format!("{w}x{h}"),
                "-r",
                &fps.to_string(),
                "-i",
                "-",
                "-i",
                &path,
                "-map",
                "0:v",
            ])
            .args(if settings.keep_audio {
                vec!["-map", "1:a?", "-c:a", "aac", "-b:a", "192k", "-shortest"]
            } else {
                Vec::new()
            })
            .args(["-pix_fmt", "yuv420p"])
            .args(if hw {
                vec![
                    "-c:v",
                    encoder,
                    "-preset",
                    "p5",
                    "-rc",
                    "vbr",
                    "-cq",
                    quality_s.as_str(),
                    "-b:v",
                    "0",
                ]
            } else {
                vec![
                    "-c:v",
                    encoder,
                    "-preset",
                    "veryfast",
                    "-crf",
                    quality_s.as_str(),
                ]
            })
            .arg(&destination)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("无法启动 FFmpeg 编码器: {e}"))?;
        let mut reader = decode.stdout.take().ok_or("无法读取视频数据")?;
        let mut writer = encode.stdin.take().ok_or("无法写入视频数据")?;
        // 两个 ffmpeg 的 stderr 由线程收集，失败时附带在错误信息里便于定位
        let decode_stderr = decode.stderr.take();
        let decode_stderr_thread = std::thread::spawn(move || {
            let mut buffer = Vec::new();
            if let Some(mut stderr) = decode_stderr {
                let _ = stderr.read_to_end(&mut buffer);
            }
            buffer
        });
        let encode_stderr = encode.stderr.take();
        let encode_stderr_thread = std::thread::spawn(move || {
            let mut buffer = Vec::new();
            if let Some(mut stderr) = encode_stderr {
                let _ = stderr.read_to_end(&mut buffer);
            }
            buffer
        });
        // 预读线程：GPU 推理当前帧的同时预取下一帧，解除读帧与渲染的串行
        let (tx, rx) = std::sync::mpsc::sync_channel::<Result<Vec<u8>, String>>(3);
        let reader_thread = std::thread::spawn(move || {
            let mut buffer = vec![0_u8; frame_bytes];
            loop {
                let mut read = 0;
                while read < buffer.len() {
                    match reader.read(&mut buffer[read..]) {
                        Ok(0) => break,
                        Ok(n) => read += n,
                        Err(e) => {
                            let _ = tx.send(Err(e.to_string()));
                            return;
                        }
                    }
                }
                if read == 0 {
                    return;
                }
                if read != buffer.len() {
                    let _ = tx.send(Err("视频流末尾帧不完整。".into()));
                    return;
                }
                if tx.send(Ok(buffer.clone())).is_err() {
                    return;
                }
            }
        });
        let mut count = 0;
        let mut cancelled = false;
        let mut outcome: Result<u32, String> = Ok(0);
        for message in rx {
            if is_cancelled() {
                cancelled = true;
                outcome = Err(BATCH_CANCELLED.into());
                break;
            }
            // 暂停标志为单个导出专用；批量处理期间不会被置位
            while state.export_paused.load(Ordering::Acquire) && !is_cancelled() {
                std::thread::sleep(std::time::Duration::from_millis(120));
            }
            if is_cancelled() {
                cancelled = true;
                outcome = Err(BATCH_CANCELLED.into());
                break;
            }
            let frame = match message {
                Ok(frame) => frame,
                Err(e) => {
                    outcome = Err(e);
                    break;
                }
            };
            let image = match RgbaImage::from_raw(decode_w, decode_h, frame) {
                Some(image) => image,
                None => {
                    outcome = Err("无效视频帧".into());
                    break;
                }
            };
            let image = if bypass && !defer_vsr {
                match upscale_frame_strict(state, image, (w, h), settings.vsr_quality) {
                    Ok(upscaled) => upscaled,
                    Err(error) => {
                        outcome = Err(error);
                        break;
                    }
                }
            } else {
                image
            };
            let rendered = match render_with_pass_order(
                state,
                runtime,
                image,
                settings,
                count == 0,
                defer_vsr.then_some((w, h)),
                true,
            ) {
                Ok(rendered) => rendered,
                Err(error) => {
                    outcome = Err(error);
                    break;
                }
            };
            if let Err(e) = writer.write_all(&rendered.into_raw()) {
                outcome = Err(format!("视频编码写入失败: {e}"));
                break;
            }
            count += 1;
            on_progress(
                count,
                total_frames,
                format!("正在导出第 {count} / {total_frames} 帧"),
            );
        }
        let _ = reader_thread.join();
        drop(writer);
        if outcome.is_err() {
            let _ = decode.kill();
            let _ = encode.kill();
        }
        let encode_tail = recent_text(encode_stderr_thread.join().unwrap_or_default());
        if !decode.wait().map_err(|e| e.to_string())?.success() && outcome.is_ok() {
            let decode_tail = recent_text(decode_stderr_thread.join().unwrap_or_default());
            outcome = Err(format!("FFmpeg 解码失败。{decode_tail}"));
        }
        if !encode.wait().map_err(|e| e.to_string())?.success() && outcome.is_ok() {
            outcome = Err(format!("FFmpeg 编码失败。{encode_tail}"));
        }
        if let Err(message) = outcome.as_mut() {
            if !cancelled && !encode_tail.is_empty() && !message.contains("编码器输出") {
                message.push_str(&format!("；编码器输出: {encode_tail}"));
            }
        }
        if outcome.is_ok() {
            outcome = Ok(count);
        }
        if cancelled {
            let _ = std::fs::remove_file(destination);
        }
        outcome
    })();
    result
}

// 单个视频导出：沿用全局进度条与暂停/取消标志
#[tauri::command]
async fn export_video(
    state: tauri::State<'_, AppState>,
    path: String,
    destination: String,
    runtime: String,
    settings: RenderSettings,
    output_width: Option<u32>,
    output_height: Option<u32>,
) -> Result<u32, String> {
    begin_export_progress(&state, None)?;
    let mut last = (0_u32, 0_u32);
    let mut on_progress = |current: u32, total: u32, message: String| {
        last = (current, total);
        set_export_progress(&state, true, current, total, message);
    };
    let result = export_one_video(
        &state,
        &path,
        &destination,
        &runtime,
        &settings,
        output_width,
        output_height,
        &mut on_progress,
        &|| state.export_cancel.load(Ordering::Acquire),
    );
    if result.as_ref().err().map(String::as_str) == Some(BATCH_CANCELLED) {
        set_export_progress(&state, false, last.0, last.1, "导出已取消".into());
    } else {
        finish_export_progress(&state, &result, last.0);
    }
    result
}

// 选择多个视频文件（批量处理入口）
#[tauri::command]
fn choose_media_multi() -> Option<Vec<String>> {
    rfd::FileDialog::new()
        .add_filter(
            "视频",
            &["mp4", "avi", "mov", "mkv", "webm", "wmv", "m4v", "gif"],
        )
        .pick_files()
        .map(|paths| {
            paths
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect()
        })
}

// 选择输出文件夹（批量处理）
#[tauri::command]
fn choose_directory() -> Option<String> {
    rfd::FileDialog::new()
        .pick_folder()
        .map(|p| p.to_string_lossy().into_owned())
}

// 批量导出：按队列逐个走完整导出管线，进度经 batch_state 轮询给前端
#[tauri::command]
async fn batch_export(
    state: tauri::State<'_, AppState>,
    paths: Vec<String>,
    output_dir: String,
    runtime: String,
    settings: RenderSettings,
    output_ratio: Option<f64>,
) -> Result<(), String> {
    if paths.is_empty() {
        return Err("没有选择视频".into());
    }
    std::fs::create_dir_all(&output_dir).map_err(|e| format!("无法创建输出文件夹: {e}"))?;
    {
        let mut batch = state.batch.lock().map_err(|_| "批量状态锁定失败")?;
        if batch.running {
            return Err("已有批量任务正在进行".into());
        }
        batch.running = true;
        batch.cancelled = false;
        batch.jobs = paths
            .iter()
            .map(|p| BatchJobProgress {
                name: std::path::Path::new(p)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| p.clone()),
                status: "pending".into(),
                current: 0,
                frames: 0,
                fps: 0.0,
                error: String::new(),
            })
            .collect();
    }

    let mut cancelled = false;
    // 批量沿用主预览当前的放大倍率，逐视频按自身分辨率换算输出尺寸
    let vsr_mode = settings.upscale == "vsr";
    let upscale_ratio = match output_ratio {
        Some(r) if vsr_mode && r > 1.0 => r.min(4.0),
        _ => 1.0,
    };
    for (index, path) in paths.iter().enumerate() {
        {
            let mut batch = match state.batch.lock() {
                Ok(b) => b,
                Err(_) => return finish_batch(&state, cancelled),
            };
            if batch.cancelled {
                for job in batch.jobs.iter_mut().skip(index) {
                    if job.status == "pending" {
                        job.status = "cancelled".into();
                    }
                }
                cancelled = true;
                break;
            }
            if let Some(job) = batch.jobs.get_mut(index) {
                job.status = "running".into();
            }
        }
        let source = std::path::Path::new(path);
        let stem = source
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("video{index}"));
        let destination = unique_destination(std::path::Path::new(&output_dir), &stem);
        // 放大倍率按每个视频自身的分辨率换算；越界与偶数化由导出管线兜底
        let sizes = if upscale_ratio > 1.0 {
            video_probe(path).map(|(sw, sh, _, _)| {
                let w = ((sw as f64 * upscale_ratio).round() as u32).clamp(32, 8192) & !1;
                let h = ((sh as f64 * upscale_ratio).round() as u32).clamp(32, 8192) & !1;
                (Some(w), Some(h))
            })
        } else {
            Ok((None, None))
        };
        let start = std::time::Instant::now();
        let result = match sizes {
            Err(e) => Err(e),
            Ok((out_w, out_h)) => export_one_video(
                &state,
                path,
                &destination.to_string_lossy(),
                &runtime,
                &settings,
                out_w,
                out_h,
                &mut |current, frames, _message| {
                    if let Ok(mut batch) = state.batch.lock() {
                        if let Some(job) = batch.jobs.get_mut(index) {
                            job.current = current;
                            job.frames = frames;
                            let elapsed = start.elapsed().as_secs_f64();
                            job.fps = if elapsed > 0.3 {
                                (current as f64 / elapsed * 10.0).round() / 10.0
                            } else {
                                0.0
                            };
                        }
                    }
                },
                &|| state.batch.lock().map(|b| b.cancelled).unwrap_or(false),
            ),
        };
        if let Ok(mut batch) = state.batch.lock() {
            if let Some(job) = batch.jobs.get_mut(index) {
                match &result {
                    Ok(_) => job.status = "done".into(),
                    Err(e) if e == BATCH_CANCELLED => {
                        job.status = "cancelled".into();
                        job.error = BATCH_CANCELLED.into();
                    }
                    Err(e) => {
                        job.status = "failed".into();
                        job.error = e.clone();
                    }
                }
            }
        }
        match &result {
            Err(e) if e == BATCH_CANCELLED => {
                cancelled = true;
                if let Ok(mut batch) = state.batch.lock() {
                    for job in batch.jobs.iter_mut().skip(index + 1) {
                        if job.status == "pending" {
                            job.status = "cancelled".into();
                        }
                    }
                }
                break;
            }
            // 失败与取消都清理半成品文件（取消的已由导出管线删除，此处兜底）
            Err(_) => {
                let _ = std::fs::remove_file(&destination);
            }
            Ok(_) => {}
        }
    }
    finish_batch(&state, cancelled)
}

fn finish_batch(state: &AppState, cancelled: bool) -> Result<(), String> {
    let mut batch = state.batch.lock().map_err(|_| "批量状态锁定失败")?;
    batch.running = false;
    batch.cancelled = cancelled;
    Ok(())
}

#[tauri::command]
fn batch_state(state: tauri::State<'_, AppState>) -> BatchProgress {
    state.batch.lock().map(|b| b.clone()).unwrap_or_default()
}

#[tauri::command]
fn batch_cancel(state: tauri::State<'_, AppState>) {
    if let Ok(mut batch) = state.batch.lock() {
        batch.cancelled = true;
    }
}

#[cfg(test)]
mod render_order_tests {
    use super::*;

    #[test]
    fn defers_vsr_only_for_a_real_multi_pass_upscale() {
        let mut settings = RenderSettings {
            multi_pass: true,
            pass_count: 3,
            ..RenderSettings::default()
        };
        assert!(should_defer_vsr_until_after_first_pass(
            &settings,
            (1920, 1080),
            Some((3840, 2160)),
        ));

        settings.pass_count = 1;
        assert!(!should_defer_vsr_until_after_first_pass(
            &settings,
            (1920, 1080),
            Some((3840, 2160)),
        ));

        settings.pass_count = 3;
        settings.upscale = "none".into();
        assert!(!should_defer_vsr_until_after_first_pass(
            &settings,
            (1920, 1080),
            Some((3840, 2160)),
        ));
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|arg| arg == "--selftest") {
        let root = std::env::current_dir().expect("无法确定工作目录");
        let runtime = args
            .iter()
            .find_map(|arg| arg.strip_prefix("--runtime="))
            .unwrap_or("50");
        let pass_count = args
            .iter()
            .find_map(|arg| arg.strip_prefix("--passes="))
            .map(|value| value.parse::<i32>().expect("--passes 必须为整数"))
            .unwrap_or(1)
            .clamp(1, 5);
        let settings = RenderSettings {
            multi_pass: pass_count > 1,
            pass_count,
            ..RenderSettings::default()
        };
        let mut host = unsafe { Host::load(&root, runtime) }.expect("无法加载 DLSS 宿主");
        let input = vec![128_u8; 640 * 360 * 4];
        match unsafe { host.render(&root, runtime, &input, 640, 360, &settings, true) } {
            Ok(output) if output.len() == 640 * 360 * 4 => {
                println!("DLSS_SELFTEST_OK RTX{runtime} passes={pass_count}");
                return;
            }
            Ok(_) => panic!("DLSS 自检返回了错误的帧长度"),
            Err(error) => panic!("DLSS 自检失败: {error}"),
        }
    }
    tauri::Builder::default()
        .on_window_event(|window, event| {
            let Some(state) = window.try_state::<AppState>() else {
                return;
            };
            match event {
                tauri::WindowEvent::DragDrop(tauri::DragDropEvent::Enter { .. })
                | tauri::WindowEvent::DragDrop(tauri::DragDropEvent::Over { .. }) => {
                    state.drop_active.store(true, Ordering::Release);
                }
                tauri::WindowEvent::DragDrop(tauri::DragDropEvent::Leave) => {
                    state.drop_active.store(false, Ordering::Release);
                }
                tauri::WindowEvent::DragDrop(tauri::DragDropEvent::Drop { paths, .. }) => {
                    state.drop_active.store(false, Ordering::Release);
                    if let Ok(mut pending) = state.pending_drop_paths.lock() {
                        pending.extend(paths.iter().cloned());
                    }
                }
                _ => {}
            }
        })
        .setup(|app| {
            let root = root_dir(&app.handle());
            let temp_dir = root.join("Temp");
            if temp_dir.is_dir() {
                std::fs::remove_dir_all(&temp_dir)?;
            }
            std::fs::create_dir_all(&temp_dir)?;
            app.manage(AppState {
                root,
                temp_dir,
                host: Mutex::new(None),
                vsr: Mutex::new(None),
                vsr_disabled: AtomicBool::new(false),
                media: Mutex::new(HashMap::new()),
                decoder: Mutex::new(None),
                temp_counter: AtomicU64::new(0),
                gpu: detect_gpu_info(),
                drop_active: AtomicBool::new(false),
                pending_drop_paths: Mutex::new(Vec::new()),
                export_progress: Mutex::new(ExportProgress {
                    active: false,
                    current: 0,
                    total: 1,
                    message: String::new(),
                }),
                export_paused: AtomicBool::new(false),
                export_cancel: AtomicBool::new(false),
                batch: Mutex::new(BatchProgress::default()),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            choose_media,
            choose_export,
            gpu_info,
            poll_drop,
            poll_export_progress,
            export_set_paused,
            export_cancel,
            media_info,
            read_image_data,
            process_image,
            process_image_data,
            save_png,
            save_data_png,
            frame_png,
            render_frame_png,
            export_video,
            batch_export,
            batch_state,
            batch_cancel,
            choose_media_multi,
            choose_directory,
            vsr_probe
        ])
        .run(tauri::generate_context!())
        .expect("Tauri 应用启动失败");
}
