// Shared definitions for the Windows layer.
//
// Windows programs call their libraries with the Microsoft x64 calling convention (arguments in
// rcx, rdx, r8, r9 plus 32 bytes of "shadow space"), not the System V one Linux uses. GCC can
// compile a function either way; WINAPI marks the ones Windows code calls.
#pragma once

#include <cstddef>
#include <cstdint>

#define WINAPI __attribute__((ms_abi))

using BOOL = int;
using DWORD = uint32_t;
using UINT = unsigned int;
using WORD = uint16_t;
using BYTE = uint8_t;
using LONG = int32_t;
using ULONG = uint32_t;
using HANDLE = void *;
using HMODULE = void *;
using LPVOID = void *;
using LPCSTR = const char *;
using LPSTR = char *;
using WCHAR = char16_t;
using LPCWSTR = const char16_t *;
using LPWSTR = char16_t *;
using SIZE_T = size_t;

// One function a built-in DLL provides: its name and where it is. Data exports (variables) are
// listed the same way; the import table gets the variable's address.
struct Export {
    const char *name;
    void *address;
};

struct BuiltinDll {
    const char *name;     // lower case, e.g. "kernel32.dll"
    const Export *exports;
    size_t count;
};

// The built-in DLLs. kernel32 comes in parts (kernel32.cpp, files.cpp, threads.cpp), each listing
// its own functions under the same DLL name.
extern const BuiltinDll kernel32_dll;
extern const BuiltinDll kernel32_files_dll;
extern const BuiltinDll kernel32_threads_dll;
extern const BuiltinDll msvcrt_dll;

// The image's thread-local storage template (its .tls directory), copied for every thread.
struct TlsTemplate {
    bool present;
    uint64_t start, end, callbacks;
    uint32_t zero_fill;
};

// Process-wide facts the DLLs need.
namespace process {
extern int argc;
extern char **argv;
extern char **envp;
extern void *image_base;
extern size_t image_size;
extern const char *command_line;
extern const char *module_path;  // the .exe, as an absolute Windows path
extern TlsTemplate tls;
[[noreturn]] void exit(int code);
}  // namespace process

// The Thread Environment Block: Windows code reaches it through the gs segment (gs:[0x30] is its
// own address). Only the fields programs commonly read are given names here.
namespace teb {
constexpr size_t EXCEPTION_LIST = 0x00;
constexpr size_t STACK_BASE = 0x08;
constexpr size_t STACK_LIMIT = 0x10;
constexpr size_t SELF = 0x30;
constexpr size_t PROCESS_ID = 0x40;
constexpr size_t THREAD_ID = 0x48;
constexpr size_t TLS_POINTER = 0x58;
constexpr size_t PEB = 0x60;
constexpr size_t LAST_ERROR = 0x68;
constexpr size_t TLS_SLOTS = 0x1480;  // 64 TlsAlloc slots
constexpr size_t SIZE = 0x2000;

constexpr DWORD DLL_PROCESS_ATTACH = 1;
constexpr DWORD DLL_THREAD_ATTACH = 2;

// Sets up a TEB (and the shared PEB) for the calling thread, points gs at it, gives the thread its
// copy of the image's TLS variables, and runs the TLS callbacks with `reason`.
void attach_thread(DWORD reason);
uint8_t *current();
}  // namespace teb

inline void set_last_error(DWORD error) {
    *reinterpret_cast<DWORD *>(teb::current() + teb::LAST_ERROR) = error;
}

// Windows error codes the layer sets.
constexpr DWORD ERROR_FILE_NOT_FOUND = 2;
constexpr DWORD ERROR_PATH_NOT_FOUND = 3;
constexpr DWORD ERROR_ACCESS_DENIED = 5;
constexpr DWORD ERROR_INVALID_HANDLE = 6;
constexpr DWORD ERROR_NOT_ENOUGH_MEMORY = 8;
constexpr DWORD ERROR_NO_MORE_FILES = 18;
constexpr DWORD ERROR_FILE_EXISTS = 80;
constexpr DWORD ERROR_INVALID_PARAMETER = 87;
constexpr DWORD ERROR_INSUFFICIENT_BUFFER = 122;
constexpr DWORD ERROR_DIR_NOT_EMPTY = 145;
constexpr DWORD ERROR_ALREADY_EXISTS = 183;

// The Windows error for a Linux errno (sets it as the last error too).
DWORD set_error_from_errno(int e);

// Paths: Windows "C:\dirile" (any drive letter) to "/dir/file", and back as "C:\dirile".
#include <string>
std::string unix_path(const char *windows_path);
std::string windows_path(const std::string &unix_path);
