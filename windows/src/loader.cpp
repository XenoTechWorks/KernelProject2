// The PE loader: maps .exe and .dll images, applies base relocations, reads export tables, resolves
// imports (against our built-in DLLs, or by loading the program's own DLLs found next to it), sets
// up each image's thread-local storage slot, and runs DllMain. Also kernel32's module functions:
// LoadLibrary, GetProcAddress, GetModuleHandle, GetModuleFileName, FreeLibrary.

#include "loader.hpp"

#include <algorithm>
#include <cstdio>
#include <cstring>
#include <dirent.h>
#include <fcntl.h>
#include <strings.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>
#include <vector>

namespace {

std::vector<Module *> loaded;

template <typename T> T read(const uint8_t *p, size_t at) {
    T v;
    memcpy(&v, p + at, sizeof v);
    return v;
}

std::string lower(std::string s) {
    for (char &c : s) c = static_cast<char>(tolower(static_cast<unsigned char>(c)));
    return s;
}

std::string file_name(const std::string &path) {
    size_t slash = path.find_last_of("/\\");
    return slash == std::string::npos ? path : path.substr(slash + 1);
}

// "MATHLIB" and "mathlib.dll" name the same DLL.
std::string dll_name(const char *name) {
    std::string n = lower(file_name(name));
    if (n.find('.') == std::string::npos) n += ".dll";
    return n;
}

const BuiltinDll *const builtins[] = {&kernel32_dll, &kernel32_files_dll, &kernel32_threads_dll, &kernel32_loader_dll,
                                      &kernel32_seh_dll, &msvcrt_dll};

bool is_ucrt(const std::string &name) { return name.starts_with("api-ms-win-crt-") || name == "ucrtbase.dll"; }

bool is_builtin(const std::string &name) {
    if (is_ucrt(name)) return true;
    for (const BuiltinDll *d : builtins)
        if (name == d->name) return true;
    return false;
}

void *find_in(const BuiltinDll *d, const char *name) {
    for (size_t i = 0; i < d->count; i++)
        if (strcmp(d->exports[i].name, name) == 0) return d->exports[i].address;
    return nullptr;
}

void *find_builtin(const std::string &dll, const char *name) {
    // Windows resolves every api-ms-win-crt-*.dll "API set" to ucrtbase.dll, whose C functions are
    // msvcrt's.
    if (is_ucrt(dll)) {
        if (void *f = find_in(&ucrt_dll, name)) return f;
        return find_in(&msvcrt_dll, name);
    }
    for (const BuiltinDll *d : builtins)
        if (dll == d->name)
            if (void *f = find_in(d, name)) return f;
    return nullptr;
}

// Called (through a per-function stub) when a program uses a function we don't have yet.
extern "C" [[noreturn]] void missing_function(const char *name) {
    fprintf(stderr, "winload: this program called %s, which isn't implemented yet\n", name);
    _exit(127);
}

// A tiny piece of code per missing function that reports which one it was.
void *missing_stub(const std::string &dll, const std::string &name) {
    static uint8_t *code;
    static size_t used = 4096;
    if (used + 32 > 4096) {
        code = static_cast<uint8_t *>(mmap(nullptr, 4096, PROT_READ | PROT_WRITE | PROT_EXEC, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0));
        used = 0;
    }
    std::string *label = new std::string(dll + "!" + name);
    uint64_t text = reinterpret_cast<uint64_t>(label->c_str());
    uint64_t target = reinterpret_cast<uint64_t>(&missing_function);
    uint8_t bytes[] = {0x48, 0xBF, 0, 0, 0, 0, 0, 0, 0, 0,   // mov rdi, imm64
                       0x48, 0xB8, 0, 0, 0, 0, 0, 0, 0, 0,   // mov rax, imm64
                       0x48, 0x83, 0xE4, 0xF0,               // and rsp, -16
                       0x48, 0x83, 0xEC, 0x08,               // sub rsp, 8 (as if called)
                       0xFF, 0xE0};                          // jmp rax
    memcpy(bytes + 2, &text, 8);
    memcpy(bytes + 12, &target, 8);
    memcpy(code + used, bytes, sizeof bytes);
    used += 32;
    return code + used - 32;
}

Module *find_loaded(const std::string &name) {
    for (Module *m : loaded)
        if (m->name == name) return m;
    return nullptr;
}

// Where Windows would look for a DLL: next to the program, the current directory, then the system
// folders (here /bin and /usr/lib/windows).
std::string find_dll_file(const std::string &name) {
    if (name.find('/') != std::string::npos || name.find('\\') != std::string::npos) return unix_path(name.c_str());
    std::vector<std::string> dirs;
    if (!loaded.empty()) {
        std::string exe = loaded[0]->path;
        dirs.push_back(exe.substr(0, exe.rfind('/') == std::string::npos ? 0 : exe.rfind('/')));
    }
    dirs.push_back(".");
    dirs.push_back("/bin");
    dirs.push_back("/usr/lib/windows");
    for (const std::string &dir : dirs) {
        DIR *d = opendir(dir.empty() ? "/" : dir.c_str());
        if (!d) continue;
        std::string found;
        while (dirent *e = readdir(d))
            if (strcasecmp(e->d_name, name.c_str()) == 0) {  // Windows names ignore case
                found = dir + "/" + e->d_name;
                break;
            }
        closedir(d);
        if (!found.empty()) return found;
    }
    return "";
}

void *export_by_ordinal(Module *m, uint32_t ordinal);
void *export_by_name(Module *m, const char *name);

// An export's address; forwarded exports ("OTHER.Function") are followed.
void *export_address(Module *m, uint32_t rva) {
    if (rva >= m->export_rva && rva < m->export_rva + m->export_size) {
        std::string forward = reinterpret_cast<const char *>(m->base + rva);
        size_t dot = forward.find('.');
        std::string dll = dll_name(forward.substr(0, dot).c_str());
        std::string function = forward.substr(dot + 1);
        std::string error;
        if (is_builtin(dll)) return find_builtin(dll, function.c_str());
        Module *target = find_loaded(dll);
        if (!target) target = loader::load(find_dll_file(dll), error);
        if (!target) return nullptr;
        if (function.starts_with("#")) return export_by_ordinal(target, std::stoul(function.substr(1)));
        return export_by_name(target, function.c_str());
    }
    return m->base + rva;
}

void *export_by_ordinal(Module *m, uint32_t ordinal) {
    if (!m->export_rva) return nullptr;
    uint32_t base = read<uint32_t>(m->base, m->export_rva + 16);
    uint32_t count = read<uint32_t>(m->base, m->export_rva + 20);
    uint32_t functions = read<uint32_t>(m->base, m->export_rva + 28);
    if (ordinal < base || ordinal - base >= count) return nullptr;
    uint32_t rva = read<uint32_t>(m->base, functions + (ordinal - base) * 4);
    return rva ? export_address(m, rva) : nullptr;
}

void *export_by_name(Module *m, const char *name) {
    if (!m->export_rva) return nullptr;
    uint32_t names = read<uint32_t>(m->base, m->export_rva + 24);
    uint32_t name_table = read<uint32_t>(m->base, m->export_rva + 32);
    uint32_t ordinal_table = read<uint32_t>(m->base, m->export_rva + 36);
    uint32_t functions = read<uint32_t>(m->base, m->export_rva + 28);
    // The name table is sorted, so search it by halves.
    uint32_t lo = 0, hi = names;
    while (lo < hi) {
        uint32_t mid = (lo + hi) / 2;
        const char *candidate = reinterpret_cast<const char *>(m->base + read<uint32_t>(m->base, name_table + mid * 4));
        int c = strcmp(candidate, name);
        if (c == 0) {
            uint16_t index = read<uint16_t>(m->base, ordinal_table + mid * 2);
            return export_address(m, read<uint32_t>(m->base, functions + index * 4));
        }
        if (c < 0) lo = mid + 1;
        else hi = mid;
    }
    return nullptr;
}

bool resolve_imports(Module *m, std::string &error) {
    uint32_t rva = read<uint32_t>(m->base, read<uint32_t>(m->base, 0x3C) + 24 + 112 + 8);
    if (!rva) return true;
    for (uint32_t d = rva; read<uint32_t>(m->base, d + 12) != 0; d += 20) {
        std::string dll = dll_name(reinterpret_cast<const char *>(m->base + read<uint32_t>(m->base, d + 12)));
        uint32_t names = read<uint32_t>(m->base, d);
        uint32_t slots = read<uint32_t>(m->base, d + 16);
        if (names == 0) names = slots;
        Module *target = nullptr;
        if (!is_builtin(dll)) {
            target = find_loaded(dll);
            if (!target) {
                std::string path = find_dll_file(dll);
                if (path.empty()) {
                    error = m->name + " needs " + dll + ", which wasn't found";
                    return false;
                }
                target = loader::load(path, error);
                if (!target) return false;
            }
        }
        for (size_t i = 0;; i++) {
            uint64_t thunk = read<uint64_t>(m->base, names + i * 8);
            if (thunk == 0) break;
            void *address;
            if (thunk >> 63) {
                uint32_t ordinal = thunk & 0xFFFF;
                address = target ? export_by_ordinal(target, ordinal) : nullptr;
                if (!address) address = missing_stub(dll, "#" + std::to_string(ordinal));
            } else {
                const char *name = reinterpret_cast<const char *>(m->base + (thunk & 0x7FFFFFFF) + 2);
                address = target ? export_by_name(target, name) : find_builtin(dll, name);
                if (!address) address = missing_stub(dll, name);
            }
            memcpy(m->base + slots + i * 8, &address, 8);
        }
    }
    return true;
}

}  // namespace

Module *loader::load(const std::string &path, std::string &error) {
    int fd = open(path.c_str(), O_RDONLY | O_CLOEXEC);
    if (fd < 0) {
        error = "can't open " + path;
        return nullptr;
    }
    struct stat st;
    fstat(fd, &st);
    std::vector<uint8_t> file(st.st_size);
    for (size_t done = 0; done < file.size();) {
        ssize_t n = ::read(fd, file.data() + done, file.size() - done);
        if (n <= 0) break;
        done += n;
    }
    close(fd);
    const uint8_t *f = file.data();
    if (file.size() < 0x40 || f[0] != 'M' || f[1] != 'Z') {
        error = "not a Windows program: " + path;
        return nullptr;
    }
    uint32_t pe = read<uint32_t>(f, 0x3C);
    if (pe + 0x108 > file.size() || memcmp(f + pe, "PE\0\0", 4) != 0) {
        error = "no PE header in " + path;
        return nullptr;
    }
    size_t opt = pe + 24;
    if (read<uint16_t>(f, pe + 4) != 0x8664 || read<uint16_t>(f, opt) != 0x20B) {
        error = "only 64-bit x86 programs are supported: " + path;
        return nullptr;
    }
    uint16_t sections = read<uint16_t>(f, pe + 6);
    uint16_t optional_size = read<uint16_t>(f, pe + 20);
    uint64_t preferred = read<uint64_t>(f, opt + 24);
    uint32_t image_size = read<uint32_t>(f, opt + 56);
    uint32_t headers_size = read<uint32_t>(f, opt + 60);
    auto directory = [&](int i) { return std::pair{read<uint32_t>(f, opt + 112 + i * 8), read<uint32_t>(f, opt + 116 + i * 8)}; };

    // Map it: everything readable, writable and executable for now (the C runtime changes
    // protections itself with VirtualProtect where it cares).
    const int prot = PROT_READ | PROT_WRITE | PROT_EXEC;
    void *at = mmap(reinterpret_cast<void *>(preferred), image_size, prot, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED_NOREPLACE, -1, 0);
    if (at == MAP_FAILED || at != reinterpret_cast<void *>(preferred)) at = mmap(nullptr, image_size, prot, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (at == MAP_FAILED) {
        error = "out of memory";
        return nullptr;
    }
    auto *base = static_cast<uint8_t *>(at);
    memcpy(base, f, std::min<size_t>(headers_size, file.size()));
    size_t section_table = opt + optional_size;
    for (int i = 0; i < sections; i++) {
        size_t s = section_table + i * 40;
        uint32_t virtual_address = read<uint32_t>(f, s + 12);
        uint32_t raw_size = read<uint32_t>(f, s + 16);
        uint32_t raw_offset = read<uint32_t>(f, s + 20);
        if (raw_size && raw_offset + raw_size <= file.size()) memcpy(base + virtual_address, f + raw_offset, raw_size);
    }

    // Moved from its preferred address? Fix up every absolute address it contains.
    int64_t delta = reinterpret_cast<int64_t>(base) - static_cast<int64_t>(preferred);
    if (delta != 0) {
        auto [rva, size] = directory(5);
        if (rva == 0) {
            error = path + " can't load at its preferred address, and it has no relocations";
            return nullptr;
        }
        for (uint32_t block = 0; block + 8 <= size;) {
            uint32_t page = read<uint32_t>(base, rva + block);
            uint32_t block_size = read<uint32_t>(base, rva + block + 4);
            if (block_size < 8) break;
            for (uint32_t e = 8; e + 2 <= block_size; e += 2) {
                uint16_t entry = read<uint16_t>(base, rva + block + e);
                if (entry >> 12 == 10) *reinterpret_cast<uint64_t *>(base + page + (entry & 0xFFF)) += delta;  // DIR64
            }
            block += block_size;
        }
    }

    auto *m = new Module();
    char *absolute = realpath(path.c_str(), nullptr);
    m->path = absolute ? absolute : path;
    free(absolute);
    m->name = lower(file_name(m->path));
    m->base = base;
    m->size = image_size;
    m->entry_rva = read<uint32_t>(f, opt + 16);
    m->is_dll = read<uint16_t>(f, pe + 22) & 0x2000;  // IMAGE_FILE_DLL
    std::tie(m->export_rva, m->export_size) = directory(0);
    loaded.push_back(m);

    // Thread-local storage: each image with a .tls section gets the next slot in every thread's
    // TLS array, and is told its slot number.
    if (auto [tls_rva, tls_size] = directory(9); tls_rva) {
        uint32_t index = static_cast<uint32_t>(process::tls.size());
        process::tls.push_back({base, read<uint64_t>(base, tls_rva), read<uint64_t>(base, tls_rva + 8),
                                read<uint64_t>(base, tls_rva + 24), read<uint32_t>(base, tls_rva + 32)});
        if (uint64_t index_address = read<uint64_t>(base, tls_rva + 16)) *reinterpret_cast<uint32_t *>(index_address) = index;
        if (teb::current()) teb::add_tls_slot(index);  // loaded at run time: give this thread its copy
    }

    if (!resolve_imports(m, error)) return nullptr;
    return m;
}

void loader::attach_all() {
    // Load order puts a DLL's dependencies after it, so attach in reverse: dependencies first.
    for (auto it = loaded.rbegin(); it != loaded.rend(); ++it) {
        Module *m = *it;
        if (!m->is_dll || m->attached) continue;
        m->attached = true;
        if (m->entry_rva) {
            using DllMain = BOOL(WINAPI *)(void *, DWORD, void *);
            reinterpret_cast<DllMain>(m->base + m->entry_rva)(m->base, teb::DLL_PROCESS_ATTACH, nullptr);
        }
    }
}

const std::vector<Module *> &loader::modules() { return loaded; }

// ── kernel32: modules ───────────────────────────────────────────────────────────────────────────

namespace {
constexpr DWORD ERROR_MOD_NOT_FOUND = 126;
constexpr DWORD ERROR_PROC_NOT_FOUND = 127;

// Built-in DLLs have no image; their handle is the address of their table.
const BuiltinDll *builtin_for_handle(HMODULE h) {
    for (const BuiltinDll *d : builtins)
        if (static_cast<const void *>(d) == h) return d;
    return h == &ucrt_dll ? &ucrt_dll : nullptr;
}

Module *module_for_handle(HMODULE h) {
    if (!h) return loaded.empty() ? nullptr : loaded[0];
    for (Module *m : loaded)
        if (m->base == h) return m;
    return nullptr;
}

HMODULE handle_for_name(const std::string &name) {
    std::string n = dll_name(name.c_str());
    if (Module *m = find_loaded(n)) return m->base;
    if (is_ucrt(n)) return const_cast<BuiltinDll *>(&ucrt_dll);
    for (const BuiltinDll *d : builtins)
        if (n == d->name) return const_cast<BuiltinDll *>(d);
    return nullptr;
}

std::string narrow(LPCWSTR w) {
    std::string s;
    for (; w && *w; w++) s += static_cast<char>(*w < 0x80 ? *w : '?');
    return s;
}
}  // namespace

extern "C" {

WINAPI HMODULE k32_GetModuleHandleA(LPCSTR name) {
    if (!name) return loaded.empty() ? nullptr : loaded[0]->base;
    HMODULE h = handle_for_name(name);
    if (!h) set_last_error(ERROR_MOD_NOT_FOUND);
    return h;
}
WINAPI HMODULE k32_GetModuleHandleW(LPCWSTR name) { return k32_GetModuleHandleA(name ? narrow(name).c_str() : nullptr); }

WINAPI HMODULE k32_LoadLibraryA(LPCSTR name) {
    if (HMODULE h = handle_for_name(name)) return h;
    std::string path = find_dll_file(file_name(name).find('.') == std::string::npos && !strpbrk(name, "/\\") ? dll_name(name) : name);
    std::string error;
    Module *m = path.empty() ? nullptr : loader::load(path, error);
    if (!m) {
        set_last_error(ERROR_MOD_NOT_FOUND);
        return nullptr;
    }
    loader::attach_all();
    return m->base;
}
WINAPI HMODULE k32_LoadLibraryW(LPCWSTR name) { return k32_LoadLibraryA(narrow(name).c_str()); }
WINAPI HMODULE k32_LoadLibraryExA(LPCSTR name, HANDLE, DWORD) { return k32_LoadLibraryA(name); }
WINAPI HMODULE k32_LoadLibraryExW(LPCWSTR name, HANDLE, DWORD) { return k32_LoadLibraryA(narrow(name).c_str()); }
WINAPI BOOL k32_FreeLibrary(HMODULE) { return 1; }  // images stay loaded

WINAPI void *k32_GetProcAddress(HMODULE h, LPCSTR name) {
    bool by_ordinal = reinterpret_cast<uintptr_t>(name) < 0x10000;
    void *address = nullptr;
    if (const BuiltinDll *d = builtin_for_handle(h)) {
        if (!by_ordinal) address = find_builtin(d->name, name);
    } else if (Module *m = module_for_handle(h)) {
        address = by_ordinal ? export_by_ordinal(m, static_cast<uint32_t>(reinterpret_cast<uintptr_t>(name)))
                             : export_by_name(m, name);
    }
    if (!address) set_last_error(ERROR_PROC_NOT_FOUND);
    return address;
}

WINAPI DWORD k32_GetModuleFileNameA(HMODULE h, LPSTR buf, DWORD size) {
    Module *m = module_for_handle(h);
    if (!m) {
        set_last_error(ERROR_MOD_NOT_FOUND);
        return 0;
    }
    std::string path = windows_path(m->path);
    if (size == 0) return 0;
    size_t n = std::min<size_t>(path.size(), size - 1);
    memcpy(buf, path.c_str(), n);
    buf[n] = 0;
    if (n < path.size()) set_last_error(ERROR_INSUFFICIENT_BUFFER);
    return static_cast<DWORD>(n);
}

}  // extern "C"

#define EXPORT(name) {#name, reinterpret_cast<void *>(&k32_##name)}

namespace {
const Export exports[] = {
    EXPORT(FreeLibrary), EXPORT(GetModuleFileNameA), EXPORT(GetModuleHandleA), EXPORT(GetModuleHandleW),
    EXPORT(GetProcAddress), EXPORT(LoadLibraryA), EXPORT(LoadLibraryExA), EXPORT(LoadLibraryExW), EXPORT(LoadLibraryW),
};
}  // namespace

const BuiltinDll kernel32_loader_dll{"kernel32.dll", exports, sizeof exports / sizeof exports[0]};
