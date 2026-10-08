// winload: runs a Windows program (a PE .exe) as a normal process.
//
//   winload program.exe [arguments...]
//
// The kernel starts this automatically when a .exe is run, the way it starts the interpreter named
// in a #! line. Steps, as Windows' own loader does them:
//   1. map the image at its preferred address (or anywhere, then apply its base relocations);
//   2. point each imported function at our built-in DLLs;
//   3. set up thread-local storage, the TEB and PEB, and run the TLS callbacks;
//   4. jump to the entry point.

#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <strings.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <fcntl.h>
#include <unistd.h>
#include <vector>

#include "win.hpp"

namespace process {
int argc;
char **argv;
char **envp;
void *image_base;
size_t image_size;
const char *command_line;
const char *module_path;
}  // namespace process

namespace {

[[noreturn]] void fail(const char *what, const char *detail = "") {
    fprintf(stderr, "winload: %s%s\n", what, detail);
    exit(126);
}

template <typename T> T read(const uint8_t *p, size_t at) {
    T v;
    memcpy(&v, p + at, sizeof v);
    return v;
}

const BuiltinDll *const dlls[] = {&kernel32_dll, &kernel32_files_dll, &kernel32_threads_dll, &msvcrt_dll};

void *find_export(const char *dll, const char *name) {
    for (const BuiltinDll *d : dlls) {
        if (strcasecmp(d->name, dll) != 0) continue;
        for (size_t i = 0; i < d->count; i++)
            if (strcmp(d->exports[i].name, name) == 0) return d->exports[i].address;
    }
    return nullptr;
}

// Called (through a per-function stub) when a program uses a function we don't have yet.
extern "C" [[noreturn]] void missing_function(const char *name) {
    fprintf(stderr, "winload: this program called %s, which isn't implemented yet\n", name);
    _exit(127);
}

// A tiny piece of code per missing function: `mov rdi, name; mov rax, missing_function; jmp rax`.
void *missing_stub(const char *dll, const char *name) {
    static uint8_t *code;
    static size_t used = 4096;
    if (used + 32 > 4096) {
        code = static_cast<uint8_t *>(mmap(nullptr, 4096, PROT_READ | PROT_WRITE | PROT_EXEC, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0));
        used = 0;
    }
    std::string *label = new std::string(std::string(dll) + "!" + name);
    uint8_t *stub = code + used;
    uint64_t text = reinterpret_cast<uint64_t>(label->c_str());
    uint64_t target = reinterpret_cast<uint64_t>(&missing_function);
    uint8_t bytes[] = {0x48, 0xBF, 0, 0, 0, 0, 0, 0, 0, 0,   // mov rdi, imm64
                       0x48, 0xB8, 0, 0, 0, 0, 0, 0, 0, 0,   // mov rax, imm64
                       0x48, 0x83, 0xE4, 0xF0,               // and rsp, -16
                       0x48, 0x83, 0xEC, 0x08,               // sub rsp, 8 (as if called)
                       0xFF, 0xE0};                          // jmp rax
    memcpy(bytes + 2, &text, 8);
    memcpy(bytes + 12, &target, 8);
    memcpy(stub, bytes, sizeof bytes);
    used += 32;
    return stub;
}

std::vector<uint8_t> read_file(const char *path) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) fail("can't open ", path);
    struct stat st;
    fstat(fd, &st);
    std::vector<uint8_t> data(st.st_size);
    size_t done = 0;
    while (done < data.size()) {
        ssize_t n = ::read(fd, data.data() + done, data.size() - done);
        if (n <= 0) fail("can't read ", path);
        done += n;
    }
    close(fd);
    return data;
}

std::string quote_command_line(int argc, char **argv) {
    std::string line;
    for (int i = 0; i < argc; i++) {
        if (i) line += ' ';
        bool spaces = strchr(argv[i], ' ') != nullptr;
        if (spaces) line += '"';
        line += argv[i];
        if (spaces) line += '"';
    }
    return line;
}

}  // namespace

void process::exit(int code) {
    fflush(stdout);
    _exit(code);
}

int main(int argc, char **argv, char **envp) {
    if (argc < 2) {
        fprintf(stderr, "usage: winload program.exe [arguments...]\n");
        return 2;
    }
    std::vector<uint8_t> file = read_file(argv[1]);
    const uint8_t *f = file.data();
    if (file.size() < 0x40 || f[0] != 'M' || f[1] != 'Z') fail("not a Windows program: ", argv[1]);
    uint32_t pe = read<uint32_t>(f, 0x3C);
    if (pe + 0x108 > file.size() || memcmp(f + pe, "PE\0\0", 4) != 0) fail("no PE header in ", argv[1]);
    uint16_t machine = read<uint16_t>(f, pe + 4);
    uint16_t sections = read<uint16_t>(f, pe + 6);
    uint16_t optional_size = read<uint16_t>(f, pe + 20);
    size_t opt = pe + 24;
    if (machine != 0x8664 || read<uint16_t>(f, opt) != 0x20B) fail("only 64-bit x86 programs are supported: ", argv[1]);

    uint64_t preferred = read<uint64_t>(f, opt + 24);
    uint32_t entry_rva = read<uint32_t>(f, opt + 16);
    uint32_t image_size = read<uint32_t>(f, opt + 56);
    uint32_t headers_size = read<uint32_t>(f, opt + 60);
    auto directory = [&](int i) { return std::pair{read<uint32_t>(f, opt + 112 + i * 8), read<uint32_t>(f, opt + 116 + i * 8)}; };

    // 1. Map it: everything readable, writable and executable for now (the C runtime changes
    //    protections itself with VirtualProtect where it cares).
    const int prot = PROT_READ | PROT_WRITE | PROT_EXEC;
    void *at = mmap(reinterpret_cast<void *>(preferred), image_size, prot, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED_NOREPLACE, -1, 0);
    if (at == MAP_FAILED || at != reinterpret_cast<void *>(preferred))
        at = mmap(nullptr, image_size, prot, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (at == MAP_FAILED) fail("out of memory");
    uint8_t *base = static_cast<uint8_t *>(at);
    memcpy(base, f, headers_size < file.size() ? headers_size : file.size());
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
        if (rva == 0) fail("can't load at its preferred address, and it has no relocations");
        for (uint32_t block = 0; block + 8 <= size;) {
            uint32_t page = read<uint32_t>(base, rva + block);
            uint32_t block_size = read<uint32_t>(base, rva + block + 4);
            if (block_size < 8) break;
            for (uint32_t e = 8; e + 2 <= block_size; e += 2) {
                uint16_t entry = read<uint16_t>(base, rva + block + e);
                if (entry >> 12 == 10) {  // IMAGE_REL_BASED_DIR64
                    uint64_t *p = reinterpret_cast<uint64_t *>(base + page + (entry & 0xFFF));
                    *p += delta;
                }
            }
            block += block_size;
        }
    }

    process::argc = argc - 1;
    process::argv = argv + 1;
    process::envp = envp;
    process::image_base = base;
    process::image_size = image_size;
    process::command_line = strdup(quote_command_line(argc - 1, argv + 1).c_str());
    char *absolute = realpath(argv[1], nullptr);
    process::module_path = strdup(windows_path(absolute ? absolute : argv[1]).c_str());

    // 2. Imports.
    if (auto [rva, size] = directory(1); rva) {
        for (uint32_t d = rva; read<uint32_t>(base, d + 12) != 0; d += 20) {
            const char *dll = reinterpret_cast<const char *>(base + read<uint32_t>(base, d + 12));
            uint32_t names = read<uint32_t>(base, d);
            uint32_t slots = read<uint32_t>(base, d + 16);
            if (names == 0) names = slots;
            for (size_t i = 0;; i++) {
                uint64_t thunk = read<uint64_t>(base, names + i * 8);
                if (thunk == 0) break;
                void *address;
                if (thunk >> 63) {
                    char ordinal[32];
                    snprintf(ordinal, sizeof ordinal, "#%u", static_cast<unsigned>(thunk & 0xFFFF));
                    address = missing_stub(dll, ordinal);
                } else {
                    const char *name = reinterpret_cast<const char *>(base + (thunk & 0x7FFFFFFF) + 2);
                    address = find_export(dll, name);
                    if (!address) address = missing_stub(dll, name);
                }
                memcpy(base + slots + i * 8, &address, 8);
            }
        }
    }

    // 3. Thread-local storage: remember the image's template (every thread gets a copy), then set up
    //    this thread's TEB and run the TLS callbacks.
    if (auto [tls_rva, tls_size] = directory(9); tls_rva) {
        process::tls = {true, read<uint64_t>(base, tls_rva), read<uint64_t>(base, tls_rva + 8),
                        read<uint64_t>(base, tls_rva + 24), read<uint32_t>(base, tls_rva + 32)};
        if (uint64_t index_address = read<uint64_t>(base, tls_rva + 16))
            *reinterpret_cast<uint32_t *>(index_address) = 0;
    }
    teb::attach_thread(teb::DLL_PROCESS_ATTACH);

    // 4. Run it. A console program's entry point takes nothing and returns the exit code.
    using Entry = int(WINAPI *)();
    int code = reinterpret_cast<Entry>(base + entry_rva)();
    process::exit(code);
}
