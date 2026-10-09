// advapi32.dll: security and cryptography basics. There's one user, root, and it's an
// administrator; random numbers come from the kernel's getrandom.

#include <cstring>
#include <sys/random.h>

#include "win.hpp"

namespace {
const uintptr_t TOKEN = 0x7000;   // the one access token
const uintptr_t CRYPTO = 0x7004;  // the one crypto provider

bool fill_random(void *buf, size_t len) {
    auto *p = static_cast<uint8_t *>(buf);
    while (len) {
        ssize_t n = getrandom(p, len, 0);
        if (n <= 0) return false;
        p += n;
        len -= n;
    }
    return true;
}
}  // namespace

extern "C" {

WINAPI BOOL adv_CryptAcquireContextA(uintptr_t *provider, LPCSTR, LPCSTR, DWORD, DWORD) {
    *provider = CRYPTO;
    return 1;
}
WINAPI BOOL adv_CryptAcquireContextW(uintptr_t *provider, LPCWSTR, LPCWSTR, DWORD, DWORD) {
    *provider = CRYPTO;
    return 1;
}
WINAPI BOOL adv_CryptGenRandom(uintptr_t, DWORD len, BYTE *buf) { return fill_random(buf, len); }
WINAPI BOOL adv_CryptReleaseContext(uintptr_t, DWORD) { return 1; }
// RtlGenRandom, exported under this name.
WINAPI BOOL adv_SystemFunction036(void *buf, ULONG len) { return fill_random(buf, len); }

WINAPI BOOL adv_GetUserNameA(LPSTR buf, DWORD *size) {
    if (*size < 5) {
        *size = 5;
        set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    }
    memcpy(buf, "root", 5);
    *size = 5;
    return 1;
}
WINAPI BOOL adv_GetUserNameW(LPWSTR buf, DWORD *size) {
    if (*size < 5) {
        *size = 5;
        set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    }
    const char16_t name[] = u"root";
    memcpy(buf, name, sizeof name);
    *size = 5;
    return 1;
}

WINAPI BOOL adv_OpenProcessToken(HANDLE, DWORD, HANDLE *token) {
    *token = reinterpret_cast<HANDLE>(TOKEN);
    return 1;
}
WINAPI BOOL adv_OpenThreadToken(HANDLE, DWORD, BOOL, HANDLE *token) {
    *token = reinterpret_cast<HANDLE>(TOKEN);
    return 1;
}
// Token details beyond membership aren't modelled yet: callers fall back to their defaults.
WINAPI BOOL adv_GetTokenInformation(HANDLE, int, void *, DWORD, DWORD *needed) {
    if (needed) *needed = 0;
    set_last_error(ERROR_INVALID_PARAMETER);
    return 0;
}
WINAPI BOOL adv_CheckTokenMembership(HANDLE, void *, BOOL *is_member) {
    *is_member = 1;  // root is in every group
    return 1;
}
WINAPI BOOL adv_EqualSid(void *, void *) { return 0; }
WINAPI BOOL adv_DuplicateToken(HANDLE, int, HANDLE *copy) {
    *copy = reinterpret_cast<HANDLE>(TOKEN);
    return 1;
}
WINAPI BOOL adv_SetTokenInformation(HANDLE, int, void *, DWORD) { return 1; }
WINAPI BOOL adv_AccessCheck(void *, HANDLE, DWORD, void *, void *, DWORD *, DWORD *granted, BOOL *status) {
    if (granted) *granted = 0xFFFFFFFF;
    if (status) *status = 1;
    return 1;
}
WINAPI DWORD adv_GetSecurityInfo(HANDLE, int, DWORD, void **owner, void **group, void **dacl, void **sacl, void **descriptor) {
    for (void **p : {owner, group, dacl, sacl, descriptor})
        if (p) *p = nullptr;
    return 0;
}

}  // extern "C"

#define EXPORT(name) {#name, reinterpret_cast<void *>(&adv_##name)}

namespace {
const Export exports[] = {
    EXPORT(AccessCheck), EXPORT(CheckTokenMembership), EXPORT(CryptAcquireContextA), EXPORT(CryptAcquireContextW),
    EXPORT(CryptGenRandom), EXPORT(CryptReleaseContext), EXPORT(DuplicateToken), EXPORT(EqualSid),
    EXPORT(GetSecurityInfo), EXPORT(GetTokenInformation), EXPORT(GetUserNameA), EXPORT(GetUserNameW),
    EXPORT(OpenProcessToken), EXPORT(OpenThreadToken), EXPORT(SetTokenInformation), EXPORT(SystemFunction036),
};
}  // namespace

const BuiltinDll advapi32_dll{"advapi32.dll", exports, sizeof exports / sizeof exports[0]};
