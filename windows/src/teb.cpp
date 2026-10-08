// The Thread and Process Environment Blocks Windows programs find through gs, and per-thread set-up
// (thread-local storage and TLS callbacks) for the main thread and every CreateThread thread.

#include <cstdlib>
#include <cstring>
#include <sys/syscall.h>
#include <unistd.h>

#include "win.hpp"

namespace process {
std::vector<TlsTemplate> tls;
}

namespace {
constexpr long ARCH_SET_GS = 0x1001;
thread_local uint8_t *this_teb;
uint8_t *the_peb;

void put(uint8_t *block, size_t at, uint64_t value) { memcpy(block + at, &value, 8); }

void *copy_template(const TlsTemplate &tls) {
    auto *block = static_cast<uint8_t *>(calloc(1, tls.end - tls.start + tls.zero_fill + 16));
    memcpy(block, reinterpret_cast<void *>(tls.start), tls.end - tls.start);
    return block;
}
}  // namespace

void teb::attach_thread(DWORD reason) {
    if (!the_peb) {
        the_peb = static_cast<uint8_t *>(calloc(1, 0x1000));
        put(the_peb, 0x10, reinterpret_cast<uint64_t>(process::image_base));  // ImageBaseAddress
    }
    uint8_t *t = static_cast<uint8_t *>(calloc(1, SIZE));
    this_teb = t;

    // The stack: from here down to the 8 MiB limit (what the C runtime's stack checks look at).
    uint64_t here = reinterpret_cast<uint64_t>(__builtin_frame_address(0));
    put(t, STACK_BASE, (here + 0xFFFF) & ~0xFFFFull);
    put(t, STACK_LIMIT, here - (8 << 20));
    put(t, EXCEPTION_LIST, ~0ull);
    put(t, SELF, reinterpret_cast<uint64_t>(t));
    put(t, PROCESS_ID, getpid());
    put(t, THREAD_ID, static_cast<uint64_t>(syscall(SYS_gettid)));
    put(t, PEB, reinterpret_cast<uint64_t>(the_peb));

    // This thread's copies of every image's thread-local variables, one slot each.
    auto **array = static_cast<void **>(calloc(64, sizeof(void *)));
    for (size_t i = 0; i < process::tls.size() && i < 64; i++) array[i] = copy_template(process::tls[i]);
    put(t, TLS_POINTER, reinterpret_cast<uint64_t>(array));
    syscall(SYS_arch_prctl, ARCH_SET_GS, t);

    using TlsCallback = void(WINAPI *)(void *, DWORD, void *);
    for (const TlsTemplate &tls : process::tls)
        for (auto *cb = reinterpret_cast<TlsCallback *>(tls.callbacks); cb && *cb; cb++) (*cb)(tls.image, reason, nullptr);
}

void teb::add_tls_slot(uint32_t index) {
    if (index >= 64 || !this_teb) return;
    void **array;
    memcpy(&array, this_teb + TLS_POINTER, 8);
    array[index] = copy_template(process::tls[index]);
}

uint8_t *teb::current() { return this_teb; }
