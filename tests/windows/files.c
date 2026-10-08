/* Files and directories through the Windows API, built with mingw-w64.
 * Prints one line per check and "files: N/N passed" at the end. */
#include <stdio.h>
#include <string.h>
#include <windows.h>

static int passed, total;
static void check(const char *what, int ok) {
    total++;
    passed += ok;
    printf("  %s %s\n", ok ? "ok  " : "FAIL", what);
}

int main(void) {
    char dir[MAX_PATH];
    GetTempPathA(sizeof dir, dir);
    strcat(dir, "wintest");
    RemoveDirectoryA(dir);
    check("CreateDirectoryA", CreateDirectoryA(dir, NULL));
    check("SetCurrentDirectoryA", SetCurrentDirectoryA(dir));
    char cwd[MAX_PATH];
    GetCurrentDirectoryA(sizeof cwd, cwd);
    check("GetCurrentDirectoryA", strstr(cwd, "wintest") != NULL);

    HANDLE f = CreateFileA("notes.txt", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
    check("CreateFileA (create)", f != INVALID_HANDLE_VALUE);
    const char text[] = "line one\r\nline two\r\n";
    DWORD n = 0;
    check("WriteFile", WriteFile(f, text, sizeof text - 1, &n, NULL) && n == sizeof text - 1);
    CloseHandle(f);

    f = CreateFileA("notes.txt", GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, 0, NULL);
    check("GetFileSize", GetFileSize(f, NULL) == sizeof text - 1);
    char buf[64] = {0};
    check("ReadFile", ReadFile(f, buf, sizeof buf, &n, NULL) && n == sizeof text - 1 && strcmp(buf, text) == 0);
    SetFilePointer(f, 10, NULL, FILE_BEGIN);
    memset(buf, 0, sizeof buf);
    ReadFile(f, buf, 8, &n, NULL);
    check("SetFilePointer then ReadFile", strcmp(buf, "line two") == 0);
    CloseHandle(f);

    check("OPEN_EXISTING on a missing file fails with ERROR_FILE_NOT_FOUND",
          CreateFileA("missing.txt", GENERIC_READ, 0, NULL, OPEN_EXISTING, 0, NULL) == INVALID_HANDLE_VALUE &&
              GetLastError() == ERROR_FILE_NOT_FOUND);

    DWORD attrs = GetFileAttributesA(dir);
    check("GetFileAttributesA sees a directory", attrs != INVALID_FILE_ATTRIBUTES && (attrs & FILE_ATTRIBUTE_DIRECTORY));

    check("MoveFileA", MoveFileA("notes.txt", "renamed.txt"));
    WIN32_FIND_DATAA found;
    HANDLE search = FindFirstFileA("*.txt", &found);
    int matches = 0, saw_renamed = 0;
    if (search != INVALID_HANDLE_VALUE) {
        do {
            matches++;
            saw_renamed |= strcmp(found.cFileName, "renamed.txt") == 0;
        } while (FindNextFileA(search, &found));
        FindClose(search);
    }
    check("FindFirstFileA / FindNextFileA", matches == 1 && saw_renamed);

    FILE *c = fopen("renamed.txt", "r");
    char line[32] = {0};
    if (c) {
        fgets(line, sizeof line, c);
        fclose(c);
    }
    check("the C runtime reads the same file (fopen/fgets)", strncmp(line, "line one", 8) == 0);

    check("DeleteFileA", DeleteFileA("renamed.txt"));
    SetCurrentDirectoryA("..");
    check("RemoveDirectoryA", RemoveDirectoryA(dir));

    char self[MAX_PATH];
    GetModuleFileNameA(NULL, self, sizeof self);
    check("GetModuleFileNameA", strstr(self, "files") != NULL);

    printf("files: %d/%d passed\n", passed, total);
    return passed == total ? 0 : 1;
}
