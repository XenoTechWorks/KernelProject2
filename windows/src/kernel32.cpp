// kernel32.dll: the core Windows API (memory, threads, errors, text conversion, console output).
// Each function follows the documented Windows behaviour, built on the Linux calls underneath.

#include <cerrno>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <ctime>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

#include "win.hpp"

namespace {

constexpr DWORD PAGE_NOACCESS = 0x01;
constexpr DWORD PAGE_READONLY = 0x02;
constexpr DWORD PAGE_READWRITE = 0x04;
constexpr DWORD PAGE_EXECUTE = 0x10;
constexpr DWORD PAGE_EXECUTE_READ = 0x20;
constexpr DWORD PAGE_EXECUTE_READWRITE = 0x40;

int to_linux_protection(DWORD p) {
    switch (p & 0xFF) {
        case PAGE_NOACCESS: return PROT_NONE;
        case PAGE_READONLY: return PROT_READ;
        case PAGE_READWRITE: return PROT_READ | PROT_WRITE;
        case PAGE_EXECUTE: return PROT_EXEC;
        case PAGE_EXECUTE_READ: return PROT_READ | PROT_EXEC;
        default: return PROT_READ | PROT_WRITE | PROT_EXEC;
    }
}

// Images are mapped read/write/execute (see main.cpp); remember what each page was changed to.
DWORD current_protection = PAGE_EXECUTE_READWRITE;

}  // namespace

extern "C" {

WINAPI DWORD k32_GetLastError() { return *reinterpret_cast<DWORD *>(teb::current() + teb::LAST_ERROR); }
WINAPI void k32_SetLastError(DWORD e) { set_last_error(e); }


WINAPI void k32_Sleep(DWORD ms) {
    timespec t{static_cast<time_t>(ms / 1000), static_cast<long>(ms % 1000) * 1000000};
    nanosleep(&t, nullptr);
}


// Thread-local slots (TlsAlloc and friends), stored in the TEB like on Windows.
WINAPI DWORD k32_TlsAlloc() {
    static DWORD next = 0;
    return next < 64 ? next++ : 0xFFFFFFFF;
}
WINAPI void *k32_TlsGetValue(DWORD i) {
    set_last_error(0);
    return i < 64 ? reinterpret_cast<void **>(teb::current() + teb::TLS_SLOTS)[i] : nullptr;
}
WINAPI BOOL k32_TlsSetValue(DWORD i, void *v) {
    if (i >= 64) return 0;
    reinterpret_cast<void **>(teb::current() + teb::TLS_SLOTS)[i] = v;
    return 1;
}
WINAPI BOOL k32_TlsFree(DWORD) { return 1; }

WINAPI BOOL k32_VirtualProtect(void *address, SIZE_T size, DWORD protection, DWORD *old) {
    if (old) *old = current_protection;
    uintptr_t start = reinterpret_cast<uintptr_t>(address) & ~0xFFFull;
    uintptr_t end = reinterpret_cast<uintptr_t>(address) + size;
    if (mprotect(reinterpret_cast<void *>(start), end - start, to_linux_protection(protection)) != 0) {
        set_last_error(ERROR_INVALID_PARAMETER);
        return 0;
    }
    current_protection = protection;
    return 1;
}

struct MemoryInfo {
    void *base;
    void *allocation_base;
    DWORD allocation_protect;
    WORD partition_id;
    SIZE_T region_size;
    DWORD state;
    DWORD protect;
    DWORD type;
};

WINAPI SIZE_T k32_VirtualQuery(const void *address, MemoryInfo *info, SIZE_T length) {
    if (length < sizeof(MemoryInfo)) {
        set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    }
    uintptr_t page = reinterpret_cast<uintptr_t>(address) & ~0xFFFull;
    uintptr_t image = reinterpret_cast<uintptr_t>(process::image_base);
    bool in_image = page >= image && page < image + process::image_size;
    *info = MemoryInfo{
        reinterpret_cast<void *>(page),
        in_image ? process::image_base : reinterpret_cast<void *>(page),
        PAGE_EXECUTE_READWRITE,
        0,
        4096,
        0x1000,                             // MEM_COMMIT
        current_protection,
        in_image ? 0x1000000u : 0x20000u,   // MEM_IMAGE or MEM_PRIVATE
    };
    return sizeof(MemoryInfo);
}

WINAPI void *k32_VirtualAlloc(void *address, SIZE_T size, DWORD, DWORD protection) {
    int flags = MAP_PRIVATE | MAP_ANONYMOUS | (address ? MAP_FIXED_NOREPLACE : 0);
    void *p = mmap(address, size, to_linux_protection(protection), flags, -1, 0);
    return p == MAP_FAILED ? nullptr : p;
}
WINAPI BOOL k32_VirtualFree(void *address, SIZE_T size, DWORD) { return munmap(address, size ? size : 4096) == 0; }

WINAPI BOOL k32_IsDBCSLeadByteEx(UINT, BYTE) { return 0; }

// Text conversion between UTF-8 / single-byte strings and Windows' UTF-16.
WINAPI int k32_MultiByteToWideChar(UINT, DWORD, LPCSTR in, int in_len, LPWSTR out, int out_len) {
    if (in_len < 0) in_len = static_cast<int>(strlen(in)) + 1;
    int n = 0;
    for (int i = 0; i < in_len;) {
        uint32_t c = static_cast<uint8_t>(in[i]);
        int extra = c >= 0xF0 ? 3 : c >= 0xE0 ? 2 : c >= 0xC0 ? 1 : 0;
        if (extra) c &= 0x3F >> extra;
        for (int k = 1; k <= extra && i + k < in_len; k++) c = (c << 6) | (static_cast<uint8_t>(in[i + k]) & 0x3F);
        i += extra + 1;
        int units = c >= 0x10000 ? 2 : 1;
        if (out_len) {
            if (n + units > out_len) {
                set_last_error(ERROR_INSUFFICIENT_BUFFER);
                return 0;
            }
            if (units == 2) {
                c -= 0x10000;
                out[n] = static_cast<WCHAR>(0xD800 + (c >> 10));
                out[n + 1] = static_cast<WCHAR>(0xDC00 + (c & 0x3FF));
            } else {
                out[n] = static_cast<WCHAR>(c);
            }
        }
        n += units;
    }
    return n;
}

WINAPI int k32_WideCharToMultiByte(UINT, DWORD, LPCWSTR in, int in_len, LPSTR out, int out_len, LPCSTR, BOOL *used_default) {
    if (used_default) *used_default = 0;
    if (in_len < 0) {
        in_len = 0;
        while (in[in_len]) in_len++;
        in_len++;
    }
    int n = 0;
    for (int i = 0; i < in_len; i++) {
        uint32_t c = in[i];
        if (c >= 0xD800 && c < 0xDC00 && i + 1 < in_len) c = 0x10000 + ((c - 0xD800) << 10) + (in[++i] - 0xDC00);
        char buf[4];
        int len = c < 0x80 ? 1 : c < 0x800 ? 2 : c < 0x10000 ? 3 : 4;
        if (len == 1) buf[0] = static_cast<char>(c);
        else {
            for (int k = len - 1; k > 0; k--) {
                buf[k] = static_cast<char>(0x80 | (c & 0x3F));
                c >>= 6;
            }
            buf[0] = static_cast<char>((0xF00 >> len) | c);
        }
        if (out_len) {
            if (n + len > out_len) {
                set_last_error(ERROR_INSUFFICIENT_BUFFER);
                return 0;
            }
            memcpy(out + n, buf, len);
        }
        n += len;
    }
    return n;
}

// FormatMessageA: the text for an error number (FORMAT_MESSAGE_FROM_SYSTEM), into the caller's
// buffer or a new one (FORMAT_MESSAGE_ALLOCATE_BUFFER, freed with LocalFree).
WINAPI DWORD k32_FormatMessageA(DWORD flags, const void *, DWORD id, DWORD, LPSTR buf, DWORD size, void *) {
    const char *known = id == 0 ? "The operation completed successfully." : id == 2 ? "The system cannot find the file specified." :
                        id == 3 ? "The system cannot find the path specified." : id == 5 ? "Access is denied." :
                        id == 6 ? "The handle is invalid." : id == 8 ? "Not enough memory resources are available." :
                        id == 87 ? "The parameter is incorrect." : nullptr;
    char text[96];
    if (known) snprintf(text, sizeof text, "%s\r\n", known);
    else snprintf(text, sizeof text, "Error %u.\r\n", id);
    size_t len = strlen(text);
    if (flags & 0x100) {  // FORMAT_MESSAGE_ALLOCATE_BUFFER: `buf` is where to store the pointer
        char *copy = static_cast<char *>(malloc(len + 1));
        memcpy(copy, text, len + 1);
        *reinterpret_cast<char **>(buf) = copy;
        return static_cast<DWORD>(len);
    }
    if (len + 1 > size) return 0;
    memcpy(buf, text, len + 1);
    return static_cast<DWORD>(len);
}
WINAPI void *k32_LocalFree(void *p) {
    free(p);
    return nullptr;
}
WINAPI void *k32_LocalAlloc(UINT flags, SIZE_T size) { return flags & 0x40 ? calloc(1, size) : malloc(size); }

// Process basics.
WINAPI void k32_ExitProcess(UINT code) { process::exit(static_cast<int>(code)); }
WINAPI LPSTR k32_GetCommandLineA() { return const_cast<LPSTR>(process::command_line); }
WINAPI DWORD k32_GetCurrentProcessId() { return static_cast<DWORD>(getpid()); }
WINAPI DWORD k32_GetCurrentThreadId() { return static_cast<DWORD>(syscall(SYS_gettid)); }
WINAPI HANDLE k32_GetCurrentProcess() { return reinterpret_cast<HANDLE>(-1); }

WINAPI DWORD k32_GetTickCount() {
    timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return static_cast<DWORD>(t.tv_sec * 1000 + t.tv_nsec / 1000000);
}
WINAPI BOOL k32_QueryPerformanceCounter(int64_t *count) {
    timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    *count = t.tv_sec * 1000000000LL + t.tv_nsec;
    return 1;
}
WINAPI BOOL k32_QueryPerformanceFrequency(int64_t *frequency) {
    *frequency = 1000000000LL;
    return 1;
}
// 100-nanosecond intervals since 1601, Windows' clock.
WINAPI void k32_GetSystemTimeAsFileTime(uint64_t *out) {
    timespec t;
    clock_gettime(CLOCK_REALTIME, &t);
    *out = (static_cast<uint64_t>(t.tv_sec) + 11644473600ull) * 10000000ull + t.tv_nsec / 100;
}

}  // extern "C"

#define EXPORT(name) {#name, reinterpret_cast<void *>(&k32_##name)}

namespace {
const Export exports[] = {
    EXPORT(ExitProcess), EXPORT(FormatMessageA), EXPORT(LocalAlloc), EXPORT(LocalFree), EXPORT(GetCommandLineA),
    EXPORT(GetCurrentProcess), EXPORT(GetCurrentProcessId), EXPORT(GetCurrentThreadId), EXPORT(GetLastError),
    EXPORT(GetSystemTimeAsFileTime),
    EXPORT(GetTickCount), EXPORT(IsDBCSLeadByteEx), EXPORT(MultiByteToWideChar),
    EXPORT(QueryPerformanceCounter), EXPORT(QueryPerformanceFrequency), EXPORT(SetLastError),
    EXPORT(Sleep), EXPORT(TlsAlloc), EXPORT(TlsFree), EXPORT(TlsGetValue),
    EXPORT(TlsSetValue), EXPORT(VirtualAlloc), EXPORT(VirtualFree),
    EXPORT(VirtualProtect), EXPORT(VirtualQuery), EXPORT(WideCharToMultiByte), };
}  // namespace

const BuiltinDll kernel32_dll{"kernel32.dll", exports, sizeof exports / sizeof exports[0]};
