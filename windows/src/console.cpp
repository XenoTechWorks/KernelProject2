// kernel32.dll, the console: modes (line input, echo, Ctrl+C handling), the screen size, text colors,
// the cursor, titles, and keyboard input. The console is the Linux terminal underneath: modes map to
// termios, and colors and cursor moves become the escape sequences terminals understand.

#include <cstring>
#include <string>
#include <sys/ioctl.h>
#include <termios.h>
#include <unistd.h>

#include "handles.hpp"

namespace {
// Input modes.
constexpr DWORD ENABLE_PROCESSED_INPUT = 0x1;
constexpr DWORD ENABLE_LINE_INPUT = 0x2;
constexpr DWORD ENABLE_ECHO_INPUT = 0x4;
// Output modes.
constexpr DWORD ENABLE_PROCESSED_OUTPUT = 0x1;
constexpr DWORD ENABLE_VIRTUAL_TERMINAL_PROCESSING = 0x4;

WORD current_attributes = 0x07;  // light grey on black
std::string title = "KernelProject2";

bool is_terminal(HANDLE h) { return isatty(handles::fd_of(h)); }

void write_out(HANDLE h, const std::string &s) {
    int fd = handles::fd_of(h);
    (void)!write(fd < 0 ? 1 : fd, s.data(), s.size());
}

struct Coord {
    int16_t x, y;
};

void terminal_size(int16_t &cols, int16_t &rows) {
    winsize ws{};
    if (ioctl(1, TIOCGWINSZ, &ws) == 0 && ws.ws_col) {
        cols = static_cast<int16_t>(ws.ws_col);
        rows = static_cast<int16_t>(ws.ws_row);
    } else {
        cols = 80;
        rows = 25;
    }
}

// A Windows color (bits: 1 blue, 2 green, 4 red, 8 bright) as a terminal color number.
int ansi_color(int windows) {
    int c = ((windows & 4) ? 1 : 0) | ((windows & 2) ? 2 : 0) | ((windows & 1) ? 4 : 0);
    return c;
}
}  // namespace

extern "C" {

WINAPI BOOL k32_GetConsoleMode(HANDLE h, DWORD *mode) {
    int fd = handles::fd_of(h);
    termios t{};
    if (!isatty(fd) || tcgetattr(fd, &t) != 0) {
        set_last_error(ERROR_INVALID_HANDLE);
        return 0;
    }
    if (fd == 0) {
        *mode = (t.c_lflag & ICANON ? ENABLE_LINE_INPUT : 0) | (t.c_lflag & ECHO ? ENABLE_ECHO_INPUT : 0) |
                (t.c_lflag & ISIG ? ENABLE_PROCESSED_INPUT : 0);
    } else {
        *mode = ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING;
    }
    return 1;
}

WINAPI BOOL k32_SetConsoleMode(HANDLE h, DWORD mode) {
    int fd = handles::fd_of(h);
    termios t{};
    if (!isatty(fd) || tcgetattr(fd, &t) != 0) {
        set_last_error(ERROR_INVALID_HANDLE);
        return 0;
    }
    if (fd != 0) return 1;  // output modes: escape sequences always work here
    auto flag = [&](bool on, tcflag_t bit) { t.c_lflag = on ? t.c_lflag | bit : t.c_lflag & ~bit; };
    flag(mode & ENABLE_LINE_INPUT, ICANON);
    flag(mode & ENABLE_ECHO_INPUT, ECHO);
    flag(mode & ENABLE_PROCESSED_INPUT, ISIG);
    t.c_cc[VMIN] = 1;
    t.c_cc[VTIME] = 0;
    tcsetattr(fd, TCSANOW, &t);
    return 1;
}

struct ScreenBufferInfo {
    Coord size, cursor;
    WORD attributes;
    int16_t left, top, right, bottom;
    Coord max_window;
};
static_assert(sizeof(ScreenBufferInfo) == 22);

WINAPI BOOL k32_GetConsoleScreenBufferInfo(HANDLE h, ScreenBufferInfo *info) {
    if (!is_terminal(h)) {
        set_last_error(ERROR_INVALID_HANDLE);
        return 0;
    }
    int16_t cols, rows;
    terminal_size(cols, rows);
    *info = ScreenBufferInfo{{cols, rows}, {0, 0}, current_attributes, 0, 0, static_cast<int16_t>(cols - 1),
                             static_cast<int16_t>(rows - 1), {cols, rows}};
    return 1;
}

WINAPI BOOL k32_SetConsoleTextAttribute(HANDLE h, WORD attributes) {
    current_attributes = attributes;
    int fg = attributes & 0xF, bg = (attributes >> 4) & 0xF;
    std::string s = "\x1b[0;" + std::to_string((fg & 8 ? 90 : 30) + ansi_color(fg));
    if (bg) s += ";" + std::to_string((bg & 8 ? 100 : 40) + ansi_color(bg));
    write_out(h, s + "m");
    return 1;
}

WINAPI BOOL k32_SetConsoleCursorPosition(HANDLE h, Coord at) {
    write_out(h, "\x1b[" + std::to_string(at.y + 1) + ";" + std::to_string(at.x + 1) + "H");
    return 1;
}

// Fills `count` cells from `at` with a character; used to clear the screen.
WINAPI BOOL k32_FillConsoleOutputCharacterA(HANDLE h, char c, DWORD count, Coord at, DWORD *written) {
    int16_t cols, rows;
    terminal_size(cols, rows);
    if (at.x == 0 && at.y == 0 && count >= static_cast<DWORD>(cols) * rows && c == ' ') {
        write_out(h, "\x1b[2J");
    } else {
        k32_SetConsoleCursorPosition(h, at);
        write_out(h, std::string(count, c));
    }
    if (written) *written = count;
    return 1;
}
WINAPI BOOL k32_FillConsoleOutputAttribute(HANDLE, WORD, DWORD count, Coord, DWORD *written) {
    if (written) *written = count;
    return 1;
}

WINAPI BOOL k32_SetConsoleScreenBufferSize(HANDLE, Coord) { return 1; }
WINAPI HANDLE k32_CreateConsoleScreenBuffer(DWORD, DWORD, void *, DWORD, void *) { return handles::std_handle(1); }
WINAPI BOOL k32_SetConsoleActiveScreenBuffer(HANDLE) { return 1; }
WINAPI DWORD k32_GetConsoleTitleA(LPSTR buf, DWORD size) {
    if (!size) return 0;
    size_t n = title.size() < size ? title.size() : size - 1;
    memcpy(buf, title.data(), n);
    buf[n] = 0;
    return static_cast<DWORD>(n);
}
WINAPI BOOL k32_SetConsoleTitleA(LPCSTR t) {
    title = t;
    return 1;
}
WINAPI HANDLE k32_GetConsoleWindow() { return nullptr; }  // there's no window to give
WINAPI UINT k32_GetConsoleCP() { return 65001; }          // UTF-8
WINAPI UINT k32_GetConsoleOutputCP() { return 65001; }
WINAPI BOOL k32_SetConsoleCP(UINT) { return 1; }
WINAPI BOOL k32_SetConsoleOutputCP(UINT) { return 1; }
WINAPI BOOL k32_SetConsoleCtrlHandler(void *, BOOL) { return 1; }
WINAPI BOOL k32_GenerateConsoleCtrlEvent(DWORD, DWORD) { return 1; }

// Keyboard input as console input records (KEY_EVENT), one per byte typed.
struct InputRecord {
    WORD event_type;
    WORD pad;
    BOOL key_down;
    WORD repeat_count;
    WORD virtual_key;
    WORD virtual_scan;
    char16_t character;
    DWORD control_keys;
};
static_assert(sizeof(InputRecord) == 20);

WINAPI BOOL k32_GetNumberOfConsoleInputEvents(HANDLE h, DWORD *count) {
    int available = 0;
    ioctl(handles::fd_of(h), FIONREAD, &available);
    *count = static_cast<DWORD>(available);
    return 1;
}

static BOOL read_input(HANDLE h, InputRecord *records, DWORD length, DWORD *read_count, bool peek) {
    int fd = handles::fd_of(h);
    if (peek) {
        int available = 0;
        ioctl(fd, FIONREAD, &available);
        if (available == 0) {
            *read_count = 0;
            return 1;
        }
    }
    unsigned char c;
    if (length == 0 || read(fd, &c, 1) != 1) {
        *read_count = 0;
        return length == 0;
    }
    WORD vk = c == '\r' || c == '\n' ? 0x0D : c == 0x7F || c == 8 ? 0x08 : c == 0x1B ? 0x1B : static_cast<WORD>(toupper(c));
    records[0] = InputRecord{1, 0, 1, 1, vk, 0, static_cast<char16_t>(c == '\n' ? '\r' : c), 0};
    *read_count = 1;
    return 1;
}
WINAPI BOOL k32_ReadConsoleInputA(HANDLE h, InputRecord *r, DWORD n, DWORD *count) { return read_input(h, r, n, count, false); }
WINAPI BOOL k32_ReadConsoleInputW(HANDLE h, InputRecord *r, DWORD n, DWORD *count) { return read_input(h, r, n, count, false); }
WINAPI BOOL k32_PeekConsoleInputW(HANDLE h, InputRecord *r, DWORD n, DWORD *count) { return read_input(h, r, n, count, true); }
WINAPI BOOL k32_ReadConsoleA(HANDLE h, void *buf, DWORD n, DWORD *count, void *) {
    ssize_t r = read(handles::fd_of(h), buf, n);
    *count = r < 0 ? 0 : static_cast<DWORD>(r);
    return r >= 0;
}

}  // extern "C"

#define EXPORT(name) {#name, reinterpret_cast<void *>(&k32_##name)}

namespace {
const Export exports[] = {
    EXPORT(CreateConsoleScreenBuffer), EXPORT(FillConsoleOutputAttribute), EXPORT(FillConsoleOutputCharacterA),
    EXPORT(GenerateConsoleCtrlEvent), EXPORT(GetConsoleCP), EXPORT(GetConsoleMode), EXPORT(GetConsoleOutputCP),
    EXPORT(GetConsoleScreenBufferInfo), EXPORT(GetConsoleTitleA), EXPORT(GetConsoleWindow),
    EXPORT(GetNumberOfConsoleInputEvents), EXPORT(PeekConsoleInputW), EXPORT(ReadConsoleA), EXPORT(ReadConsoleInputA),
    EXPORT(ReadConsoleInputW), EXPORT(SetConsoleActiveScreenBuffer), EXPORT(SetConsoleCP), EXPORT(SetConsoleCtrlHandler),
    EXPORT(SetConsoleCursorPosition), EXPORT(SetConsoleMode), EXPORT(SetConsoleOutputCP), EXPORT(SetConsoleScreenBufferSize),
    EXPORT(SetConsoleTextAttribute), EXPORT(SetConsoleTitleA),
};
}  // namespace

const BuiltinDll kernel32_console_dll{"kernel32.dll", exports, sizeof exports / sizeof exports[0]};
