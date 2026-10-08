// msvcrt.dll: the Microsoft C runtime (stdio, memory, strings, program start-up and exit).
//
// printf and friends get their extra arguments in Microsoft's va_list layout, which glibc can't
// read, so formatting is done here (format()). Everything else maps closely onto the C library
// underneath.

#include <cctype>
#include <cerrno>
#include <clocale>
#include <cmath>
#include <csignal>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <ctime>
#include <fcntl.h>
#include <string>
#include <unistd.h>
#include <vector>

#include "win.hpp"

using ms_va_list = __builtin_ms_va_list;

namespace {

// Microsoft's FILE (struct _iobuf). Programs only hold pointers to it; `file` is the descriptor.
struct WinFile {
    char *ptr;
    int cnt;
    char *base;
    int flag;
    int file;
    int charbuf;
    int bufsiz;
    char *tmpfname;
};
static_assert(sizeof(WinFile) == 48);

WinFile iob[3] = {{nullptr, 0, nullptr, 0, 0, 0, 0, nullptr}, {nullptr, 0, nullptr, 0, 1, 0, 0, nullptr}, {nullptr, 0, nullptr, 0, 2, 0, 0, nullptr}};

int fd_of(WinFile *f) { return f ? f->file : -1; }

bool write_all(int fd, const char *data, size_t len) {
    while (len) {
        ssize_t n = write(fd, data, len);
        if (n < 0) {
            if (errno == EINTR) continue;
            return false;
        }
        data += n;
        len -= n;
    }
    return true;
}

// ── printf formatting over Microsoft's va_list ─────────────────────────────────────────────────

void pad(std::string &out, size_t width, size_t len, char c) {
    for (size_t i = len; i < width; i++) out += c;
}

std::string utf16_to_utf8(const char16_t *s, long max) {
    std::string out;
    for (long i = 0; s[i] && (max < 0 || i < max); i++) {
        uint32_t c = s[i];
        if (c >= 0xD800 && c < 0xDC00 && s[i + 1]) c = 0x10000 + ((c - 0xD800) << 10) + (s[++i] - 0xDC00);
        if (c < 0x80) out += static_cast<char>(c);
        else if (c < 0x800) out += {static_cast<char>(0xC0 | c >> 6), static_cast<char>(0x80 | (c & 0x3F))};
        else if (c < 0x10000) out += {static_cast<char>(0xE0 | c >> 12), static_cast<char>(0x80 | (c >> 6 & 0x3F)), static_cast<char>(0x80 | (c & 0x3F))};
        else out += {static_cast<char>(0xF0 | c >> 18), static_cast<char>(0x80 | (c >> 12 & 0x3F)), static_cast<char>(0x80 | (c >> 6 & 0x3F)), static_cast<char>(0x80 | (c & 0x3F))};
    }
    return out;
}

std::string format(const char *fmt, ms_va_list ap) {
    std::string out;
    for (const char *p = fmt; *p; p++) {
        if (*p != '%') {
            out += *p;
            continue;
        }
        if (*++p == '%') {
            out += '%';
            continue;
        }
        bool left = false, plus = false, space = false, alt = false, zero = false;
        for (;; p++) {
            if (*p == '-') left = true;
            else if (*p == '+') plus = true;
            else if (*p == ' ') space = true;
            else if (*p == '#') alt = true;
            else if (*p == '0') zero = true;
            else break;
        }
        long width = 0, precision = -1;
        if (*p == '*') {
            width = __builtin_va_arg(ap, int);
            if (width < 0) left = true, width = -width;
            p++;
        } else
            while (isdigit(*p)) width = width * 10 + (*p++ - '0');
        if (*p == '.') {
            p++;
            precision = 0;
            if (*p == '*') {
                precision = __builtin_va_arg(ap, int);
                p++;
            } else
                while (isdigit(*p)) precision = precision * 10 + (*p++ - '0');
        }
        // Length: h hh l ll L z j t, and Microsoft's I, I32, I64.
        int size = 4;  // bytes of the integer argument
        bool wide = false;
        if (*p == 'h') {
            p++;
            if (*p == 'h') p++;
        } else if (*p == 'l') {
            p++;
            wide = true;  // %ls, %lc; long is 4 bytes on Windows
            if (*p == 'l') size = 8, p++;
        } else if (*p == 'L' || *p == 'z' || *p == 'j' || *p == 't') {
            size = 8;
            p++;
        } else if (*p == 'I') {
            p++;
            if (p[0] == '6' && p[1] == '4') size = 8, p += 2;
            else if (p[0] == '3' && p[1] == '2') size = 4, p += 2;
            else size = 8;
        } else if (*p == 'w') {
            wide = true;
            p++;
        }
        char conv = *p;
        if (!conv) break;

        std::string body;
        std::string prefix;
        switch (conv) {
            case 'd':
            case 'i': {
                int64_t v = size == 8 ? __builtin_va_arg(ap, int64_t) : __builtin_va_arg(ap, int);
                uint64_t u = v < 0 ? -static_cast<uint64_t>(v) : v;
                body = std::to_string(u);
                prefix = v < 0 ? "-" : plus ? "+" : space ? " " : "";
                break;
            }
            case 'u':
            case 'x':
            case 'X':
            case 'o': {
                uint64_t v = size == 8 ? __builtin_va_arg(ap, uint64_t) : __builtin_va_arg(ap, unsigned);
                int radix = conv == 'u' ? 10 : conv == 'o' ? 8 : 16;
                const char *digits = conv == 'X' ? "0123456789ABCDEF" : "0123456789abcdef";
                do {
                    body.insert(body.begin(), digits[v % radix]);
                    v /= radix;
                } while (v);
                if (alt && radix == 16 && body != "0") prefix = conv == 'X' ? "0X" : "0x";
                if (alt && radix == 8) prefix = "0";
                break;
            }
            case 'p': {
                uint64_t v = reinterpret_cast<uint64_t>(__builtin_va_arg(ap, void *));
                char buf[17];
                snprintf(buf, sizeof buf, "%016llX", static_cast<unsigned long long>(v));
                body = buf;
                break;
            }
            case 'c':
            case 'C':
                body = static_cast<char>(__builtin_va_arg(ap, int));
                break;
            case 's':
            case 'S': {
                void *arg = __builtin_va_arg(ap, void *);
                if (!arg) body = "(null)";
                else if (wide || conv == 'S') body = utf16_to_utf8(static_cast<char16_t *>(arg), precision);
                else body = precision >= 0 ? std::string(static_cast<char *>(arg), strnlen(static_cast<char *>(arg), precision)) : static_cast<char *>(arg);
                precision = -1;
                break;
            }
            case 'f':
            case 'F':
            case 'e':
            case 'E':
            case 'g':
            case 'G':
            case 'a':
            case 'A': {
                double v = __builtin_va_arg(ap, double);
                // glibc formats the number; only the argument passing was Windows-specific.
                char spec[16] = "%";
                size_t n = 1;
                if (alt) spec[n++] = '#';
                spec[n++] = '.';
                spec[n++] = '*';
                spec[n++] = conv;
                spec[n] = 0;
                char buf[512];
                snprintf(buf, sizeof buf, spec, precision < 0 ? 6 : static_cast<int>(precision), std::fabs(v));
                body = buf;
                prefix = std::signbit(v) ? "-" : plus ? "+" : space ? " " : "";
                precision = -1;
                break;
            }
            case 'n':
                *static_cast<int *>(__builtin_va_arg(ap, void *)) = static_cast<int>(out.size());
                continue;
            default:
                out += '%';
                out += conv;
                continue;
        }
        if (precision >= 0 && strchr("diuxXo", conv)) {
            if (precision == 0 && body == "0") body.clear();
            while (body.size() < static_cast<size_t>(precision)) body.insert(body.begin(), '0');
            zero = false;
        }
        size_t len = prefix.size() + body.size();
        if (left) {
            out += prefix + body;
            pad(out, width, len, ' ');
        } else if (zero) {
            out += prefix;
            pad(out, width, len, '0');
            out += body;
        } else {
            pad(out, width, len, ' ');
            out += prefix + body;
        }
    }
    return out;
}

// ── start-up and exit ──────────────────────────────────────────────────────────────────────────

using Callback = void(WINAPI *)();
std::vector<Callback> exit_handlers;
int commode, fmode;
char **initenv;
int win_errno;

}  // namespace

extern "C" {

WINAPI int crt_vfprintf(WinFile *f, const char *fmt, ms_va_list ap) {
    std::string s = format(fmt, ap);
    return write_all(fd_of(f), s.data(), s.size()) ? static_cast<int>(s.size()) : -1;
}
WINAPI int crt_fprintf(WinFile *f, const char *fmt, ...) {
    ms_va_list ap;
    __builtin_ms_va_start(ap, fmt);
    int n = crt_vfprintf(f, fmt, ap);
    __builtin_ms_va_end(ap);
    return n;
}
WINAPI int crt_vprintf(const char *fmt, ms_va_list ap) { return crt_vfprintf(&iob[1], fmt, ap); }
WINAPI int crt_printf(const char *fmt, ...) {
    ms_va_list ap;
    __builtin_ms_va_start(ap, fmt);
    int n = crt_vfprintf(&iob[1], fmt, ap);
    __builtin_ms_va_end(ap);
    return n;
}
WINAPI int crt_vsnprintf(char *buf, size_t size, const char *fmt, ms_va_list ap) {
    std::string s = format(fmt, ap);
    if (size) {
        size_t n = s.size() < size ? s.size() : size - 1;
        memcpy(buf, s.data(), n);
        buf[n] = 0;
    }
    // msvcrt's _vsnprintf returns -1 when the text didn't fit.
    return s.size() < size ? static_cast<int>(s.size()) : -1;
}
WINAPI int crt_snprintf(char *buf, size_t size, const char *fmt, ...) {
    ms_va_list ap;
    __builtin_ms_va_start(ap, fmt);
    int n = crt_vsnprintf(buf, size, fmt, ap);
    __builtin_ms_va_end(ap);
    return n;
}
WINAPI int crt_vsprintf(char *buf, const char *fmt, ms_va_list ap) {
    std::string s = format(fmt, ap);
    memcpy(buf, s.c_str(), s.size() + 1);
    return static_cast<int>(s.size());
}
WINAPI int crt_sprintf(char *buf, const char *fmt, ...) {
    ms_va_list ap;
    __builtin_ms_va_start(ap, fmt);
    int n = crt_vsprintf(buf, fmt, ap);
    __builtin_ms_va_end(ap);
    return n;
}

WINAPI int crt_fputc(int c, WinFile *f) {
    char ch = static_cast<char>(c);
    return write_all(fd_of(f), &ch, 1) ? c & 0xFF : -1;
}
WINAPI int crt_putchar(int c) { return crt_fputc(c, &iob[1]); }
WINAPI int crt_fputs(const char *s, WinFile *f) { return write_all(fd_of(f), s, strlen(s)) ? 0 : -1; }
WINAPI int crt_puts(const char *s) {
    std::string line = std::string(s) + "\n";
    return write_all(1, line.data(), line.size()) ? 0 : -1;
}
WINAPI size_t crt_fwrite(const void *data, size_t size, size_t count, WinFile *f) {
    return write_all(fd_of(f), static_cast<const char *>(data), size * count) ? count : 0;
}
WINAPI size_t crt_fread(void *data, size_t size, size_t count, WinFile *f) {
    size_t want = size * count, got = 0;
    while (got < want) {
        ssize_t n = read(fd_of(f), static_cast<char *>(data) + got, want - got);
        if (n <= 0) break;
        got += n;
    }
    return size ? got / size : 0;
}
WINAPI int crt_fgetc(WinFile *f) {
    unsigned char c;
    return read(fd_of(f), &c, 1) == 1 ? c : -1;
}
WINAPI int crt_getchar() { return crt_fgetc(&iob[0]); }
WINAPI char *crt_fgets(char *buf, int size, WinFile *f) {
    int n = 0;
    while (n < size - 1) {
        int c = crt_fgetc(f);
        if (c < 0) break;
        buf[n++] = static_cast<char>(c);
        if (c == '\n') break;
    }
    if (n == 0) return nullptr;
    buf[n] = 0;
    return buf;
}
WINAPI int crt_fflush(WinFile *) { return 0; }
WINAPI WinFile *crt___iob_func() { return iob; }

WINAPI WinFile *crt_fopen(const char *path, const char *mode) {
    int flags = mode[0] == 'r' ? O_RDONLY : mode[0] == 'w' ? O_WRONLY | O_CREAT | O_TRUNC : O_WRONLY | O_CREAT | O_APPEND;
    if (strchr(mode, '+')) flags = (flags & ~(O_RDONLY | O_WRONLY)) | O_RDWR;
    int fd = open(path, flags, 0644);
    if (fd < 0) {
        win_errno = errno;
        return nullptr;
    }
    return new WinFile{nullptr, 0, nullptr, 0, fd, 0, 0, nullptr};
}
WINAPI int crt_fclose(WinFile *f) {
    int r = close(fd_of(f));
    if (f < iob || f > iob + 2) delete f;
    return r;
}

WINAPI void *crt_malloc(size_t n) { return malloc(n); }
WINAPI void *crt_calloc(size_t n, size_t size) { return calloc(n, size); }
WINAPI void *crt_realloc(void *p, size_t n) { return realloc(p, n); }
WINAPI void crt_free(void *p) { free(p); }

WINAPI void *crt_memcpy(void *d, const void *s, size_t n) { return memcpy(d, s, n); }
WINAPI void *crt_memmove(void *d, const void *s, size_t n) { return memmove(d, s, n); }
WINAPI void *crt_memset(void *d, int c, size_t n) { return memset(d, c, n); }
WINAPI int crt_memcmp(const void *a, const void *b, size_t n) { return memcmp(a, b, n); }
WINAPI size_t crt_strlen(const char *s) { return strlen(s); }
WINAPI int crt_strcmp(const char *a, const char *b) { return strcmp(a, b); }
WINAPI int crt_strncmp(const char *a, const char *b, size_t n) { return strncmp(a, b, n); }
WINAPI char *crt_strcpy(char *d, const char *s) { return strcpy(d, s); }
WINAPI char *crt_strncpy(char *d, const char *s, size_t n) { return strncpy(d, s, n); }
WINAPI char *crt_strcat(char *d, const char *s) { return strcat(d, s); }
WINAPI char *crt_strchr(const char *s, int c) { return const_cast<char *>(strchr(s, c)); }
WINAPI char *crt_strrchr(const char *s, int c) { return const_cast<char *>(strrchr(s, c)); }
WINAPI char *crt_strstr(const char *s, const char *t) { return const_cast<char *>(strstr(s, t)); }
WINAPI char *crt_strdup(const char *s) { return strdup(s); }
WINAPI char *crt_strerror(int e) { return strerror(e); }
WINAPI size_t crt_wcslen(const char16_t *s) {
    size_t n = 0;
    while (s[n]) n++;
    return n;
}
WINAPI int crt_atoi(const char *s) { return atoi(s); }
WINAPI long crt_strtol(const char *s, char **end, int base) { return static_cast<long>(static_cast<int32_t>(strtol(s, end, base))); }
WINAPI int crt_toupper(int c) { return toupper(c); }
WINAPI int crt_tolower(int c) { return tolower(c); }
WINAPI int crt_isspace(int c) { return isspace(c); }
WINAPI int crt_isdigit(int c) { return isdigit(c); }
WINAPI int crt_isalpha(int c) { return isalpha(c); }
WINAPI char *crt_getenv(const char *name) { return getenv(name); }
WINAPI int crt_rand() { return rand() & 0x7FFF; }
WINAPI void crt_srand(unsigned seed) { srand(seed); }
WINAPI int64_t crt_time64(int64_t *out) {
    int64_t t = time(nullptr);
    if (out) *out = t;
    return t;
}

WINAPI int *crt__errno() { return &win_errno; }
WINAPI int crt____lc_codepage_func() { return 0; }
WINAPI int crt____mb_cur_max_func() { return 1; }
WINAPI lconv *crt_localeconv() { return localeconv(); }
WINAPI void *crt_signal(int, void *) { return nullptr; }

WINAPI int crt___getmainargs(int *argc, char ***argv, char ***envp, int, void *) {
    *argc = process::argc;
    *argv = process::argv;
    *envp = process::envp;
    initenv = process::envp;
    return 0;
}
WINAPI void crt___set_app_type(int) {}
WINAPI void crt___setusermatherr(void *) {}
WINAPI void crt__lock(int) {}
WINAPI void crt__unlock(int) {}

// Runs the C/C++ initializer functions the compiler collected between two addresses.
WINAPI void crt__initterm(Callback *begin, Callback *end) {
    for (Callback *f = begin; f < end; f++)
        if (*f) (*f)();
}
WINAPI Callback crt__onexit(Callback f) {
    exit_handlers.push_back(f);
    return f;
}
WINAPI int crt_atexit(Callback f) {
    exit_handlers.push_back(f);
    return 0;
}
WINAPI void crt__cexit() {
    while (!exit_handlers.empty()) {
        Callback f = exit_handlers.back();
        exit_handlers.pop_back();
        f();
    }
}
WINAPI void crt_exit(int code) {
    crt__cexit();
    process::exit(code);
}
WINAPI void crt__exit(int code) { process::exit(code); }
WINAPI void crt_abort() {
    write_all(2, "This application has requested the Runtime to terminate it in an unusual way.\n", 79);
    process::exit(3);
}
WINAPI void crt__amsg_exit(int code) {
    std::string s = "runtime error R60" + std::to_string(code) + "\n";
    write_all(2, s.data(), s.size());
    process::exit(255);
}
// Structured exception handling: nothing to unwind yet, so keep searching (ExceptionContinueSearch).
WINAPI int crt___C_specific_handler(void *, void *, void *, void *) { return 1; }

}  // extern "C"

#define EXPORT(name) {#name, reinterpret_cast<void *>(&crt_##name)}
#define DATA(name, variable) {#name, reinterpret_cast<void *>(&variable)}

namespace {
const Export exports[] = {
    EXPORT(__C_specific_handler), EXPORT(___lc_codepage_func), EXPORT(___mb_cur_max_func), EXPORT(__getmainargs),
    DATA(__initenv, initenv), EXPORT(__iob_func), EXPORT(__set_app_type), EXPORT(__setusermatherr),
    EXPORT(_amsg_exit), EXPORT(_cexit), DATA(_commode, commode), EXPORT(_errno), EXPORT(_exit),
    DATA(_fmode, fmode), EXPORT(_initterm), EXPORT(_lock), EXPORT(_onexit), EXPORT(_unlock), EXPORT(abort),
    EXPORT(atexit), EXPORT(atoi), EXPORT(calloc), EXPORT(exit), EXPORT(fclose), EXPORT(fflush), EXPORT(fgetc),
    EXPORT(fgets), EXPORT(fopen), EXPORT(fprintf), EXPORT(fputc), EXPORT(fputs), EXPORT(fread), EXPORT(free),
    EXPORT(fwrite), EXPORT(getchar), EXPORT(getenv), EXPORT(isalpha), EXPORT(isdigit), EXPORT(isspace),
    EXPORT(localeconv), EXPORT(malloc), EXPORT(memcmp), EXPORT(memcpy), EXPORT(memmove), EXPORT(memset),
    EXPORT(printf), EXPORT(putchar), EXPORT(puts), EXPORT(rand), EXPORT(realloc), EXPORT(signal),
    EXPORT(snprintf), EXPORT(sprintf), EXPORT(srand), EXPORT(strcat), EXPORT(strchr), EXPORT(strcmp),
    EXPORT(strcpy), EXPORT(strdup), EXPORT(strerror), EXPORT(strlen), EXPORT(strncmp), EXPORT(strncpy),
    EXPORT(strrchr), EXPORT(strstr), EXPORT(strtol), EXPORT(tolower), EXPORT(toupper), EXPORT(vfprintf),
    EXPORT(vprintf), EXPORT(vsnprintf), EXPORT(vsprintf), EXPORT(wcslen),
    {"_snprintf", reinterpret_cast<void *>(&crt_snprintf)}, {"_vsnprintf", reinterpret_cast<void *>(&crt_vsnprintf)},
    {"_time64", reinterpret_cast<void *>(&crt_time64)}, {"time", reinterpret_cast<void *>(&crt_time64)},
    {"_strdup", reinterpret_cast<void *>(&crt_strdup)},
};
}  // namespace

const BuiltinDll msvcrt_dll{"msvcrt.dll", exports, sizeof exports / sizeof exports[0]};
