// kernel32.dll, the system: the Windows version programs see, processor and memory facts, environment
// variables, drives and volumes, file times and attributes, and code pages.
//
// Programs ask "which Windows is this?": the answer is Windows 10 (version 10.0, build 19045), the
// version most software targets.

#include <cerrno>
#include <cstdlib>
#include <cstring>
#include <ctime>
#include <sys/stat.h>
#include <sys/statvfs.h>
#include <sys/time.h>
#include <unistd.h>

#include "handles.hpp"

namespace {
// 100-nanosecond intervals since 1601 <-> seconds since 1970.
uint64_t to_filetime(const timespec &t) {
    return (static_cast<uint64_t>(t.tv_sec) + 11644473600ull) * 10000000ull + t.tv_nsec / 100;
}
timespec from_filetime(uint64_t ft) {
    uint64_t since_1970 = ft / 10 - 11644473600000000ull;  // microseconds
    return timespec{static_cast<time_t>(since_1970 / 1000000), static_cast<long>(since_1970 % 1000000) * 1000};
}

DWORD attributes_of(const struct stat &st) {
    DWORD a = S_ISDIR(st.st_mode) ? 0x10 : 0x20;  // DIRECTORY or ARCHIVE
    if (!(st.st_mode & S_IWUSR)) a |= 0x01;      // READONLY
    return a;
}

std::string narrow(LPCWSTR w) {
    std::string s;
    for (; w && *w; w++) s += static_cast<char>(*w < 0x80 ? *w : '?');
    return s;
}
}  // namespace

extern "C" {

WINAPI UINT k32_SetErrorMode(UINT) { return 0; }
WINAPI UINT k32_GetACP() { return 65001; }  // UTF-8
WINAPI UINT k32_GetOEMCP() { return 65001; }
WINAPI BOOL k32_IsValidCodePage(UINT page) { return page == 65001 || page == 20127 || page == 1252 || page == 437; }
WINAPI BOOL k32_GetCPInfo(UINT, BYTE *info) {
    memset(info, 0, 20);
    info[0] = 4;    // MaxCharSize (UTF-8)
    info[4] = '?';  // DefaultChar
    return 1;
}
WINAPI BOOL k32_AreFileApisANSI() { return 1; }

// OSVERSIONINFOA (and the start of OSVERSIONINFOEXA): Windows 10.
WINAPI BOOL k32_GetVersionExA(DWORD *info) {
    info[1] = 10;     // major
    info[2] = 0;      // minor
    info[3] = 19045;  // build
    info[4] = 2;      // VER_PLATFORM_WIN32_NT
    memset(info + 5, 0, 128);
    return 1;
}
WINAPI BOOL k32_GetVersionExW(DWORD *info) {
    info[1] = 10;
    info[2] = 0;
    info[3] = 19045;
    info[4] = 2;
    memset(info + 5, 0, 256);
    return 1;
}
WINAPI DWORD k32_GetVersion() { return (19045u << 16) | (0 << 8) | 10; }

struct SystemInfo {
    WORD architecture, reserved;
    DWORD page_size;
    void *min_address, *max_address;
    uintptr_t active_processors;
    DWORD processor_count, processor_type, allocation_granularity;
    WORD level, revision;
};
static_assert(sizeof(SystemInfo) == 48);
WINAPI void k32_GetSystemInfo(SystemInfo *info) {
    long cpus = sysconf(_SC_NPROCESSORS_ONLN);
    if (cpus < 1) cpus = 1;
    *info = SystemInfo{9 /* AMD64 */, 0, 4096, reinterpret_cast<void *>(0x10000), reinterpret_cast<void *>(0x7FFFFFFEFFFF),
                       (1ull << cpus) - 1, static_cast<DWORD>(cpus), 8664, 65536, 6, 0};
}
WINAPI void k32_GetNativeSystemInfo(SystemInfo *info) { k32_GetSystemInfo(info); }
WINAPI BOOL k32_IsWow64Process(HANDLE, BOOL *wow64) {
    *wow64 = 0;
    return 1;
}
WINAPI BOOL k32_GetProcessAffinityMask(HANDLE, uintptr_t *process, uintptr_t *system) {
    *process = *system = 1;
    return 1;
}
WINAPI UINT k32_GetSystemDirectoryA(LPSTR buf, UINT size) {
    const char dir[] = "C:\\Windows\\System32";
    if (size < sizeof dir) return sizeof dir;
    memcpy(buf, dir, sizeof dir);
    return sizeof dir - 1;
}
WINAPI UINT k32_GetWindowsDirectoryA(LPSTR buf, UINT size) {
    const char dir[] = "C:\\Windows";
    if (size < sizeof dir) return sizeof dir;
    memcpy(buf, dir, sizeof dir);
    return sizeof dir - 1;
}

// Environment variables.
WINAPI DWORD k32_GetEnvironmentVariableA(LPCSTR name, LPSTR buf, DWORD size) {
    const char *v = getenv(name);
    if (!v) {
        set_last_error(203);  // ERROR_ENVVAR_NOT_FOUND
        return 0;
    }
    size_t len = strlen(v);
    if (len + 1 > size) return static_cast<DWORD>(len + 1);
    memcpy(buf, v, len + 1);
    return static_cast<DWORD>(len);
}
WINAPI DWORD k32_GetEnvironmentVariableW(LPCWSTR name, LPWSTR buf, DWORD size) {
    const char *v = getenv(narrow(name).c_str());
    if (!v) {
        set_last_error(203);
        return 0;
    }
    size_t len = strlen(v);
    if (len + 1 > size) return static_cast<DWORD>(len + 1);
    for (size_t i = 0; i <= len; i++) buf[i] = static_cast<unsigned char>(v[i]);
    return static_cast<DWORD>(len);
}
WINAPI BOOL k32_SetEnvironmentVariableA(LPCSTR name, LPCSTR value) {
    return (value ? setenv(name, value, 1) : unsetenv(name)) == 0;
}

// Drives: one drive, C:, which is /.
WINAPI DWORD k32_GetLogicalDrives() { return 1u << 2; }
WINAPI UINT k32_GetDriveTypeA(LPCSTR) { return 3; }  // DRIVE_FIXED
WINAPI BOOL k32_GetDiskFreeSpaceExA(LPCSTR path, uint64_t *available, uint64_t *total, uint64_t *free_bytes) {
    struct statvfs st;
    if (statvfs(path ? unix_path(path).c_str() : ".", &st) != 0) return set_error_from_errno(errno), 0;
    if (available) *available = static_cast<uint64_t>(st.f_bavail) * st.f_frsize;
    if (total) *total = static_cast<uint64_t>(st.f_blocks) * st.f_frsize;
    if (free_bytes) *free_bytes = static_cast<uint64_t>(st.f_bfree) * st.f_frsize;
    return 1;
}
WINAPI BOOL k32_GetDiskFreeSpaceExW(LPCWSTR path, uint64_t *a, uint64_t *t, uint64_t *f) {
    return k32_GetDiskFreeSpaceExA(path ? narrow(path).c_str() : nullptr, a, t, f);
}
WINAPI BOOL k32_GetVolumeInformationA(LPCSTR, LPSTR name, DWORD name_size, DWORD *serial, DWORD *max_component,
                                      DWORD *flags, LPSTR fs_name, DWORD fs_size) {
    if (name && name_size) strncpy(name, "KernelProject2", name_size);
    if (serial) *serial = 0x4B320001;
    if (max_component) *max_component = 255;
    if (flags) *flags = 0x3;  // case sensitive and case preserving
    if (fs_name && fs_size) strncpy(fs_name, "NTFS", fs_size);
    return 1;
}
WINAPI BOOL k32_GetVolumeInformationW(LPCWSTR, LPWSTR name, DWORD name_size, DWORD *serial, DWORD *max_component,
                                      DWORD *flags, LPWSTR fs_name, DWORD fs_size) {
    auto put = [](LPWSTR out, DWORD size, const char *s) {
        DWORD i = 0;
        for (; s[i] && i + 1 < size; i++) out[i] = static_cast<char16_t>(s[i]);
        if (size) out[i] = 0;
    };
    if (name) put(name, name_size, "KernelProject2");
    if (serial) *serial = 0x4B320001;
    if (max_component) *max_component = 255;
    if (flags) *flags = 0x3;
    if (fs_name) put(fs_name, fs_size, "NTFS");
    return 1;
}

// File attributes and times by name and by handle.
WINAPI BOOL k32_GetFileAttributesExA(LPCSTR name, int, uint8_t *out) {
    struct stat st;
    if (stat(unix_path(name).c_str(), &st) != 0) return set_error_from_errno(errno), 0;
    DWORD attrs = attributes_of(st);
    uint64_t created = to_filetime(st.st_ctim), accessed = to_filetime(st.st_atim), written = to_filetime(st.st_mtim);
    uint32_t high = static_cast<uint32_t>(st.st_size >> 32), low = static_cast<uint32_t>(st.st_size);
    memcpy(out, &attrs, 4);
    memcpy(out + 4, &created, 8);
    memcpy(out + 12, &accessed, 8);
    memcpy(out + 20, &written, 8);
    memcpy(out + 28, &high, 4);
    memcpy(out + 32, &low, 4);
    return 1;
}
WINAPI BOOL k32_GetFileInformationByHandle(HANDLE h, uint8_t *out) {
    struct stat st;
    if (fstat(handles::fd_of(h), &st) != 0) return set_error_from_errno(errno), 0;
    memset(out, 0, 52);
    DWORD attrs = attributes_of(st);
    uint64_t created = to_filetime(st.st_ctim), accessed = to_filetime(st.st_atim), written = to_filetime(st.st_mtim);
    uint32_t serial = static_cast<uint32_t>(st.st_dev), high = static_cast<uint32_t>(st.st_size >> 32),
             low = static_cast<uint32_t>(st.st_size), links = static_cast<uint32_t>(st.st_nlink),
             index_high = static_cast<uint32_t>(st.st_ino >> 32), index_low = static_cast<uint32_t>(st.st_ino);
    memcpy(out, &attrs, 4);
    memcpy(out + 4, &created, 8);
    memcpy(out + 12, &accessed, 8);
    memcpy(out + 20, &written, 8);
    memcpy(out + 28, &serial, 4);
    memcpy(out + 32, &high, 4);
    memcpy(out + 36, &low, 4);
    memcpy(out + 40, &links, 4);
    memcpy(out + 44, &index_high, 4);
    memcpy(out + 48, &index_low, 4);
    return 1;
}
WINAPI BOOL k32_SetFileAttributesA(LPCSTR name, DWORD attrs) {
    struct stat st;
    std::string path = unix_path(name);
    if (stat(path.c_str(), &st) != 0) return set_error_from_errno(errno), 0;
    mode_t mode = attrs & 0x01 ? st.st_mode & ~0222 : st.st_mode | S_IWUSR;
    return chmod(path.c_str(), mode & 07777) == 0;
}
WINAPI BOOL k32_SetFileTime(HANDLE h, const uint64_t *, const uint64_t *accessed, const uint64_t *written) {
    timespec times[2] = {{0, UTIME_OMIT}, {0, UTIME_OMIT}};
    if (accessed) times[0] = from_filetime(*accessed);
    if (written) times[1] = from_filetime(*written);
    return futimens(handles::fd_of(h), times) == 0;
}
WINAPI DWORD k32_GetCompressedFileSizeA(LPCSTR name, DWORD *high) {
    struct stat st;
    if (stat(unix_path(name).c_str(), &st) != 0) return set_error_from_errno(errno), 0xFFFFFFFF;
    if (high) *high = static_cast<DWORD>(st.st_size >> 32);
    return static_cast<DWORD>(st.st_size);
}
WINAPI BOOL k32_LockFileEx(HANDLE, DWORD, DWORD, DWORD, DWORD, void *) { return 1; }  // one user: no contention
WINAPI BOOL k32_UnlockFile(HANDLE, DWORD, DWORD, DWORD, DWORD) { return 1; }
WINAPI BOOL k32_UnlockFileEx(HANDLE, DWORD, DWORD, DWORD, void *) { return 1; }

// SYSTEMTIME, in UTC.
WINAPI BOOL k32_FileTimeToSystemTime(const uint64_t *ft, WORD *st) {
    timespec t = from_filetime(*ft);
    tm parts;
    gmtime_r(&t.tv_sec, &parts);
    WORD values[8] = {static_cast<WORD>(parts.tm_year + 1900), static_cast<WORD>(parts.tm_mon + 1),
                      static_cast<WORD>(parts.tm_wday), static_cast<WORD>(parts.tm_mday), static_cast<WORD>(parts.tm_hour),
                      static_cast<WORD>(parts.tm_min), static_cast<WORD>(parts.tm_sec),
                      static_cast<WORD>(t.tv_nsec / 1000000)};
    memcpy(st, values, sizeof values);
    return 1;
}
WINAPI void k32_GetSystemTime(WORD *st) {
    timespec now;
    clock_gettime(CLOCK_REALTIME, &now);
    uint64_t ft = to_filetime(now);
    k32_FileTimeToSystemTime(&ft, st);
}
WINAPI void k32_GetLocalTime(WORD *st) { k32_GetSystemTime(st); }  // the clock runs on UTC here
WINAPI BOOL k32_SetSystemTime(const WORD *) {
    set_last_error(1314);  // ERROR_PRIVILEGE_NOT_HELD: the clock comes from the machine
    return 0;
}
WINAPI BOOL k32_SleepEx(DWORD ms, BOOL) {
    timespec t{static_cast<time_t>(ms / 1000), static_cast<long>(ms % 1000) * 1000000};
    nanosleep(&t, nullptr);
    return 0;
}

}  // extern "C"

#define EXPORT(name) {#name, reinterpret_cast<void *>(&k32_##name)}

namespace {
const Export exports[] = {
    EXPORT(AreFileApisANSI), EXPORT(FileTimeToSystemTime), EXPORT(GetACP), EXPORT(GetCPInfo), EXPORT(GetCompressedFileSizeA),
    EXPORT(GetDiskFreeSpaceExA), EXPORT(GetDiskFreeSpaceExW), EXPORT(GetDriveTypeA), EXPORT(GetEnvironmentVariableA),
    EXPORT(GetEnvironmentVariableW), EXPORT(GetFileAttributesExA), EXPORT(GetFileInformationByHandle),
    EXPORT(GetLocalTime), EXPORT(GetLogicalDrives), EXPORT(GetNativeSystemInfo), EXPORT(GetOEMCP),
    EXPORT(GetProcessAffinityMask), EXPORT(GetSystemDirectoryA), EXPORT(GetSystemInfo), EXPORT(GetSystemTime),
    EXPORT(GetVersion), EXPORT(GetVersionExA), EXPORT(GetVersionExW), EXPORT(GetVolumeInformationA),
    EXPORT(GetVolumeInformationW), EXPORT(GetWindowsDirectoryA), EXPORT(IsValidCodePage), EXPORT(IsWow64Process),
    EXPORT(LockFileEx), EXPORT(SetEnvironmentVariableA), EXPORT(SetErrorMode), EXPORT(SetFileAttributesA),
    EXPORT(SetFileTime), EXPORT(SetSystemTime), EXPORT(SleepEx), EXPORT(UnlockFile), EXPORT(UnlockFileEx),
};
}  // namespace

const BuiltinDll kernel32_system_dll{"kernel32.dll", exports, sizeof exports / sizeof exports[0]};
