/* Using a DLL both ways Windows programs do: linked at build time (mathlib.dll is loaded before
 * main starts) and loaded at run time with LoadLibrary/GetProcAddress. Built with mingw-w64.
 * Prints one line per check and "dlls: N/N passed" at the end. */
#include <stdio.h>
#include <string.h>
#include <windows.h>

__declspec(dllimport) int add(int a, int b);
__declspec(dllimport) int was_attached(void);
__declspec(dllimport) int mathlib_loads;

static int passed, total;
static void check(const char *what, int ok) {
    total++;
    passed += ok;
    printf("  %s %s\n", ok ? "ok  " : "FAIL", what);
}

int main(void) {
    check("a linked DLL's function", add(2, 3) == 5);
    check("its DllMain ran before main", was_attached() == 1);
    check("its exported variable", mathlib_loads == 1);

    HMODULE lib = LoadLibraryA("mathlib.dll");
    check("LoadLibraryA returns the already-loaded DLL", lib != NULL && lib == GetModuleHandleA("mathlib.dll"));
    typedef const char *(*describe_fn)(int);
    describe_fn describe = (describe_fn)GetProcAddress(lib, "describe");
    check("GetProcAddress", describe && strcmp(describe(7), "mathlib says 7") == 0);
    check("GetProcAddress for a missing name returns NULL", GetProcAddress(lib, "nope") == NULL);
    check("DllMain didn't run twice", mathlib_loads == 1);

    HMODULE k32 = GetModuleHandleA("kernel32.dll");
    typedef DWORD (WINAPI *tick_fn)(void);
    tick_fn tick = (tick_fn)GetProcAddress(k32, "GetTickCount");
    check("GetProcAddress on a system DLL (kernel32)", tick && tick() > 0);

    check("LoadLibraryA of a missing DLL fails", LoadLibraryA("nosuch.dll") == NULL);
    check("FreeLibrary", FreeLibrary(lib));

    printf("dlls: %d/%d passed\n", passed, total);
    return passed == total ? 0 : 1;
}
