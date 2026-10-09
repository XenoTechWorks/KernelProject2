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
#include <strings.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>
#include <vector>

#include "handles.hpp"
#include "win.hpp"

using ms_va_list = __builtin_ms_va_list;

void crt_set_program_args(char **argv, char **envp);  // below

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
    crt_set_program_args(process::argv, process::envp);
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

// ── POSIX-style functions, process start, sorting, setjmp (what bigger programs like BusyBox use) ─

namespace {
// qsort and bsearch call back into the program with the Windows convention; a Linux libc can't, so
// the comparison goes through this thread's current Windows comparator.
using WinCompare = int(WINAPI *)(const void *, const void *);
thread_local WinCompare current_compare;
int call_compare(const void *a, const void *b) { return current_compare(a, b); }

char **program_argv;  // __argv
char **program_environ;  // _environ
long timezone_value;  // _timezone

int set_errno_result(int r) {
    if (r < 0) win_errno = errno;
    return r;
}

// Windows' _open flags.
int linux_open_flags(int f) {
    int out = (f & 3) == 1 ? O_WRONLY : (f & 3) == 2 ? O_RDWR : O_RDONLY;
    if (f & 0x0008) out |= O_APPEND;
    if (f & 0x0100) out |= O_CREAT;
    if (f & 0x0200) out |= O_TRUNC;
    if (f & 0x0400) out |= O_EXCL;
    return out;
}
}  // namespace

extern "C" {

WINAPI int crt__vscprintf(const char *fmt, ms_va_list ap) { return static_cast<int>(format(fmt, ap).size()); }
WINAPI int crt_sscanf(const char *input, const char *fmt, ...) {
    // The common cases: %d %u %x %p (with l / ll), %s %c and %n, with literal text between.
    ms_va_list ap;
    __builtin_ms_va_start(ap, fmt);
    int matched = 0;
    const char *in = input;
    for (const char *p = fmt; *p; p++) {
        if (isspace(static_cast<unsigned char>(*p))) {
            while (isspace(static_cast<unsigned char>(*in))) in++;
            continue;
        }
        if (*p != '%') {
            if (*in != *p) break;
            in++;
            continue;
        }
        p++;
        if (*p == 'l') p++;
        if (*p == 'l') p++;
        if (*p != 'c' && *p != 'n') while (isspace(static_cast<unsigned char>(*in))) in++;
        char *end;
        if (*p == 'd' || *p == 'i') {
            long v = strtol(in, &end, *p == 'i' ? 0 : 10);
            if (end == in) break;
            *static_cast<int *>(__builtin_va_arg(ap, void *)) = static_cast<int>(v);
            in = end;
        } else if (*p == 'u' || *p == 'x' || *p == 'X') {
            unsigned long long v = strtoull(in, &end, *p == 'u' ? 10 : 16);
            if (end == in) break;
            if (p[-1] == 'l') *static_cast<uint64_t *>(__builtin_va_arg(ap, void *)) = v;
            else *static_cast<unsigned *>(__builtin_va_arg(ap, void *)) = static_cast<unsigned>(v);
            in = end;
        } else if (*p == 'p') {
            unsigned long long v = strtoull(in, &end, 16);
            if (end == in) break;
            *static_cast<void **>(__builtin_va_arg(ap, void *)) = reinterpret_cast<void *>(v);
            in = end;
        } else if (*p == 's') {
            char *out = static_cast<char *>(__builtin_va_arg(ap, void *));
            if (!*in) break;
            while (*in && !isspace(static_cast<unsigned char>(*in))) *out++ = *in++;
            *out = 0;
        } else if (*p == 'c') {
            if (!*in) break;
            *static_cast<char *>(__builtin_va_arg(ap, void *)) = *in++;
        } else if (*p == 'n') {
            *static_cast<int *>(__builtin_va_arg(ap, void *)) = static_cast<int>(in - input);
            continue;
        } else {
            break;
        }
        matched++;
    }
    __builtin_ms_va_end(ap);
    return matched;
}

WINAPI int crt__open(const char *path, int flags, int mode) {
    return set_errno_result(open(unix_path(path).c_str(), linux_open_flags(flags), mode ? mode : 0644));
}
WINAPI int crt__creat(const char *path, int mode) { return crt__open(path, 0x0301, mode); }
WINAPI int crt__close(int fd) { return set_errno_result(close(fd)); }
WINAPI int crt__dup(int fd) { return set_errno_result(dup(fd)); }
WINAPI int crt__dup2(int from, int to) { return set_errno_result(dup2(from, to)) < 0 ? -1 : 0; }
WINAPI int crt__pipe(int *fds, unsigned, int) { return set_errno_result(pipe(fds)); }
WINAPI int64_t crt__telli64(int fd) { return lseek(fd, 0, SEEK_CUR); }
WINAPI int crt__setmode(int, int mode) { return mode == 0x8000 ? 0x4000 : 0x8000; }  // binary <-> text: same here
WINAPI int crt__isatty(int fd) { return isatty(fd); }
WINAPI int crt__access(const char *path, int mode) { return set_errno_result(access(unix_path(path).c_str(), mode & 6)); }
WINAPI int crt__unlink(const char *path) { return set_errno_result(unlink(unix_path(path).c_str())); }
WINAPI int crt_rename(const char *from, const char *to) { return set_errno_result(rename(unix_path(from).c_str(), unix_path(to).c_str())); }
WINAPI int crt__mkdir(const char *path) { return set_errno_result(mkdir(unix_path(path).c_str(), 0755)); }
WINAPI int crt__rmdir(const char *path) { return set_errno_result(rmdir(unix_path(path).c_str())); }
WINAPI int crt__chdir(const char *path) { return set_errno_result(chdir(unix_path(path).c_str())); }
WINAPI int crt__chmod(const char *path, int mode) { return set_errno_result(chmod(unix_path(path).c_str(), mode & 0x80 ? 0644 : 0444)); }
WINAPI int crt__umask(int mask) { return static_cast<int>(umask(mask)); }
WINAPI int crt__getpid() { return getpid(); }
WINAPI char *crt__getcwd(char *buf, int size) {
    char cwd[4096];
    if (!getcwd(cwd, sizeof cwd)) return nullptr;
    std::string w = windows_path(cwd);
    if (!buf) return strdup(w.c_str());
    if (static_cast<int>(w.size()) + 1 > size) return nullptr;
    memcpy(buf, w.c_str(), w.size() + 1);
    return buf;
}
WINAPI char *crt__fullpath(char *buf, const char *path, size_t size) {
    std::string p = unix_path(path);
    if (!p.starts_with('/')) {
        char cwd[4096];
        if (getcwd(cwd, sizeof cwd)) p = std::string(cwd) + "/" + p;
    }
    std::string w = windows_path(p);
    if (!buf) return strdup(w.c_str());
    if (w.size() + 1 > size) return nullptr;
    memcpy(buf, w.c_str(), w.size() + 1);
    return buf;
}
// Fills in the XXXXXX at the end of `tmpl` with a name that doesn't exist yet.
WINAPI char *crt__mktemp(char *tmpl) {
    size_t len = strlen(tmpl);
    if (len < 6 || strcmp(tmpl + len - 6, "XXXXXX") != 0) return nullptr;
    static const char letters[] = "abcdefghijklmnopqrstuvwxyz0123456789";
    for (int attempt = 0; attempt < 100; attempt++) {
        for (size_t i = len - 6; i < len; i++) tmpl[i] = letters[rand() % 36];
        if (access(unix_path(tmpl).c_str(), F_OK) != 0) return tmpl;
    }
    return nullptr;
}
// "NAME=value" sets a variable; "NAME=" removes it, as on Windows. _environ follows the change.
WINAPI int crt__putenv(const char *entry) {
    const char *eq = strchr(entry, '=');
    if (!eq) return -1;
    std::string name(entry, eq);
    int r = eq[1] ? setenv(name.c_str(), eq + 1, 1) : unsetenv(name.c_str());
    program_environ = environ;
    return r;
}
WINAPI int crt__getch() {
    unsigned char c;
    return read(0, &c, 1) == 1 ? c : -1;
}
WINAPI int crt__getche() { return crt__getch(); }
// File handles are file descriptors in disguise (see handles.hpp): (fd + 1) * 4.
WINAPI intptr_t crt__get_osfhandle(int fd) { return fd >= 0 ? (static_cast<intptr_t>(fd) + 1) * 4 : -1; }
WINAPI int crt__open_osfhandle(intptr_t handle, int) {
    return handle >= 4 && handle < 0x100000 && handle % 4 == 0 ? static_cast<int>(handle / 4 - 1) : -1;
}
WINAPI int crt__filbuf(WinFile *f) { return crt_getc(f); }
WINAPI int crt_feof(WinFile *) { return 0; }
WINAPI int crt_ferror(WinFile *) { return 0; }
WINAPI void crt_clearerr(WinFile *) {}
WINAPI void crt_setbuf(WinFile *, char *) {}
WINAPI long crt_clock() { return static_cast<long>(clock() / (CLOCKS_PER_SEC / 1000)); }

// Process creation: runs another program and waits (_P_WAIT) or not (_P_NOWAIT).
WINAPI intptr_t crt__spawnve(int mode, const char *path, char *const *argv, char *const *env) {
    flush_shared_views();
    pid_t pid = fork();
    if (pid == 0) {
        std::string p = unix_path(path);
        exec_program(p.c_str(), argv, env ? env : process::envp);
        _exit(127);
    }
    if (pid < 0) return set_errno_result(-1);
    // _P_NOWAIT: a process handle to wait on, as msvcrt returns.
    if (mode != 0) return reinterpret_cast<intptr_t>(handles::add(new ProcessObject(pid)));
    int status = 0;
    waitpid(pid, &status, 0);
    return WIFEXITED(status) ? WEXITSTATUS(status) : 255;
}

// Waits for a process started with _spawn*(_P_NOWAIT); its exit code goes to *status.
WINAPI intptr_t crt__cwait(int *status, intptr_t handle, int) {
    auto *p = handles::get_as<ProcessObject>(reinterpret_cast<HANDLE>(handle), Object::Kind::Process);
    if (!p) {
        win_errno = ECHILD;
        return -1;
    }
    int raw = 0;
    if (!p->done && waitpid(p->pid, &raw, 0) == p->pid) {
        p->done = true;
        p->exit_code = WIFEXITED(raw) ? static_cast<DWORD>(WEXITSTATUS(raw)) : 255;
    }
    if (status) *status = static_cast<int>(p->exit_code);
    return handle;
}

WINAPI double crt_atof(const char *s) { return atof(s); }
WINAPI double crt_ldexp(double x, int e) { return ldexp(x, e); }
WINAPI void crt_qsort(void *base, size_t n, size_t size, WinCompare compare) {
    WinCompare saved = current_compare;
    current_compare = compare;
    qsort(base, n, size, call_compare);
    current_compare = saved;
}
WINAPI void *crt_bsearch(const void *key, const void *base, size_t n, size_t size, WinCompare compare) {
    WinCompare saved = current_compare;
    current_compare = compare;
    void *r = bsearch(key, base, n, size, call_compare);
    current_compare = saved;
    return r;
}
WINAPI int crt__stricmp(const char *a, const char *b) { return strcasecmp(a, b); }
WINAPI int crt__strnicmp(const char *a, const char *b, size_t n) { return strncasecmp(a, b, n); }
WINAPI size_t crt_strcspn(const char *s, const char *reject) { return strcspn(s, reject); }
WINAPI size_t crt_strspn(const char *s, const char *accept) { return strspn(s, accept); }
WINAPI char *crt_strpbrk(const char *s, const char *accept) { return const_cast<char *>(strpbrk(s, accept)); }
WINAPI char *crt_strtok(char *s, const char *delim) {
    static char *saved;
    return strtok_r(s, delim, &saved);
}
WINAPI int64_t crt__strtoi64(const char *s, char **end, int base) { return strtoll(s, end, base); }
WINAPI uint64_t crt__strtoui64(const char *s, char **end, int base) { return strtoull(s, end, base); }
WINAPI char16_t *crt_wcscpy(char16_t *d, const char16_t *s) {
    size_t i = 0;
    for (; s[i]; i++) d[i] = s[i];
    d[i] = 0;
    return d;
}
WINAPI int crt_wcsncmp(const char16_t *a, const char16_t *b, size_t n) {
    for (size_t i = 0; i < n; i++) {
        if (a[i] != b[i] || !a[i]) return a[i] - b[i];
    }
    return 0;
}
WINAPI int crt__wcsnicmp(const char16_t *a, const char16_t *b, size_t n) {
    for (size_t i = 0; i < n; i++) {
        int x = a[i] < 0x80 ? tolower(a[i]) : a[i], y = b[i] < 0x80 ? tolower(b[i]) : b[i];
        if (x != y || !a[i]) return x - y;
    }
    return 0;
}
#define CTYPE(name) \
    WINAPI int crt_##name(int c) { return name(c); }
CTYPE(isalnum)
CTYPE(iscntrl)
CTYPE(isgraph)
CTYPE(islower)
CTYPE(isprint)
CTYPE(ispunct)
CTYPE(isupper)
CTYPE(isxdigit)

// Time: Windows' 64-bit time_t is seconds since 1970, like Linux's; struct tm is the same nine ints.
WINAPI struct tm *crt__gmtime64(const int64_t *t) {
    time_t v = *t;
    return gmtime(&v);
}
WINAPI struct tm *crt__localtime64(const int64_t *t) {
    time_t v = *t;
    return localtime(&v);
}
WINAPI int64_t crt__mktime64(struct tm *tm) { return mktime(tm); }
WINAPI char *crt__ctime64(const int64_t *t) {
    time_t v = *t;
    return ctime(&v);
}

}  // extern "C"

// setjmp / longjmp with Windows' x64 jump buffer: Frame, Rbx, Rsp, Rbp, Rsi, Rdi, R12-R15, Rip, MxCsr,
// FpCsr, Spare, then Xmm6-15. (longjmp doesn't unwind SEH frames on the way; plain C doesn't need it.)
asm(R"(
.intel_syntax noprefix
.text
.global crt__setjmp
crt__setjmp:
    mov [rcx+0x00], rdx
    mov [rcx+0x08], rbx
    lea rax, [rsp+8]
    mov [rcx+0x10], rax
    mov [rcx+0x18], rbp
    mov [rcx+0x20], rsi
    mov [rcx+0x28], rdi
    mov [rcx+0x30], r12
    mov [rcx+0x38], r13
    mov [rcx+0x40], r14
    mov [rcx+0x48], r15
    mov rax, [rsp]
    mov [rcx+0x50], rax
    stmxcsr [rcx+0x58]
    fnstcw [rcx+0x5C]
    movdqu [rcx+0x60], xmm6
    movdqu [rcx+0x70], xmm7
    movdqu [rcx+0x80], xmm8
    movdqu [rcx+0x90], xmm9
    movdqu [rcx+0xA0], xmm10
    movdqu [rcx+0xB0], xmm11
    movdqu [rcx+0xC0], xmm12
    movdqu [rcx+0xD0], xmm13
    movdqu [rcx+0xE0], xmm14
    movdqu [rcx+0xF0], xmm15
    xor eax, eax
    ret
.global crt_longjmp
crt_longjmp:
    mov eax, edx
    test eax, eax
    jnz 1f
    inc eax
1:
    mov rbx, [rcx+0x08]
    mov rsp, [rcx+0x10]
    mov rbp, [rcx+0x18]
    mov rsi, [rcx+0x20]
    mov rdi, [rcx+0x28]
    mov r12, [rcx+0x30]
    mov r13, [rcx+0x38]
    mov r14, [rcx+0x40]
    mov r15, [rcx+0x48]
    ldmxcsr [rcx+0x58]
    fldcw [rcx+0x5C]
    movdqu xmm6, [rcx+0x60]
    movdqu xmm7, [rcx+0x70]
    movdqu xmm8, [rcx+0x80]
    movdqu xmm9, [rcx+0x90]
    movdqu xmm10, [rcx+0xA0]
    movdqu xmm11, [rcx+0xB0]
    movdqu xmm12, [rcx+0xC0]
    movdqu xmm13, [rcx+0xD0]
    movdqu xmm14, [rcx+0xE0]
    movdqu xmm15, [rcx+0xF0]
    jmp qword ptr [rcx+0x50]
.att_syntax prefix
)");

extern "C" {
int crt__setjmp();
void crt_longjmp();
}

#define MORE(name) {#name, reinterpret_cast<void *>(&crt_##name)}

namespace {
const Export more_exports[] = {
    MORE(_vscprintf), MORE(sscanf), MORE(_open), MORE(_creat), MORE(_close), MORE(_dup), MORE(_dup2), MORE(_pipe),
    MORE(_telli64), MORE(_setmode), MORE(_isatty), MORE(_access), MORE(_unlink), MORE(rename), MORE(_mkdir),
    MORE(_rmdir), MORE(_chdir), MORE(_chmod), MORE(_umask), MORE(_getpid), MORE(_getcwd), MORE(_fullpath),
    MORE(_mktemp), MORE(_putenv), MORE(_cwait), MORE(_getch), MORE(_getche), MORE(_get_osfhandle), MORE(_open_osfhandle),
    MORE(_filbuf), MORE(feof), MORE(ferror), MORE(clearerr), MORE(setbuf), MORE(clock), MORE(_spawnve), MORE(atof),
    MORE(ldexp), MORE(qsort), MORE(bsearch), MORE(_stricmp), MORE(_strnicmp), MORE(strcspn), MORE(strspn),
    MORE(strpbrk), MORE(strtok), MORE(_strtoi64), MORE(_strtoui64), MORE(wcscpy), MORE(wcsncmp), MORE(_wcsnicmp),
    MORE(isalnum), MORE(iscntrl), MORE(isgraph), MORE(islower), MORE(isprint), MORE(ispunct), MORE(isupper),
    MORE(isxdigit), MORE(_gmtime64), MORE(_localtime64), MORE(_mktime64), MORE(_ctime64), MORE(_setjmp),
    MORE(longjmp),
    {"_stricmp", reinterpret_cast<void *>(&crt__stricmp)},
    {"__argv", reinterpret_cast<void *>(&program_argv)},
    {"_environ", reinterpret_cast<void *>(&program_environ)},
    {"_timezone", reinterpret_cast<void *>(&timezone_value)},
};
}  // namespace

const BuiltinDll msvcrt_more_dll{"msvcrt.dll", more_exports, sizeof more_exports / sizeof more_exports[0]};

// Called by __getmainargs (above) so the data exports __argv and _environ are filled in too.
void crt_set_program_args(char **argv, char **envp) {
    program_argv = argv;
    program_environ = envp;
}

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
