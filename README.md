# DLSS5 Neural Render v0.1.24

[中文](README.md) · [English](README.en.md) · [日本語](README.ja.md)

基于 Rust + Tauri v2 的 DLSS Neural Rendering 桌面预览与导出工具。

## 界面预览

![DLSS5 Neural Render 软件界面](Preview.jpg)

## 最新版本

无需自行编译，直接前往 [Releases](https://github.com/JDui/GUI-DLSS5/releases) 下载最新版
Windows x64 发布包，解压后运行 `run.bat` 即可。发布包已携带程序、DLSS 运行时以及视频处理所需
的 FFmpeg / FFprobe。目前支持 NVIDIA GeForce RTX 50 / 40 / 30 系列显卡。

## 构建

```powershell
cargo build --release --manifest-path src-tauri\Cargo.toml
```

构建后的程序位于 `src-tauri\target\release\dlss5-tauri.exe`。

直接双击项目根目录中的 `run.bat` 即可启动；如果 release 程序尚未构建，脚本会
自动执行上述 Cargo 构建。启动脚本只使用 ASCII 字符，兼容旧版 Windows
PowerShell / CMD 的代码页，并会自动固定工作目录，确保程序能够找到宿主 DLL 和
选中的 RTX 运行时。程序启动时会通过 NVIDIA 驱动自动识别 RTX 30 / 40 / 50 系列，
并选择对应运行时；无法识别时默认使用 RTX 50，也可以手动切换。
顶部会同时显示检测到的 NVIDIA 显卡名称。

## 内置运行时

- `nvngx_dlssnr.dll`：RTX 50 原生运行时
- `nvngx_dlssnr_40.dll`：RTX 40 兼容运行时
- `nvngx_dlssnr_30.dll`：RTX 30 兼容运行时

请在第一次进行 DLSS 预览前选择运行时。NGX 会话按进程创建，因此预览开始后切换
运行时需要重启应用。如果兼容 DLL 不被当前驱动或 GPU 接受，程序会记录具体的
原生错误，而不会静默失败。

## 交互说明

- 可以通过“导入图片 / 视频”、拖放或剪贴板载入素材。
- 支持 PNG、JPG、GIF、MP4、AVI、MOV、MKV 等格式；GIF 导入时会自动烘焙为 MP4
  缓存到程序目录的 `Temp` 文件夹，之后按视频轨逐帧预览、播放和导出。
- 视频预览使用常驻的顺序解码器和最近帧缓存；播放来不及处理时会主动跳帧追赶时间线，
  不再为每一帧重复启动 FFmpeg。所有 FFmpeg / FFprobe 子进程均在后台静默运行。
- 轻量预览横竖任一单边最高允许 4520 像素，以保留更高分辨率的预览；普通图片和视频
  导出按素材栏设置的输出分辨率处理（默认原始尺寸）。
- 素材栏可以自定义输出长宽，或使用 X1 / X2 / X4 按钮按原始尺寸的倍数快速设置。
- 右侧设置分为四个标签页：DLSS 参数、放大、后处理、编码器。
- 界面会根据系统语言自动选择中文、English 或日本語；不在这三种语言内时回退中文，顶部药丸滑块可随时手动切换。
- DLSS 参数顶部的“多重Pass模式”药丸开关默认关闭。开启后显示“Pass数”整数滑块，
  范围 1–5，默认 2，表示总处理次数（1 与普通模式相同）。每增加 1，就将上一遍的结果
  再过一次 DLSS；每遍复用下方的风格、强度、色调、结构、皮肤结构、自动掩码及 UI 修正参数。
  对图片、剪贴板图片和视频的预览及 DLSS 输出生效。单 Pass 保持“RTX VSR → DLSS”的顺序；
  多重 Pass 且实际放大时则为“第 1 次 DLSS → RTX VSR（仅一次）→ 其余 DLSS Pass”。次数越多处理越慢；
  多 Pass 会在每遍处理前重置时序历史，避免不同处理阶段相互干扰。
- **多重 Pass 的噪波限制：** 多重 Pass 可能显著增加结果噪波。开启放大功能可配合减缓，
  但不能完全消除；离线版仅使用参考图像作为输入源，缺少额外的实时信息，因此噪波增加不可避免。
- 放大功能依赖 RTX VSR（NVIDIA 网络超分）：需 RTX 20 系及以上显卡、550+ 驱动，程序目录需带
  `nvngx_vsr.dll` 与 `rtx_vsr_host.dll`。启用后 X2 / X4 等放大由 GPU 网络完成再交给 DLSS 做细节增强，
  对比视图中的“原图”侧以最近邻放大展示原始像素；RTX VSR 不可用时放大整体禁用（输出不能超过
  原始分辨率），视频输出宽高会自动取偶。
- 编码器页可为视频导出选择 H.264 / H.265（NVENC 或 CPU x264 / x265）与质量档（CQ / CRF，
  数值越低画质越高），并可开关“保持音频”（附带原视频音轨并转为 AAC）。另有 H.265 / HEVC
  NVENC Lossless 无损视频选项，使用 QP 0 与 4:4:4 RGB 像素格式；NVENC 不可用时会回退到
  x265 Lossless。注意保持音频时音轨仍会转为 AAC；H.264 NVENC 最高支持 4096×4096，更大的输出请改用 H.265。
- 显示模式提供快速比对、对比和 AB 视图。快速比对平时显示 DLSS 画面，按住鼠标左键临时
  切回原图，松开恢复；对比视图左侧显示原图、右侧显示 DLSS，
  中间的简洁分割线可直接拖动。
- AB 视图将视窗平均分成两侧，两边显示原图和 DLSS，并同步缩放与位移；滚轮缩放
  会以鼠标所在位置为锚点。
- 使用鼠标中键或右键拖动画面；滑块和数字输入框都可以精确调整参数。

## 导出

选择目标路径后，可以导出当前视图画面，或按素材类型导出 DLSS 图片 / 视频。

发布包已附带 FFmpeg / FFprobe，可直接处理视频与 GIF；从源码运行时需确保系统 PATH 中可用
FFmpeg / FFprobe。
程序启动时会清理上次遗留的 `Temp`，正常关闭时也会删除本次临时缓存。
