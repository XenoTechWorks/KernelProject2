/* A small Windows DLL (mathlib.dll): exported functions, an exported variable, a DllMain, and its
 * own imports from the C runtime. Used by dlltest.exe. */
#include <stdio.h>
#include <windows.h>

__declspec(dllexport) int mathlib_loads;
static int attached;

BOOL WINAPI DllMain(HINSTANCE instance, DWORD reason, LPVOID reserved) {
    (void)instance;
    (void)reserved;
    if (reason == DLL_PROCESS_ATTACH) {
        attached = 1;
        mathlib_loads++;
    }
    return TRUE;
}

__declspec(dllexport) int add(int a, int b) { return a + b; }

__declspec(dllexport) int was_attached(void) { return attached; }

__declspec(dllexport) const char *describe(int n) {
    static char buf[64];
    snprintf(buf, sizeof buf, "mathlib says %d", n);
    return buf;
}
