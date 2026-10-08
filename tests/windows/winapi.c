/* The Windows API directly (kernel32), built with mingw-w64. Prints one line per check and
 * "winapi: N/N passed" at the end. */
#include <stdio.h>
#include <string.h>
#include <windows.h>

static int passed, total;
static void check(const char *what, int ok) {
    total++;
    passed += ok;
    printf("  %s %s\n", ok ? "ok  " : "FAIL", what);
}

static __thread int per_thread = 41;

int main(void) {
    HANDLE out = GetStdHandle(STD_OUTPUT_HANDLE);
    const char text[] = "  (written with WriteFile)\n";
    DWORD written = 0;
    check("WriteFile to the console", WriteFile(out, text, sizeof text - 1, &written, NULL) && written == sizeof text - 1);

    check("GetCommandLineA names the program", strstr(GetCommandLineA(), "winapi") != NULL);
    check("GetCurrentProcessId", GetCurrentProcessId() != 0);

    char *mem = VirtualAlloc(NULL, 65536, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
    if (mem) memset(mem, 0x5A, 65536);
    check("VirtualAlloc gives usable memory", mem && mem[65535] == 0x5A);
    VirtualFree(mem, 0, MEM_RELEASE);

    DWORD slot = TlsAlloc();
    TlsSetValue(slot, (void *)0x1234);
    check("TlsAlloc / TlsSetValue / TlsGetValue", TlsGetValue(slot) == (void *)0x1234);

    per_thread++;
    check("__thread variables (the image's TLS block)", per_thread == 42);

    LARGE_INTEGER a, b, freq;
    QueryPerformanceFrequency(&freq);
    QueryPerformanceCounter(&a);
    Sleep(20);
    QueryPerformanceCounter(&b);
    double ms = (double)(b.QuadPart - a.QuadPart) * 1000.0 / (double)freq.QuadPart;
    check("Sleep and QueryPerformanceCounter", ms >= 15.0);
    printf("  slept %.1f ms\n", ms);

    wchar_t wide[32];
    char back[32];
    int n = MultiByteToWideChar(CP_UTF8, 0, "h\xc3\xa9llo", -1, wide, 32);
    WideCharToMultiByte(CP_UTF8, 0, wide, -1, back, sizeof back, NULL, NULL);
    check("UTF-8 to UTF-16 and back", n == 6 && wide[1] == 0xE9 && strcmp(back, "h\xc3\xa9llo") == 0);

    SetLastError(1234);
    check("SetLastError / GetLastError", GetLastError() == 1234);

    printf("winapi: %d/%d passed\n", passed, total);
    ExitProcess(passed == total ? 0 : 1);
}
