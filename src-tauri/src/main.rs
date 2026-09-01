#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use base64::{engine::general_purpose::STANDARD, Engine};
use image::{codecs::png::PngEncoder, imageops::FilterType, ExtendedColorType, ImageEncoder, RgbaImage};
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
const MAX_PREVIEW_SIDE: u32 = 2160;

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
    style: i32,
    intensity: f32,
    local_tone: f32,
    local_struct: f32,
    skin_structure: f32,
    use_auto_mask: bool,
    ui_correction: bool,
    output_view: i32,
    output_mix: f32,
}

impl Default for RenderSettings {
    fn default() -> Self {
        Self {
            style: 0,
            intensity: 1.0,
            local_tone: 1.0,
            local_struct: 1.0,
            skin_structure: 1.0,
            use_auto_mask: false,
            ui_correction: false,
            output_view: 0,
            output_mix: 1.0,
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
        if (self.process)(
            input.as_ptr() as *mut u8,
            self.motion.as_mut_ptr(),
            self.depth.as_mut_ptr(),
            self.output.as_mut_ptr(),
            i32::from(reset),
        ) == 0
        {
            return Err("DLSS 未生成画面。".into());
        }
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

struct AppState {
    root: PathBuf,
    temp_dir: PathBuf,
    host: Mutex<Option<Host>>,
    media: Mutex<HashMap<String, MediaInfo>>,
    decoder: Mutex<Option<VideoDecoder>>,
    temp_counter: AtomicU64,
    gpu: GpuInfo,
    drop_active: AtomicBool,
    pending_drop_paths: Mutex<Vec<PathBuf>>,
    export_progress: Mutex<ExportProgress>,
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

fn begin_export_progress(state: &AppState, total: u32) -> Result<(), String> {
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
        total: total.max(1),
        message: "准备导出…".into(),
    };
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

fn spawn_decoder(
    info: &MediaInfo,
    max_side: u32,
    start_frame: u32,
) -> Result<VideoDecoder, String> {
    let (width, height) = preview_dimensions(info.width, info.height, max_side);
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
    let mut slot = state.decoder.lock().map_err(|_| "视频解码器锁定失败")?;
    let reusable = slot
        .as_ref()
        .is_some_and(|decoder| decoder.path == path && decoder.max_side == max_side);
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
        *slot = Some(spawn_decoder(&info, max_side, start_frame)?);
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
fn preview_size(image: RgbaImage, max_side: u32) -> RgbaImage {
    let max_side = max_side.clamp(320, MAX_PREVIEW_SIDE);
    let (w, h) = image.dimensions();
    if w.max(h) <= max_side {
        image
    } else {
        let scale = max_side as f32 / w.max(h) as f32;
        image::imageops::resize(
            &image,
            (w as f32 * scale).round().max(1.0) as u32,
            (h as f32 * scale).round().max(1.0) as u32,
            FilterType::Triangle,
        )
    }
}
fn process_rgba(
    state: &AppState,
    image: RgbaImage,
    runtime: String,
    settings: RenderSettings,
) -> Result<Vec<u8>, String> {
    let (w, h) = image.dimensions();
    let mut host = state.host.lock().map_err(|_| "DLSS 会话锁定失败")?;
    if host.is_none() {
        *host = Some(unsafe { Host::load(&state.root, &runtime)? });
    }
    let input = image.into_raw();
    let rendered = unsafe {
        host.as_mut().unwrap().render(&state.root, &runtime, &input, w, h, &settings, true)?
    };
    rgba_png(rendered, w, h)
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
) -> Result<Response, String> {
    let image = image::open(&path)
        .map_err(|e| format!("无法读取图片: {e}"))?
        .to_rgba8();
    let _ = app;
    Ok(Response::new(process_rgba(
        &state,
        preview_size(image, max_side),
        runtime,
        settings,
    )?))
}

#[tauri::command]
async fn process_image_data(
    state: tauri::State<'_, AppState>,
    data: String,
    runtime: String,
    settings: RenderSettings,
    max_side: u32,
) -> Result<Response, String> {
    Ok(Response::new(process_rgba(
        &state,
        preview_size(image_data_uri(&data)?, max_side),
        runtime,
        settings,
    )?))
}

#[tauri::command]
async fn save_data_png(
    state: tauri::State<'_, AppState>,
    data: String,
    destination: String,
) -> Result<(), String> {
    begin_export_progress(&state, 1)?;
    let result =
        image_data_uri(&data).and_then(|image| image.save(destination).map_err(|e| e.to_string()));
    finish_export_progress(&state, &result, u32::from(result.is_ok()));
    result
}

#[tauri::command]
async fn read_image_data(path: String, max_side: u32) -> Result<Response, String> {
    let image = image::open(path)
        .map_err(|e| format!("无法读取图片: {e}"))?
        .to_rgba8();
    let image = preview_size(image, max_side);
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
) -> Result<(), String> {
    begin_export_progress(&state, 1)?;
    let result: Result<(), String> = (|| {
        let image = image::open(&path)
            .map_err(|e| format!("无法读取图片: {e}"))?
            .to_rgba8();
        let (w, h) = image.dimensions();
        let mut host = state.host.lock().map_err(|_| "DLSS 会话锁定失败")?;
        if host.is_none() {
            *host = Some(unsafe { Host::load(&state.root, &runtime)? });
        }
        let input = image.into_raw();
        let out = unsafe {
            host.as_mut().unwrap().render(&state.root, &runtime, &input, w, h, &settings, true)?
        };
        RgbaImage::from_raw(w, h, out.to_vec())
            .ok_or("无效输出")?
            .save(&destination)
            .map_err(|e| format!("无法写入 PNG: {e}"))?;
        Ok(())
    })();
    finish_export_progress(&state, &result, u32::from(result.is_ok()));
    result
}

#[tauri::command]
async fn frame_png(
    state: tauri::State<'_, AppState>,
    path: String,
    frame: u32,
    max_side: u32,
) -> Result<Response, String> {
    let (bytes, w, h) = decode_video_frame(&state, &path, frame, max_side)?;
    Ok(Response::new(rgba_png(&bytes, w, h)?))
}

#[tauri::command]
async fn render_frame_png(
    state: tauri::State<'_, AppState>,
    path: String,
    frame: u32,
    runtime: String,
    settings: RenderSettings,
    max_side: u32,
) -> Result<Response, String> {
    let (bytes, w, h) = decode_video_frame(&state, &path, frame, max_side)?;
    let mut host = state.host.lock().map_err(|_| "DLSS 会话锁定失败")?;
    if host.is_none() {
        *host = Some(unsafe { Host::load(&state.root, &runtime)? });
    }
    let rendered = unsafe {
        host.as_mut()
            .unwrap()
            .render(&state.root, &runtime, &bytes, w, h, &settings, true)?
    };
    Ok(Response::new(rgba_png(rendered, w, h)?))
}

// 预检 NVENC 会话是否可用（驱动/并发限制），失败则回退 CPU 编码
fn nvenc_available() -> bool {
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
            "h264_nvenc",
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

#[tauri::command]
async fn export_video(
    state: tauri::State<'_, AppState>,
    path: String,
    destination: String,
    runtime: String,
    settings: RenderSettings,
) -> Result<u32, String> {
    let (w, h, total_frames, fps) = video_probe(&path)?;
    begin_export_progress(&state, total_frames)?;
    let nvenc = nvenc_available();
    set_export_progress(
        &state,
        true,
        0,
        total_frames,
        if nvenc {
            "已启用 NVENC，正在导出…".into()
        } else {
            "NVENC 不可用，使用 CPU 编码…".into()
        },
    );
    let mut current = 0;
    let result: Result<u32, String> = (|| {
        let frame_bytes = (w * h * 4) as usize;
        let mut decode = tool("ffmpeg")?
            .args([
                "-v", "error", "-i", &path, "-f", "rawvideo", "-pix_fmt", "rgba", "-",
            ])
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| format!("无法启动 FFmpeg 解码器: {e}"))?;
        // 音轨在编码器内直接混流：应用中途退出时，残留的编码进程也会把
        // 带音轨的文件收尾写盘，不会留下无声的半成品。
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
                "-map",
                "1:a?",
                "-c:a",
                "copy",
                "-pix_fmt",
                "yuv420p",
                "-shortest",
            ])
            .args(if nvenc {
                &[
                    "-c:v",
                    "h264_nvenc",
                    "-preset",
                    "p5",
                    "-rc",
                    "vbr",
                    "-cq",
                    "23",
                    "-b:v",
                    "0",
                ][..]
            } else {
                &["-c:v", "libx264", "-preset", "veryfast", "-crf", "23"][..]
            })
            .arg(&destination)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .map_err(|e| format!("无法启动 FFmpeg 编码器: {e}"))?;
        let mut reader = decode.stdout.take().ok_or("无法读取视频数据")?;
        let mut writer = encode.stdin.take().ok_or("无法写入视频数据")?;
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
        let mut outcome: Result<u32, String> = Ok(0);
        {
            let mut host = state.host.lock().map_err(|_| "DLSS 会话锁定失败")?;
            if host.is_none() {
                *host = Some(unsafe { Host::load(&state.root, &runtime)? });
            }
            for message in rx {
                let frame = match message {
                    Ok(frame) => frame,
                    Err(e) => {
                        outcome = Err(e);
                        break;
                    }
                };
                let rendered = match unsafe {
                    host.as_mut().unwrap().render(
                        &state.root,
                        &runtime,
                        &frame,
                        w,
                        h,
                        &settings,
                        count == 0,
                    )
                } {
                    Ok(rendered) => rendered,
                    Err(e) => {
                        outcome = Err(e);
                        break;
                    }
                };
                if let Err(e) = writer.write_all(rendered) {
                    outcome = Err(format!("视频编码写入失败: {e}"));
                    break;
                }
                count += 1;
                current = count;
                set_export_progress(
                    &state,
                    true,
                    current,
                    total_frames,
                    format!("正在导出第 {current} / {total_frames} 帧"),
                );
            }
        }
        let _ = reader_thread.join();
        drop(writer);
        if outcome.is_err() {
            let _ = decode.kill();
            let _ = encode.kill();
        }
        if !decode.wait().map_err(|e| e.to_string())?.success() && outcome.is_ok() {
            outcome = Err("FFmpeg 解码失败。".into());
        }
        if !encode.wait().map_err(|e| e.to_string())?.success() && outcome.is_ok() {
            outcome = Err("FFmpeg 编码失败。".into());
        }
        if outcome.is_ok() {
            outcome = Ok(count);
        }
        outcome
    })();
    finish_export_progress(&state, &result, current);
    result
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|arg| arg == "--selftest") {
        let root = std::env::current_dir().expect("无法确定工作目录");
        let runtime = args
            .iter()
            .find_map(|arg| arg.strip_prefix("--runtime="))
            .unwrap_or("50");
        let settings = RenderSettings::default();
        let mut host = unsafe { Host::load(&root, runtime) }.expect("无法加载 DLSS 宿主");
        let input = vec![128_u8; 640 * 360 * 4];
        match unsafe { host.render(&root, runtime, &input, 640, 360, &settings, true) } {
            Ok(output) if output.len() == 640 * 360 * 4 => {
                println!("DLSS_SELFTEST_OK RTX{runtime}");
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
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            choose_media,
            choose_export,
            gpu_info,
            poll_drop,
            poll_export_progress,
            media_info,
            read_image_data,
            process_image,
            process_image_data,
            save_png,
            save_data_png,
            frame_png,
            render_frame_png,
            export_video
        ])
        .run(tauri::generate_context!())
        .expect("Tauri 应用启动失败");
}
