// ws2_32.dll, Winsock: start-up, errors and the host name for now. Sockets themselves come once the
// kernel has networking.

#include <cstring>
#include <unistd.h>

#include "win.hpp"

namespace {
thread_local int last_error;
}

extern "C" {

// WSADATA: version 2.2.
WINAPI int ws_WSAStartup(WORD, uint8_t *data) {
    memset(data, 0, 408);
    WORD version = 0x0202;
    memcpy(data, &version, 2);
    memcpy(data + 2, &version, 2);
    return 0;
}
WINAPI int ws_WSACleanup() { return 0; }
WINAPI int ws_WSAGetLastError() { return last_error; }
WINAPI void ws_WSASetLastError(int e) { last_error = e; }
WINAPI int ws_gethostname(char *name, int size) {
    if (gethostname(name, static_cast<size_t>(size)) != 0) {
        last_error = 10014;  // WSAEFAULT
        return -1;
    }
    return 0;
}

}  // extern "C"

#define EXPORT(name) {#name, reinterpret_cast<void *>(&ws_##name)}

namespace {
const Export exports[] = {
    EXPORT(WSACleanup), EXPORT(WSAGetLastError), EXPORT(WSASetLastError), EXPORT(WSAStartup), EXPORT(gethostname),
};
}  // namespace

const BuiltinDll ws2_32_dll{"ws2_32.dll", exports, sizeof exports / sizeof exports[0]};
