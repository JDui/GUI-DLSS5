// rife_host.dll runtime selftest.
// usage:
//   test_loader.exe <model> [gpu|cpu]                       - synthetic gradient pair
//   test_loader.exe <model> [gpu|cpu] <inA.rgb> <inB.rgb> <out.bmp> - real frame pair (rgb24)
#include <windows.h>
#include <cstdio>
#include <cstdint>
#include <cmath>
#include <string>
#include <vector>
typedef int (__cdecl *InitFn)(const wchar_t*, int, int);
typedef int (__cdecl *InterpFn)(const unsigned char*, const unsigned char*, int, int, float, unsigned char*);
typedef void (__cdecl *ShutdownFn)();

static unsigned char pixel(int x, int y, int shift) {
    int gx = (x - shift + 640 * 4) % 640;
    return (unsigned char)((gx + y) * 255 / (640 + 360));
}

static void saveBmp(const char* path, const unsigned char* rgba, int w, int h) {
    FILE* f = fopen(path, "wb");
    if (!f) return;
    unsigned char hdr[54] = {0};
    unsigned sz = (unsigned)w * h * 4;
    hdr[0] = 'B'; hdr[1] = 'M';
    *(unsigned*)(hdr + 2) = 54 + sz;
    *(unsigned*)(hdr + 10) = 54;
    *(unsigned*)(hdr + 14) = 40;
    *(int*)(hdr + 18) = w;
    *(int*)(hdr + 22) = h;
    hdr[26] = 1; hdr[28] = 32;
    fwrite(hdr, 1, 54, f);
    for (int y = h - 1; y >= 0; --y) {
        for (int x = 0; x < w; ++x) {
            unsigned char px[4] = { rgba[((size_t)y * w + x) * 4 + 2], rgba[((size_t)y * w + x) * 4 + 1], rgba[((size_t)y * w + x) * 4], 255 };
            fwrite(px, 1, 4, f);
        }
    }
    fclose(f);
}

int main(int argc, char** argv) {
    HMODULE dll = LoadLibraryW(L"rife_host.dll");
    if (!dll) { printf("SELFTEST: rife_host.dll not found\n"); return 1; }
    InitFn init = (InitFn)GetProcAddress(dll, "rife_init");
    InterpFn interp = (InterpFn)GetProcAddress(dll, "rife_interp");
    if (!init || !interp) { printf("SELFTEST: exports missing\n"); return 1; }
    const char* model = argc > 1 ? argv[1] : "rife-v4.6";
    wchar_t dir[MAX_PATH];
    GetCurrentDirectoryW(MAX_PATH, dir);
    std::string mdir = std::string(model);
    std::wstring modeldir = std::wstring(dir) + L"/models/" + std::wstring(mdir.begin(), mdir.end());
    int gpuid = (argc > 2 && std::string(argv[2]) == "cpu") ? -1 : 0;
    if (!init(modeldir.c_str(), gpuid, 1)) { printf("SELFTEST: init failed\n"); return 1; }

    const int w = 640, h = 360;
    std::vector<unsigned char> a((size_t)w * h * 4), b((size_t)w * h * 4), out((size_t)w * h * 4);
    if (argc > 3) {
        // real-frame mode: test_loader.exe <model> [gpu|cpu] <inA.rgb24> <inB.rgb24> <out.bmp>
        FILE* fa = fopen(argv[3], "rb");
        if (!fa) { printf("SELFTEST: cannot read %s\n", argv[3]); return 1; }
        FILE* fb = fopen(argv[4], "rb");
        if (!fb) { printf("SELFTEST: cannot read %s\n", argv[4]); return 1; }
        std::vector<unsigned char> ra((size_t)w * h * 3), rb((size_t)w * h * 3);
        if (fread(ra.data(), 1, ra.size(), fa) != ra.size()) { fclose(fa); return 1; }
        if (fread(rb.data(), 1, rb.size(), fb) != rb.size()) { fclose(fb); return 1; }
        fclose(fa); fclose(fb);
        for (size_t i = 0; i < (size_t)w * h; ++i) {
            a[i * 4] = ra[i * 3]; a[i * 4 + 1] = ra[i * 3 + 1]; a[i * 4 + 2] = ra[i * 3 + 2]; a[i * 4 + 3] = 255;
            b[i * 4] = rb[i * 3]; b[i * 4 + 1] = rb[i * 3 + 1]; b[i * 4 + 2] = rb[i * 3 + 2]; b[i * 4 + 3] = 255;
        }
    } else {
        for (int y = 0; y < h; ++y)
            for (int x = 0; x < w; ++x) {
                unsigned char v0 = pixel(x, y, 0), v10 = pixel(x, y, 10);
                unsigned char* pa = &a[((size_t)y * w + x) * 4];
                unsigned char* pb = &b[((size_t)y * w + x) * 4];
                pa[0] = pa[1] = pa[2] = v0;
                pb[0] = pb[1] = pb[2] = v10;
            }
    }
    if (!interp(a.data(), b.data(), w, h, 0.5f, out.data())) { printf("SELFTEST: interp failed\n"); return 1; }

    std::string outpath = argc > 5 ? argv[5] : std::string(model) + "_interp.bmp";
    saveBmp(outpath.c_str(), out.data(), w, h);
    printf("RIFE SELFTEST OK (wrote %s)\n", outpath.c_str());
    return 0;
}
