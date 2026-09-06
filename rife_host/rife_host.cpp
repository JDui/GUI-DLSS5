// RIFE frame interpolation host for GUI-DLSS5: wraps nihui/rife-ncnn-vulkan
// (MIT) built on ncnn with the Vulkan backend behind a small C ABI so the Rust
// side can load it via libloading, mirroring rtx_vsr_host.dll.
//
//   int  rife_init(const wchar_t* modeldir, int gpuid, int rife_v4);
//   int  rife_interp(const unsigned char* a, const unsigned char* b,
//                    int w, int h, float timestep, unsigned char* out);
//   void rife_shutdown();
//
// rife_init loads the ncnn model directory (e.g. models/rife-v4.6 with
// flownet.param/flownet.bin); returns 0 on failure. rife_interp takes two
// RGBA8 frames and writes the interpolated RGBA8 frame at `timestep` in [0,1].
#include "rife.h"

#include <windows.h>

#include <cstdint>
#include <string>
#include <vector>

static RIFE* g_rife = nullptr;
static bool g_ready = false;

extern "C" __declspec(dllexport) int rife_init(const wchar_t* modeldir, int gpuid, int rife_v4) {
    if (g_ready) return 1;
    if (!modeldir) return 0;
    g_rife = new RIFE(gpuid, false, false, false, 1, false, rife_v4 != 0);
    if (g_rife->load(std::wstring(modeldir)) != 0) {
        delete g_rife;
        g_rife = nullptr;
        return 0;
    }
    g_ready = true;
    return 1;
}

extern "C" __declspec(dllexport) int rife_interp(const unsigned char* a, const unsigned char* b,
                                                 int w, int h, float timestep,
                                                 unsigned char* out) {
    if (!g_ready || !a || !b || !out || w <= 0 || h <= 0) return 0;
    if (timestep < 0.f) timestep = 0.f;
    if (timestep > 1.f) timestep = 1.f;
    // RIFE::process 期望原始交错像素字节（Windows 上按 BGR 解释并内部转 RGB 平面格式）
    std::vector<unsigned char> bgra_bgr((size_t)w * h * 3);
    for (size_t i = 0; i < (size_t)w * h; ++i) {
        bgra_bgr[i * 3 + 0] = a[i * 4 + 2];
        bgra_bgr[i * 3 + 1] = a[i * 4 + 1];
        bgra_bgr[i * 3 + 2] = a[i * 4 + 0];
    }
    ncnn::Mat ma(w, h, (void*)bgra_bgr.data(), (size_t)3, 3);
    std::vector<unsigned char> bgra_bgb((size_t)w * h * 3);
    for (size_t i = 0; i < (size_t)w * h; ++i) {
        bgra_bgb[i * 3 + 0] = b[i * 4 + 2];
        bgra_bgb[i * 3 + 1] = b[i * 4 + 1];
        bgra_bgb[i * 3 + 2] = b[i * 4 + 0];
    }
    ncnn::Mat mb(w, h, (void*)bgra_bgb.data(), (size_t)3, 3);
    ncnn::Mat mo(w, h, (size_t)3);
    if (g_rife->process(ma, mb, timestep, mo) != 0) return 0;
    // 输出为 BGR 交错字节（to_pixels PIXEL_RGB2BGR 的结果）
    for (int y = 0; y < h; ++y) {
        const unsigned char* src = mo.row<const unsigned char>(y);
        unsigned char* dst = out + static_cast<int64_t>(y) * w * 4;
        for (int x = 0; x < w; ++x) {
            dst[x * 4 + 0] = src[x * 3 + 2];
            dst[x * 4 + 1] = src[x * 3 + 1];
            dst[x * 4 + 2] = src[x * 3 + 0];
            dst[x * 4 + 3] = 255;
        }
    }
    return 1;
}

extern "C" __declspec(dllexport) void rife_shutdown() {
    if (g_rife) {
        delete g_rife;
        g_rife = nullptr;
    }
    g_ready = false;
}
