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
#include <sys/stat.h>
#include <unistd.h>
#include <vector>

#include "win.hpp"

using ms_va_list = __builtin_ms_va_list;

// seh.cpp
extern "C" WINAPI int crt___C_specific_handler(void *, void *, void *, void *);

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

// `wide_strings`: the wprintf family, where %s and %c are UTF-16 (and %hs, %hc, %S narrow).
std::string format(const char *fmt, ms_va_list ap, bool wide_strings = false) {
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
        bool wide = false, narrow = false;
        if (*p == 'h') {
            p++;
            narrow = true;
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
            case 'C': {
                int c = __builtin_va_arg(ap, int);
                bool w = (wide || (wide_strings && !narrow)) != (conv == 'C');
                if (w) {
                    char16_t text[2] = {static_cast<char16_t>(c), 0};
                    body = utf16_to_utf8(text, 1);
                } else {
                    body = static_cast<char>(c);
                }
                break;
            }
            case 's':
            case 'S': {
                void *arg = __builtin_va_arg(ap, void *);
                if (!arg) body = "(null)";
                else if ((wide || (wide_strings && !narrow)) != (conv == 'S')) body = utf16_to_utf8(static_cast<char16_t *>(arg), precision);
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

// ── more stdio, low-level I/O, strings and wide characters (what libstdc++ uses) ───────────────

namespace {
// One character pushed back with ungetc, per descriptor.
int pushed_back[64];
bool has_pushed_back[64];
}  // namespace

WINAPI int crt_putc(int c, WinFile *f) { return crt_fputc(c, f); }
WINAPI int crt_getc(WinFile *f) {
    int fd = fd_of(f);
    if (fd >= 0 && fd < 64 && has_pushed_back[fd]) {
        has_pushed_back[fd] = false;
        return pushed_back[fd];
    }
    return crt_fgetc(f);
}
WINAPI int crt_ungetc(int c, WinFile *f) {
    int fd = fd_of(f);
    if (c < 0 || fd < 0 || fd >= 64) return -1;
    pushed_back[fd] = c & 0xFF;
    has_pushed_back[fd] = true;
    return c & 0xFF;
}
WINAPI int crt_setvbuf(WinFile *, char *, int, size_t) { return 0; }  // output isn't buffered here
WINAPI char *crt_setlocale(int, const char *locale) {
    static char c_locale[] = "C";
    return !locale || !*locale || strcmp(locale, "C") == 0 ? c_locale : nullptr;
}
WINAPI int crt__fileno(WinFile *f) { return fd_of(f); }
WINAPI WinFile *crt__fdopen(int fd, const char *) { return new WinFile{nullptr, 0, nullptr, 0, fd, 0, 0, nullptr}; }
WINAPI int crt__write(int fd, const void *data, unsigned size) { return static_cast<int>(write(fd, data, size)); }
WINAPI int crt__read(int fd, void *data, unsigned size) { return static_cast<int>(read(fd, data, size)); }
WINAPI int64_t crt__lseeki64(int fd, int64_t offset, int whence) { return lseek(fd, offset, whence); }
WINAPI int64_t crt__filelengthi64(int fd) {
    struct stat st;
    return fstat(fd, &st) == 0 ? st.st_size : -1;
}
// Microsoft's struct _stat64.
struct WinStat64 {
    uint32_t dev;
    uint16_t ino, mode;
    int16_t nlink, uid, gid, pad0;
    uint32_t rdev, pad1;
    int64_t size, atime, mtime, ctime;
};
static_assert(sizeof(WinStat64) == 56);
WINAPI int crt__fstat64(int fd, WinStat64 *out) {
    struct stat st;
    if (fstat(fd, &st) != 0) {
        win_errno = errno;
        return -1;
    }
    uint16_t mode = S_ISDIR(st.st_mode) ? 0x4000 : S_ISCHR(st.st_mode) || S_ISFIFO(st.st_mode) ? 0x2000 : 0x8000;
    mode |= 0x100 | (st.st_mode & S_IWUSR ? 0x80 : 0) | (st.st_mode & S_IXUSR ? 0x40 : 0);
    *out = WinStat64{static_cast<uint32_t>(st.st_dev), static_cast<uint16_t>(st.st_ino), mode, static_cast<int16_t>(st.st_nlink),
                     0, 0, 0, static_cast<uint32_t>(st.st_rdev), 0, st.st_size, st.st_atime, st.st_mtime, st.st_ctime};
    return 0;
}
WINAPI int crt_fgetpos(WinFile *f, int64_t *pos) {
    *pos = lseek(fd_of(f), 0, SEEK_CUR);
    return *pos < 0 ? -1 : 0;
}
WINAPI int crt_fsetpos(WinFile *f, const int64_t *pos) { return lseek(fd_of(f), *pos, SEEK_SET) < 0 ? -1 : 0; }
WINAPI int crt_fseek(WinFile *f, long offset, int whence) { return lseek(fd_of(f), offset, whence) < 0 ? -1 : 0; }
WINAPI long crt_ftell(WinFile *f) { return static_cast<long>(lseek(fd_of(f), 0, SEEK_CUR)); }

WINAPI void *crt_memchr(const void *s, int c, size_t n) { return const_cast<void *>(memchr(s, c, n)); }
WINAPI unsigned long crt_strtoul(const char *s, char **end, int base) { return static_cast<uint32_t>(strtoul(s, end, base)); }
WINAPI int crt_strcoll(const char *a, const char *b) { return strcmp(a, b); }
WINAPI size_t crt_strxfrm(char *d, const char *s, size_t n) {
    size_t len = strlen(s);
    if (n) strncpy(d, s, n);
    return len;
}
WINAPI size_t crt_strftime(char *out, size_t size, const char *fmt, const struct tm *t) { return strftime(out, size, fmt, t); }

// Wide characters are UTF-16 code units (wchar_t is 2 bytes on Windows).
WINAPI uint16_t crt_towlower(uint16_t c) { return c < 0x80 ? static_cast<uint16_t>(tolower(c)) : c; }
WINAPI uint16_t crt_towupper(uint16_t c) { return c < 0x80 ? static_cast<uint16_t>(toupper(c)) : c; }
WINAPI int crt_iswctype(uint16_t c, uint16_t type) {
    if (c >= 0x80) return type & 0x0103;  // _UPPER|_LOWER|_ALPHA-ish: treat as a letter
    int r = 0;
    if (isupper(c)) r |= 0x1;
    if (islower(c)) r |= 0x2;
    if (isdigit(c)) r |= 0x4;
    if (isspace(c)) r |= 0x8;
    if (ispunct(c)) r |= 0x10;
    if (iscntrl(c)) r |= 0x20;
    if (c == ' ') r |= 0x40;
    if (isxdigit(c)) r |= 0x80;
    if (isalpha(c)) r |= 0x100;
    return r & type;
}
WINAPI uint16_t crt_getwc(WinFile *f) {
    int c = crt_getc(f);
    return c < 0 ? 0xFFFF : static_cast<uint16_t>(c);
}
WINAPI uint16_t crt_putwc(uint16_t c, WinFile *f) {
    char16_t text[2] = {c, 0};
    std::string s = utf16_to_utf8(text, 1);
    return write_all(fd_of(f), s.data(), s.size()) ? c : 0xFFFF;
}
WINAPI uint16_t crt_ungetwc(uint16_t c, WinFile *f) { return crt_ungetc(c, f) < 0 ? 0xFFFF : c; }
WINAPI int crt_wcscoll(const char16_t *a, const char16_t *b) {
    while (*a && *a == *b) a++, b++;
    return *a - *b;
}
WINAPI size_t crt_wcsxfrm(char16_t *d, const char16_t *s, size_t n) {
    size_t len = crt_wcslen(s);
    for (size_t i = 0; i < n && i <= len; i++) d[i] = s[i];
    return len;
}
WINAPI size_t crt_wcsftime(char16_t *out, size_t size, const char16_t *fmt, const struct tm *t) {
    char narrow[256];
    size_t n = strftime(narrow, sizeof narrow, utf16_to_utf8(fmt, -1).c_str(), t);
    if (n >= size) return 0;
    for (size_t i = 0; i <= n; i++) out[i] = static_cast<unsigned char>(narrow[i]);
    return n;
}
WINAPI WinFile *crt__wfopen(const char16_t *path, const char16_t *mode) {
    return crt_fopen(utf16_to_utf8(path, -1).c_str(), utf16_to_utf8(mode, -1).c_str());
}

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
    EXPORT(putc), EXPORT(getc), EXPORT(ungetc), EXPORT(setvbuf), EXPORT(setlocale), EXPORT(_fileno), EXPORT(_fdopen),
    EXPORT(_write), EXPORT(_read), EXPORT(_lseeki64), EXPORT(_filelengthi64), EXPORT(_fstat64), EXPORT(fgetpos),
    EXPORT(fsetpos), EXPORT(fseek), EXPORT(ftell), EXPORT(memchr), EXPORT(strtoul), EXPORT(strcoll), EXPORT(strxfrm),
    EXPORT(strftime), EXPORT(towlower), EXPORT(towupper), EXPORT(iswctype), EXPORT(getwc), EXPORT(putwc),
    EXPORT(ungetwc), EXPORT(wcscoll), EXPORT(wcsxfrm), EXPORT(wcsftime), EXPORT(_wfopen),
};
}  // namespace

const BuiltinDll msvcrt_dll{"msvcrt.dll", exports, sizeof exports / sizeof exports[0]};

// ── UCRT (ucrtbase.dll and its api-ms-win-crt-*.dll names) ─────────────────────────────────────
// What Visual Studio and newer mingw builds link against. The C functions are the same as above;
// what differs is start-up (argument and environment set-up through __p___argv and friends) and
// one stdio entry point per family (__stdio_common_vfprintf and others) taking options and a locale.

namespace {
std::vector<std::u16string> wide_args, wide_env;
std::vector<char16_t *> wide_argv, wide_envp;

std::u16string utf8_to_utf16(const char *s) {
    std::u16string out;
    while (*s) {
        uint32_t c = static_cast<uint8_t>(*s++);
        int extra = c >= 0xF0 ? 3 : c >= 0xE0 ? 2 : c >= 0xC0 ? 1 : 0;
        if (extra) c &= 0x3F >> extra;
        for (int k = 0; k < extra && *s; k++) c = (c << 6) | (static_cast<uint8_t>(*s++) & 0x3F);
        if (c >= 0x10000) {
            c -= 0x10000;
            out += static_cast<char16_t>(0xD800 + (c >> 10));
            out += static_cast<char16_t>(0xDC00 + (c & 0x3FF));
        } else {
            out += static_cast<char16_t>(c);
        }
    }
    return out;
}

void build_wide(char **from, std::vector<std::u16string> &strings, std::vector<char16_t *> &pointers) {
    if (!pointers.empty()) return;
    for (char **p = from; *p; p++) strings.push_back(utf8_to_utf16(*p));
    for (auto &s : strings) pointers.push_back(s.data());
    pointers.push_back(nullptr);
}

int daylight_flag;
long timezone_seconds;
char *tz_names[2] = {const_cast<char *>("UTC"), const_cast<char *>("UTC")};

// __stdio_common_v*printf options (bits of the first argument).
constexpr uint64_t STANDARD_SNPRINTF_BEHAVIOR = 2;
}  // namespace

extern "C" {

WINAPI int *ucrt___p___argc() { return &process::argc; }
WINAPI char ***ucrt___p___argv() { return &process::argv; }
WINAPI char16_t ***ucrt___p___wargv() {
    build_wide(process::argv, wide_args, wide_argv);
    static char16_t **p = wide_argv.data();
    return &p;
}
WINAPI char ***ucrt___p__environ() { return &process::envp; }
WINAPI char16_t ***ucrt___p__wenviron() {
    build_wide(process::envp, wide_env, wide_envp);
    static char16_t **p = wide_envp.data();
    return &p;
}
WINAPI char **ucrt__get_initial_narrow_environment() { return process::envp; }
WINAPI int ucrt__configure_narrow_argv(int) { return 0; }
WINAPI int ucrt__configure_wide_argv(int) { return 0; }
WINAPI int ucrt__initialize_narrow_environment() { return 0; }
WINAPI int ucrt__initialize_wide_environment() { return 0; }
WINAPI void ucrt__set_app_type(int) {}
WINAPI int ucrt__set_new_mode(int) { return 0; }
WINAPI void *ucrt__set_invalid_parameter_handler(void *) { return nullptr; }
WINAPI int ucrt__crt_atexit(Callback f) { return crt_atexit(f); }
WINAPI int ucrt__crt_at_quick_exit(Callback) { return 0; }
WINAPI int ucrt__initterm_e(int(WINAPI **begin)(), int(WINAPI **end)()) {
    for (auto f = begin; f < end; f++)
        if (*f)
            if (int r = (*f)()) return r;
    return 0;
}
WINAPI int *ucrt___p__commode() { return &commode; }
WINAPI int *ucrt___p__fmode() { return &fmode; }
WINAPI WinFile *ucrt___acrt_iob_func(unsigned index) { return index < 3 ? &iob[index] : nullptr; }

WINAPI int ucrt___stdio_common_vfprintf(uint64_t, WinFile *f, const char *fmt, void *, ms_va_list ap) {
    return crt_vfprintf(f, fmt, ap);
}
WINAPI int ucrt___stdio_common_vfwprintf(uint64_t, WinFile *f, const char16_t *fmt, void *, ms_va_list ap) {
    std::string s = format(utf16_to_utf8(fmt, -1).c_str(), ap, true);
    return write_all(fd_of(f), s.data(), s.size()) ? static_cast<int>(s.size()) : -1;
}
WINAPI int ucrt___stdio_common_vsprintf(uint64_t options, char *buf, size_t size, const char *fmt, void *, ms_va_list ap) {
    std::string s = format(fmt, ap);
    if (buf && size) {
        size_t n = s.size() < size ? s.size() : size - 1;
        memcpy(buf, s.data(), n);
        buf[n] = 0;
    }
    // C99 snprintf returns the full length; the legacy behaviour is -1 when the text was cut off.
    if (s.size() < size || (options & STANDARD_SNPRINTF_BEHAVIOR) || !buf) return static_cast<int>(s.size());
    return -1;
}
WINAPI int *ucrt___daylight() { return &daylight_flag; }
WINAPI long *ucrt___timezone() { return &timezone_seconds; }
WINAPI char **ucrt___tzname() { return tz_names; }
WINAPI void ucrt__tzset() {}
WINAPI int ucrt__fileno(WinFile *f) { return fd_of(f); }
WINAPI int ucrt__isatty(int fd) { return isatty(fd); }
WINAPI void ucrt__invalid_parameter_noinfo() {}
WINAPI void ucrt__invalid_parameter_noinfo_noreturn() { crt_abort(); }
WINAPI int ucrt__seh_filter_exe(DWORD, void *) { return 0; }  // EXCEPTION_CONTINUE_SEARCH

}  // extern "C"

#define UCRT(name) {#name, reinterpret_cast<void *>(&ucrt_##name)}

namespace {
const Export ucrt_exports[] = {
    UCRT(__acrt_iob_func), UCRT(__daylight), UCRT(__p___argc), UCRT(__p___argv), UCRT(__p___wargv),
    UCRT(__p__commode), UCRT(__p__environ), UCRT(__p__fmode), UCRT(__p__wenviron), UCRT(__stdio_common_vfprintf),
    UCRT(__stdio_common_vfwprintf), UCRT(__stdio_common_vsprintf), UCRT(__timezone), UCRT(__tzname),
    UCRT(_configure_narrow_argv), UCRT(_configure_wide_argv), UCRT(_crt_at_quick_exit), UCRT(_crt_atexit),
    UCRT(_fileno), UCRT(_get_initial_narrow_environment), UCRT(_initialize_narrow_environment),
    UCRT(_initialize_wide_environment), UCRT(_initterm_e), UCRT(_invalid_parameter_noinfo),
    UCRT(_invalid_parameter_noinfo_noreturn), UCRT(_isatty), UCRT(_seh_filter_exe), UCRT(_set_app_type),
    UCRT(_set_invalid_parameter_handler), UCRT(_set_new_mode), UCRT(_tzset),
};
}  // namespace

// Its own functions first; for everything else (malloc, strlen, fwrite, ...) find_export falls back
// to msvcrt's.
const BuiltinDll ucrt_dll{"ucrtbase.dll", ucrt_exports, sizeof ucrt_exports / sizeof ucrt_exports[0]};
