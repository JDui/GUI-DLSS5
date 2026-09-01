// RTX VSR host for GUI-DLSS5: wraps the NVIDIA RTX Video SDK (NGX Feature 16, nvngx_vsr.dll)
// with a minimal C ABI so the Rust side can load it via libloading, mirroring dlssnr_host.dll.
//
//   int  vsr_init(const wchar_t* snippet_dir);
//   int  vsr_upscale(const unsigned char* src, int sw, int sh,
//                    unsigned char* dst, int dw, int dh, int quality);
//   void vsr_shutdown();
//
// vsr_init creates the D3D12 device, initializes NGX (searching snippet_dir for
// nvngx_vsr.dll) and creates the VSR feature; it returns 0 when VSR is unavailable
// (no NVIDIA RTX GPU, driver too old, missing snippet DLL). vsr_upscale runs a
// synchronous RGBA8 -> RGBA8 upscale and returns 1 on success, 0 on any failure
// (the caller falls back to CPU resampling).
//
// Built both as DLL and, with VSR_HOST_TEST defined, as a self-test executable.
#include <windows.h>
#include <d3d12.h>
#include <dxgi1_6.h>
#include <cstdio>
#include <cstdint>
#include <cstdarg>
#include <cstring>
#include <string>
#include <vector>

#include "nvsdk_ngx.h"
#include "nvsdk_ngx_helpers.h"
#include "nvsdk_ngx_defs_vsr.h"
#include "nvsdk_ngx_helpers_vsr.h"

#pragma comment(lib, "d3d12.lib")
#pragma comment(lib, "dxgi.lib")

static ID3D12Device* g_device = nullptr;
static ID3D12CommandQueue* g_queue = nullptr;
static ID3D12CommandAllocator* g_alloc = nullptr;
static ID3D12GraphicsCommandList* g_cmd = nullptr;
static ID3D12Fence* g_fence = nullptr;
static HANDLE g_fenceEvent = nullptr;
static UINT64 g_fenceValue = 1;
static NVSDK_NGX_Parameter* g_params = nullptr;
static NVSDK_NGX_Handle* g_feature = nullptr;
static bool g_ready = false;
static std::wstring g_dir;
static ID3D12CommandList* g_lists[1] = {};

struct Surfaces {
    UINT sw = 0, sh = 0, dw = 0, dh = 0;
    UINT inPitch = 0, outPitch = 0;
    ID3D12Resource* upload = nullptr;
    ID3D12Resource* input = nullptr;
    ID3D12Resource* output = nullptr;
    ID3D12Resource* readback = nullptr;
};
static Surfaces g_sf;

static void vlog(const char* fmt, ...) {
    if (g_dir.empty()) return;
    const std::wstring path = g_dir + L"\\vsr_run.log";
    FILE* f = nullptr;
    if (_wfopen_s(&f, path.c_str(), L"a") != 0 || !f) return;
    SYSTEMTIME st;
    GetLocalTime(&st);
    fprintf(f, "[%04u-%02u-%02u %02u:%02u:%02u] ", st.wYear, st.wMonth, st.wDay,
            st.wHour, st.wMinute, st.wSecond);
    va_list args;
    va_start(args, fmt);
    vfprintf(f, fmt, args);
    va_end(args);
    fputc('\n', f);
    fclose(f);
}

static void flushGpu() {
    g_fenceValue++;
    g_queue->Signal(g_fence, g_fenceValue);
    if (g_fence->GetCompletedValue() < g_fenceValue) {
        g_fence->SetEventOnCompletion(g_fenceValue, g_fenceEvent);
        WaitForSingleObject(g_fenceEvent, INFINITE);
    }
}

static ID3D12Resource* makeTexture(UINT w, UINT h, D3D12_RESOURCE_FLAGS flags) {
    D3D12_HEAP_PROPERTIES heap = { D3D12_HEAP_TYPE_DEFAULT };
    D3D12_RESOURCE_DESC desc = {};
    desc.Dimension = D3D12_RESOURCE_DIMENSION_TEXTURE2D;
    desc.Width = w;
    desc.Height = h;
    desc.DepthOrArraySize = 1;
    desc.MipLevels = 1;
    desc.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
    desc.SampleDesc.Count = 1;
    desc.Flags = flags;
    ID3D12Resource* res = nullptr;
    if (FAILED(g_device->CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &desc,
                                                 D3D12_RESOURCE_STATE_COMMON, nullptr,
                                                 IID_PPV_ARGS(&res))))
        return nullptr;
    return res;
}

static ID3D12Resource* makeBuffer(UINT64 bytes, D3D12_HEAP_TYPE heapType, D3D12_RESOURCE_STATES initial) {
    D3D12_HEAP_PROPERTIES heap = { heapType };
    D3D12_RESOURCE_DESC desc = {};
    desc.Dimension = D3D12_RESOURCE_DIMENSION_BUFFER;
    desc.Width = bytes;
    desc.Height = 1;
    desc.DepthOrArraySize = 1;
    desc.MipLevels = 1;
    desc.SampleDesc.Count = 1;
    desc.Layout = D3D12_TEXTURE_LAYOUT_ROW_MAJOR;
    ID3D12Resource* res = nullptr;
    if (FAILED(g_device->CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &desc, initial,
                                                 nullptr, IID_PPV_ARGS(&res))))
        return nullptr;
    return res;
}

static D3D12_RESOURCE_BARRIER transitionBarrier(ID3D12Resource* res, D3D12_RESOURCE_STATES before,
                                                D3D12_RESOURCE_STATES after) {
    D3D12_RESOURCE_BARRIER b = {};
    b.Type = D3D12_RESOURCE_BARRIER_TYPE_TRANSITION;
    b.Transition.pResource = res;
    b.Transition.StateBefore = before;
    b.Transition.StateAfter = after;
    b.Transition.Subresource = D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES;
    return b;
}

static void releaseSurfaces() {
    for (ID3D12Resource** res : { &g_sf.upload, &g_sf.input, &g_sf.output, &g_sf.readback }) {
        if (*res) (*res)->Release();
        *res = nullptr;
    }
    g_sf = Surfaces{};
}

// (Re)creates the upload/input/output/readback set when the requested size changes.
static bool ensureSurfaces(UINT sw, UINT sh, UINT dw, UINT dh) {
    if (g_sf.sw == sw && g_sf.sh == sh && g_sf.dw == dw && g_sf.dh == dh) return true;
    releaseSurfaces();

    D3D12_RESOURCE_DESC inDesc = {};
    inDesc.Dimension = D3D12_RESOURCE_DIMENSION_TEXTURE2D;
    inDesc.Width = sw;
    inDesc.Height = sh;
    inDesc.DepthOrArraySize = 1;
    inDesc.MipLevels = 1;
    inDesc.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
    inDesc.SampleDesc.Count = 1;
    D3D12_PLACED_SUBRESOURCE_FOOTPRINT inFp{};
    UINT64 inTotal = 0;
    g_device->GetCopyableFootprints(&inDesc, 0, 1, 0, &inFp, nullptr, nullptr, &inTotal);

    D3D12_RESOURCE_DESC outDesc = inDesc;
    outDesc.Width = dw;
    outDesc.Height = dh;
    D3D12_PLACED_SUBRESOURCE_FOOTPRINT outFp{};
    UINT64 outTotal = 0;
    g_device->GetCopyableFootprints(&outDesc, 0, 1, 0, &outFp, nullptr, nullptr, &outTotal);

    Surfaces sf;
    sf.upload = makeBuffer(inTotal, D3D12_HEAP_TYPE_UPLOAD, D3D12_RESOURCE_STATE_GENERIC_READ);
    sf.readback = makeBuffer(outTotal, D3D12_HEAP_TYPE_READBACK, D3D12_RESOURCE_STATE_COPY_DEST);
    sf.input = makeTexture(sw, sh, D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS);
    sf.output = makeTexture(dw, dh, D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS);
    if (!sf.upload || !sf.readback || !sf.input || !sf.output) {
        vlog("surface creation failed %ux%u -> %ux%u", sw, sh, dw, dh);
        for (ID3D12Resource** res : { &sf.upload, &sf.input, &sf.output, &sf.readback }) {
            if (*res) (*res)->Release();
        }
        return false;
    }
    sf.sw = sw;
    sf.sh = sh;
    sf.dw = dw;
    sf.dh = dh;
    sf.inPitch = (UINT)inFp.Footprint.RowPitch;
    sf.outPitch = (UINT)outFp.Footprint.RowPitch;
    g_sf = sf;
    return true;
}

extern "C" __declspec(dllexport) int vsr_init(const wchar_t* snippet_dir) {
    if (g_ready) return 1;
    if (!snippet_dir) return 0;
    g_dir = snippet_dir;

    IDXGIFactory1* factory = nullptr;
    IDXGIAdapter1* adapter = nullptr;
    if (FAILED(CreateDXGIFactory1(IID_PPV_ARGS(&factory)))) { vlog("CreateDXGIFactory1 failed"); return 0; }
    for (UINT i = 0; factory->EnumAdapters1(i, &adapter) != DXGI_ERROR_NOT_FOUND; ++i) {
        DXGI_ADAPTER_DESC1 desc;
        adapter->GetDesc1(&desc);
        if (!(desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE) && desc.VendorId == 0x10DE) break;
        adapter->Release();
        adapter = nullptr;
    }
    factory->Release();
    if (!adapter) { vlog("no NVIDIA adapter"); return 0; }
    if (FAILED(D3D12CreateDevice(adapter, D3D_FEATURE_LEVEL_12_0, IID_PPV_ARGS(&g_device)))) {
        vlog("D3D12CreateDevice failed");
        adapter->Release();
        return 0;
    }
    adapter->Release();

    D3D12_COMMAND_QUEUE_DESC qdesc = {};
    if (FAILED(g_device->CreateCommandQueue(&qdesc, IID_PPV_ARGS(&g_queue))) ||
        FAILED(g_device->CreateFence(0, D3D12_FENCE_FLAG_NONE, IID_PPV_ARGS(&g_fence))) ||
        FAILED(g_device->CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT, IID_PPV_ARGS(&g_alloc))) ||
        FAILED(g_device->CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, g_alloc, nullptr,
                                           IID_PPV_ARGS(&g_cmd)))) {
        vlog("D3D12 setup failed");
        return 0;
    }
    g_cmd->Close();
    g_lists[0] = g_cmd;
    g_fenceEvent = CreateEventW(nullptr, FALSE, FALSE, nullptr);

    // NGX init with a path list pointing at the directory that carries nvngx_vsr.dll
    const wchar_t* paths[1] = { g_dir.c_str() };
    NVSDK_NGX_FeatureCommonInfo info = {};
    info.PathListInfo.Path = paths;
    info.PathListInfo.Length = 1;
    const NVSDK_NGX_Result initResult = NVSDK_NGX_D3D12_Init(0ull, L".", g_device, &info);
    if (NVSDK_NGX_FAILED(initResult)) { vlog("NGX init failed 0x%08X", (unsigned)initResult); return 0; }

    if (FAILED(NVSDK_NGX_D3D12_GetCapabilityParameters(&g_params))) { vlog("capability params failed"); return 0; }
    int available = 0;
    g_params->Get(NVSDK_NGX_Parameter_VSR_Available, &available);
    if (!available) { vlog("VSR.Available=0 (driver/GPU unsupported)"); return 0; }

    g_cmd->Reset(g_alloc, nullptr);
    NVSDK_NGX_Feature_Create_Params createParams = {};
    const NVSDK_NGX_Result createResult =
        NGX_D3D12_CREATE_VSR_EXT(g_cmd, 0, 0, &g_feature, g_params, &createParams);
    if (NVSDK_NGX_FAILED(createResult)) { vlog("create VSR failed 0x%08X", (unsigned)createResult); return 0; }
    g_cmd->Close();
    g_queue->ExecuteCommandLists(1, g_lists);
    flushGpu();

    g_ready = true;
    vlog("VSR host ready");
    return 1;
}

extern "C" __declspec(dllexport) int vsr_upscale(const unsigned char* src, int sw, int sh,
                                                 unsigned char* dst, int dw, int dh, int quality) {
    if (!g_ready || !src || !dst || sw <= 0 || sh <= 0 || dw <= 0 || dh <= 0) return 0;
    if (quality < 0) quality = 0;
    if (quality > 4) quality = 4;
    if (!ensureSurfaces((UINT)sw, (UINT)sh, (UINT)dw, (UINT)dh)) return 0;
    Surfaces& sf = g_sf;

    {
        uint8_t* mapped = nullptr;
        if (FAILED(sf.upload->Map(0, nullptr, (void**)&mapped))) return 0;
        for (int y = 0; y < sh; ++y)
            memcpy(mapped + (UINT64)y * sf.inPitch, src + (UINT64)y * sw * 4, (UINT64)sw * 4);
        sf.upload->Unmap(0, nullptr);
    }

    const D3D12_RESOURCE_BARRIER b1 =
        transitionBarrier(sf.input, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_COPY_DEST);
    g_cmd->Reset(g_alloc, nullptr);
    g_cmd->ResourceBarrier(1, &b1);
    D3D12_TEXTURE_COPY_LOCATION dstLoc = {};
    dstLoc.pResource = sf.input;
    dstLoc.Type = D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX;
    dstLoc.SubresourceIndex = 0;
    D3D12_TEXTURE_COPY_LOCATION srcLoc = {};
    srcLoc.pResource = sf.upload;
    srcLoc.Type = D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT;
    srcLoc.PlacedFootprint.Footprint.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
    srcLoc.PlacedFootprint.Footprint.Width = (UINT)sw;
    srcLoc.PlacedFootprint.Footprint.Height = (UINT)sh;
    srcLoc.PlacedFootprint.Footprint.Depth = 1;
    srcLoc.PlacedFootprint.Footprint.RowPitch = sf.inPitch;
    g_cmd->CopyTextureRegion(&dstLoc, 0, 0, 0, &srcLoc, nullptr);
    const D3D12_RESOURCE_BARRIER b2 =
        transitionBarrier(sf.input, D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_COMMON);
    g_cmd->ResourceBarrier(1, &b2);

    NVSDK_NGX_D3D12_VSR_Eval_Params eval = {};
    eval.pInput = sf.input;
    eval.pOutput = sf.output;
    eval.InputSubrectSize = { (unsigned)sw, (unsigned)sh };
    eval.OutputSubrectSize = { (unsigned)dw, (unsigned)dh };
    eval.QualityLevel = (NVSDK_NGX_VSR_QualityLevel)quality;
    const NVSDK_NGX_Result result = NGX_D3D12_EVALUATE_VSR_EXT(g_cmd, g_feature, g_params, &eval);
    if (NVSDK_NGX_FAILED(result)) {
        vlog("evaluate failed 0x%08X (%dx%d -> %dx%d)", (unsigned)result, sw, sh, dw, dh);
        return 0;
    }

    const D3D12_RESOURCE_BARRIER b3 =
        transitionBarrier(sf.output, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_COPY_SOURCE);
    g_cmd->ResourceBarrier(1, &b3);
    D3D12_TEXTURE_COPY_LOCATION outLoc = {};
    outLoc.pResource = sf.output;
    outLoc.Type = D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX;
    outLoc.SubresourceIndex = 0;
    D3D12_TEXTURE_COPY_LOCATION rbLoc = {};
    rbLoc.pResource = sf.readback;
    rbLoc.Type = D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT;
    rbLoc.PlacedFootprint.Footprint.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
    rbLoc.PlacedFootprint.Footprint.Width = (UINT)dw;
    rbLoc.PlacedFootprint.Footprint.Height = (UINT)dh;
    rbLoc.PlacedFootprint.Footprint.Depth = 1;
    rbLoc.PlacedFootprint.Footprint.RowPitch = sf.outPitch;
    g_cmd->CopyTextureRegion(&rbLoc, 0, 0, 0, &outLoc, nullptr);
    const D3D12_RESOURCE_BARRIER b4 =
        transitionBarrier(sf.output, D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATE_COMMON);
    g_cmd->ResourceBarrier(1, &b4);
    g_cmd->Close();
    g_queue->ExecuteCommandLists(1, g_lists);
    flushGpu();

    {
        uint8_t* mapped = nullptr;
        if (FAILED(sf.readback->Map(0, nullptr, (void**)&mapped))) return 0;
        for (int y = 0; y < dh; ++y)
            memcpy(dst + (UINT64)y * dw * 4, mapped + (UINT64)y * sf.outPitch, (UINT64)dw * 4);
        sf.readback->Unmap(0, nullptr);
    }
    return 1;
}

extern "C" __declspec(dllexport) void vsr_shutdown() {
    if (!g_ready) return;
    releaseSurfaces();
    NVSDK_NGX_D3D12_Shutdown1(g_device);
    g_ready = false;
    vlog("VSR host shutdown");
}

#ifdef VSR_HOST_TEST
int main() {
    wchar_t dir[MAX_PATH];
    GetCurrentDirectoryW(MAX_PATH, dir);
    if (!vsr_init(dir)) { printf("SELFTEST: init failed (see vsr_run.log)\n"); return 1; }
    const int sw = 640, sh = 360, dw = 1280, dh = 720;
    std::vector<unsigned char> src((UINT64)sw * sh * 4), dst((UINT64)dw * dh * 4);
    for (int y = 0; y < sh; ++y)
        for (int x = 0; x < sw; ++x) {
            unsigned char* p = &src[((UINT64)y * sw + x) * 4];
            p[0] = (unsigned char)(x * 255 / sw);
            p[1] = (unsigned char)(y * 255 / sh);
            p[2] = ((x / 20 + y / 20) % 2) ? 60 : 200;
            p[3] = 255;
        }
    if (!vsr_upscale(src.data(), sw, sh, dst.data(), dw, dh, 4)) {
        printf("SELFTEST: upscale failed\n");
        return 1;
    }
    double mean = 0;
    unsigned char lo = 255, hi = 0;
    for (size_t i = 0; i < dst.size(); i += 4) {
        mean += dst[i];
        lo = lo < dst[i] ? lo : dst[i];
        hi = hi > dst[i] ? hi : dst[i];
    }
    mean /= dst.size() / 4;
    printf("SELFTEST OK mean=%.1f min=%u max=%u\n", mean, lo, hi);
    vsr_shutdown();
    return mean > 1.0 && hi > lo ? 0 : 1;
}
#endif
