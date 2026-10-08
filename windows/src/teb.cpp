// The Thread and Process Environment Blocks Windows programs find through gs, and per-thread set-up
// (thread-local storage and TLS callbacks) for the main thread and every CreateThread thread.

#include <cstdlib>
#include <cstring>
#include <sys/syscall.h>
#include <unistd.h>

#include "win.hpp"

namespace process {
TlsTemplate tls;
}

namespace {
constexpr long ARCH_SET_GS = 0x1001;
thread_local uint8_t *this_teb;
uint8_t *the_peb;

void put(uint8_t *block, size_t at, uint64_t value) { memcpy(block + at, &value, 8); }
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

    // This thread's copy of the program's thread-local variables.
    const TlsTemplate &tls = process::tls;
    if (tls.present) {
        auto **array = static_cast<void **>(calloc(1, sizeof(void *)));
        auto *block = static_cast<uint8_t *>(calloc(1, tls.end - tls.start + tls.zero_fill + 16));
        memcpy(block, reinterpret_cast<void *>(tls.start), tls.end - tls.start);
        array[0] = block;
        put(t, TLS_POINTER, reinterpret_cast<uint64_t>(array));
    }
    syscall(SYS_arch_prctl, ARCH_SET_GS, t);

    if (tls.present) {
        using TlsCallback = void(WINAPI *)(void *, DWORD, void *);
        for (auto *cb = reinterpret_cast<TlsCallback *>(tls.callbacks); cb && *cb; cb++) (*cb)(process::image_base, reason, nullptr);
    }
}

uint8_t *teb::current() { return this_teb; }
