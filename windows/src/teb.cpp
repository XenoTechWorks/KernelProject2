// The Thread and Process Environment Blocks Windows programs expect to find through gs.

#include <cstdlib>
#include <cstring>
#include <sys/syscall.h>
#include <unistd.h>

#include "win.hpp"

namespace {
constexpr long ARCH_SET_GS = 0x1001;
uint8_t *the_teb;
uint8_t *the_peb;
}  // namespace

void teb::init(void *image_base, void **tls_array) {
    the_teb = static_cast<uint8_t *>(calloc(1, SIZE));
    the_peb = static_cast<uint8_t *>(calloc(1, 0x1000));
    auto put = [](uint8_t *block, size_t at, uint64_t value) { memcpy(block + at, &value, 8); };

    // The stack: from here down to the 8 MiB limit (what the C runtime's stack checks look at).
    uint64_t here = reinterpret_cast<uint64_t>(__builtin_frame_address(0));
    put(the_teb, STACK_BASE, (here + 0xFFFF) & ~0xFFFFull);
    put(the_teb, STACK_LIMIT, here - (8 << 20));
    put(the_teb, EXCEPTION_LIST, ~0ull);
    put(the_teb, SELF, reinterpret_cast<uint64_t>(the_teb));
    put(the_teb, PROCESS_ID, getpid());
    put(the_teb, THREAD_ID, static_cast<uint64_t>(syscall(SYS_gettid)));
    put(the_teb, TLS_POINTER, reinterpret_cast<uint64_t>(tls_array));
    put(the_teb, PEB, reinterpret_cast<uint64_t>(the_peb));
    put(the_peb, 0x10, reinterpret_cast<uint64_t>(image_base));  // ImageBaseAddress

    syscall(SYS_arch_prctl, ARCH_SET_GS, the_teb);
}

uint8_t *teb::current() { return the_teb; }
