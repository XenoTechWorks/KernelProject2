// kernel32.dll, files and directories: CreateFile, ReadFile/WriteFile, file pointers and sizes,
// attributes, directories, FindFirstFile, moving and deleting. Windows paths ("C:\\dir\\file")
// become Linux paths ("/dir/file").

#include <cerrno>
#include <cstring>
#include <fcntl.h>
#include <fnmatch.h>
#include <sys/stat.h>
#include <unistd.h>

#include "handles.hpp"

namespace {
constexpr DWORD GENERIC_READ = 0x80000000;
constexpr DWORD GENERIC_WRITE = 0x40000000;
constexpr DWORD GENERIC_ALL = 0x10000000;
constexpr DWORD CREATE_NEW = 1, CREATE_ALWAYS = 2, OPEN_EXISTING = 3, OPEN_ALWAYS = 4, TRUNCATE_EXISTING = 5;
constexpr DWORD FILE_FLAG_BACKUP_SEMANTICS = 0x02000000;
constexpr DWORD FILE_ATTRIBUTE_READONLY = 0x01;
constexpr DWORD FILE_ATTRIBUTE_DIRECTORY = 0x10;
constexpr DWORD FILE_ATTRIBUTE_ARCHIVE = 0x20;
constexpr DWORD INVALID_FILE_ATTRIBUTES = 0xFFFFFFFF;
HANDLE const INVALID_HANDLE_VALUE = reinterpret_cast<HANDLE>(-1);

DWORD attributes_of(const struct stat &st) {
    DWORD a = S_ISDIR(st.st_mode) ? FILE_ATTRIBUTE_DIRECTORY : FILE_ATTRIBUTE_ARCHIVE;
    if (!(st.st_mode & S_IWUSR)) a |= FILE_ATTRIBUTE_READONLY;
    return a;
}

// 100-nanosecond intervals since 1601.
uint64_t filetime(time_t t) { return (static_cast<uint64_t>(t) + 11644473600ull) * 10000000ull; }

// WIN32_FIND_DATAA, field by field.
void fill_find_data(uint8_t *out, const char *name, const struct stat &st) {
    memset(out, 0, 320);
    DWORD attrs = attributes_of(st);
    uint64_t t = filetime(st.st_mtime);
    uint32_t high = static_cast<uint32_t>(st.st_size >> 32), low = static_cast<uint32_t>(st.st_size);
    memcpy(out, &attrs, 4);
    memcpy(out + 4, &t, 8);   // creation
    memcpy(out + 12, &t, 8);  // last access
    memcpy(out + 20, &t, 8);  // last write
    memcpy(out + 28, &high, 4);
    memcpy(out + 32, &low, 4);
    strncpy(reinterpret_cast<char *>(out + 44), name, 259);
}

// Advances a search to its next match. False when there are no more.
bool next_match(SearchObject *s, uint8_t *data) {
    while (dirent *e = readdir(s->dir)) {
        if (fnmatch(s->pattern.c_str(), e->d_name, FNM_CASEFOLD) != 0) continue;
        struct stat st {};
        fstatat(dirfd(s->dir), e->d_name, &st, 0);
        fill_find_data(data, e->d_name, st);
        return true;
    }
    return false;
}
// Returns the length written, or the size needed (with the NUL) when the buffer is too small.
DWORD copy_out(const std::string &s, DWORD size, LPSTR buf) {
    if (s.size() + 1 > size) return static_cast<DWORD>(s.size() + 1);
    memcpy(buf, s.c_str(), s.size() + 1);
    return static_cast<DWORD>(s.size());
}
}  // namespace

extern "C" {

WINAPI HANDLE k32_CreateFileA(LPCSTR name, DWORD access, DWORD, void *, DWORD disposition, DWORD flags_and_attributes, HANDLE) {
    std::string path = unix_path(name);
    bool read = access & (GENERIC_READ | GENERIC_ALL), write = access & (GENERIC_WRITE | GENERIC_ALL);
    int flags = write ? (read ? O_RDWR : O_WRONLY) : O_RDONLY;
    struct stat st;
    bool existed = stat(path.c_str(), &st) == 0;
    switch (disposition) {
        case CREATE_NEW: flags |= O_CREAT | O_EXCL; break;
        case CREATE_ALWAYS: flags |= O_CREAT | O_TRUNC; break;
        case OPEN_ALWAYS: flags |= O_CREAT; break;
        case TRUNCATE_EXISTING: flags |= O_TRUNC; break;
        case OPEN_EXISTING: break;
        default: set_last_error(ERROR_INVALID_PARAMETER); return INVALID_HANDLE_VALUE;
    }
    if (existed && S_ISDIR(st.st_mode)) {
        if (!(flags_and_attributes & FILE_FLAG_BACKUP_SEMANTICS)) {
            set_last_error(ERROR_ACCESS_DENIED);
            return INVALID_HANDLE_VALUE;
        }
        flags = O_RDONLY | O_DIRECTORY;
    }
    int fd = open(path.c_str(), flags | O_CLOEXEC, 0644);
    if (fd < 0) {
        if (errno == EEXIST) set_last_error(ERROR_FILE_EXISTS);
        else set_error_from_errno(errno);
        return INVALID_HANDLE_VALUE;
    }
    // Windows reports when CREATE_ALWAYS / OPEN_ALWAYS found the file already there.
    set_last_error(existed && (disposition == CREATE_ALWAYS || disposition == OPEN_ALWAYS) ? ERROR_ALREADY_EXISTS : 0);
    return handles::add(new FileObject(fd));
}

WINAPI BOOL k32_WriteFile(HANDLE h, const void *data, DWORD size, DWORD *written, void *) {
    ssize_t n = write(handles::fd_of(h), data, size);
    if (written) *written = n < 0 ? 0 : static_cast<DWORD>(n);
    if (n < 0) set_error_from_errno(errno);
    return n >= 0;
}

WINAPI BOOL k32_ReadFile(HANDLE h, void *data, DWORD size, DWORD *got, void *) {
    ssize_t n = read(handles::fd_of(h), data, size);
    if (got) *got = n < 0 ? 0 : static_cast<DWORD>(n);
    if (n < 0) set_error_from_errno(errno);
    return n >= 0;
}

WINAPI BOOL k32_WriteConsoleA(HANDLE h, const void *data, DWORD size, DWORD *written, void *) {
    return k32_WriteFile(h, data, size, written, nullptr);
}

WINAPI BOOL k32_CloseHandle(HANDLE h) {
    if (!handles::close(h)) {
        set_last_error(ERROR_INVALID_HANDLE);
        return 0;
    }
    return 1;
}

WINAPI DWORD k32_GetFileSize(HANDLE h, DWORD *high) {
    struct stat st;
    if (fstat(handles::fd_of(h), &st) != 0) {
        set_error_from_errno(errno);
        return 0xFFFFFFFF;
    }
    if (high) *high = static_cast<DWORD>(st.st_size >> 32);
    return static_cast<DWORD>(st.st_size);
}

WINAPI BOOL k32_GetFileSizeEx(HANDLE h, int64_t *size) {
    struct stat st;
    if (fstat(handles::fd_of(h), &st) != 0) return set_error_from_errno(errno), 0;
    *size = st.st_size;
    return 1;
}

WINAPI BOOL k32_SetFilePointerEx(HANDLE h, int64_t distance, int64_t *new_position, DWORD method) {
    off_t at = lseek(handles::fd_of(h), distance, method == 0 ? SEEK_SET : method == 1 ? SEEK_CUR : SEEK_END);
    if (at < 0) return set_error_from_errno(errno), 0;
    if (new_position) *new_position = at;
    return 1;
}

WINAPI DWORD k32_SetFilePointer(HANDLE h, LONG low, LONG *high, DWORD method) {
    int64_t distance = high ? (static_cast<int64_t>(*high) << 32) | static_cast<uint32_t>(low) : low;
    int64_t at;
    if (!k32_SetFilePointerEx(h, distance, &at, method)) return 0xFFFFFFFF;
    if (high) *high = static_cast<LONG>(at >> 32);
    return static_cast<DWORD>(at);
}

WINAPI BOOL k32_SetEndOfFile(HANDLE h) {
    int fd = handles::fd_of(h);
    off_t at = lseek(fd, 0, SEEK_CUR);
    return at >= 0 && ftruncate(fd, at) == 0;
}

WINAPI BOOL k32_FlushFileBuffers(HANDLE h) { return fsync(handles::fd_of(h)) == 0 || errno == EINVAL; }

WINAPI DWORD k32_GetFileType(HANDLE h) {
    struct stat st;
    if (fstat(handles::fd_of(h), &st) != 0) return 0;  // FILE_TYPE_UNKNOWN
    if (S_ISCHR(st.st_mode)) return 2;                  // FILE_TYPE_CHAR
    if (S_ISFIFO(st.st_mode)) return 3;                 // FILE_TYPE_PIPE
    return 1;                                           // FILE_TYPE_DISK
}

WINAPI DWORD k32_GetFileAttributesA(LPCSTR name) {
    struct stat st;
    if (stat(unix_path(name).c_str(), &st) != 0) {
        set_error_from_errno(errno);
        return INVALID_FILE_ATTRIBUTES;
    }
    return attributes_of(st);
}

WINAPI BOOL k32_CreateDirectoryA(LPCSTR name, void *) {
    if (mkdir(unix_path(name).c_str(), 0755) != 0) return set_error_from_errno(errno), 0;
    return 1;
}

WINAPI BOOL k32_RemoveDirectoryA(LPCSTR name) {
    if (rmdir(unix_path(name).c_str()) != 0) return set_error_from_errno(errno), 0;
    return 1;
}

WINAPI BOOL k32_DeleteFileA(LPCSTR name) {
    if (unlink(unix_path(name).c_str()) != 0) return set_error_from_errno(errno), 0;
    return 1;
}

WINAPI BOOL k32_MoveFileA(LPCSTR from, LPCSTR to) {
    std::string target = unix_path(to);
    struct stat st;
    if (stat(target.c_str(), &st) == 0) {  // MoveFile doesn't replace; MoveFileEx can
        set_last_error(ERROR_ALREADY_EXISTS);
        return 0;
    }
    if (rename(unix_path(from).c_str(), target.c_str()) != 0) return set_error_from_errno(errno), 0;
    return 1;
}

WINAPI BOOL k32_MoveFileExA(LPCSTR from, LPCSTR to, DWORD flags) {
    if (flags & 1) {  // MOVEFILE_REPLACE_EXISTING
        if (rename(unix_path(from).c_str(), unix_path(to).c_str()) != 0) return set_error_from_errno(errno), 0;
        return 1;
    }
    return k32_MoveFileA(from, to);
}

WINAPI BOOL k32_SetCurrentDirectoryA(LPCSTR name) {
    if (chdir(unix_path(name).c_str()) != 0) return set_error_from_errno(errno), 0;
    return 1;
}

WINAPI DWORD k32_GetCurrentDirectoryA(DWORD size, LPSTR buf) {
    char cwd[4096];
    if (!getcwd(cwd, sizeof cwd)) return set_error_from_errno(errno), 0;
    return copy_out(windows_path(cwd), size, buf);
}

WINAPI DWORD k32_GetTempPathA(DWORD size, LPSTR buf) { return copy_out("C:\\tmp\\", size, buf); }

WINAPI DWORD k32_GetFullPathNameA(LPCSTR name, DWORD size, LPSTR buf, LPSTR *file_part) {
    std::string path = unix_path(name);
    if (!path.starts_with('/')) {
        char cwd[4096];
        if (getcwd(cwd, sizeof cwd)) path = std::string(cwd) + "/" + path;
    }
    std::string full = windows_path(path);
    DWORD n = copy_out(full, size, buf);
    if (file_part && n < size) {
        char *slash = strrchr(buf, '\\');
        *file_part = slash ? slash + 1 : buf;
    }
    return n;
}

WINAPI HANDLE k32_FindFirstFileA(LPCSTR pattern, uint8_t *data) {
    std::string path = unix_path(pattern);
    size_t slash = path.rfind('/');
    std::string dir = slash == std::string::npos ? "." : slash == 0 ? "/" : path.substr(0, slash);
    std::string wildcard = slash == std::string::npos ? path : path.substr(slash + 1);
    DIR *d = opendir(dir.c_str());
    if (!d) {
        set_error_from_errno(errno);
        return INVALID_HANDLE_VALUE;
    }
    auto *search = new SearchObject(d, wildcard == "*.*" ? "*" : wildcard);
    if (!next_match(search, data)) {
        delete search;
        set_last_error(ERROR_FILE_NOT_FOUND);
        return INVALID_HANDLE_VALUE;
    }
    return handles::add(search);
}

WINAPI BOOL k32_FindNextFileA(HANDLE h, uint8_t *data) {
    auto *search = handles::get_as<SearchObject>(h, Object::Kind::Search);
    if (!search) return set_last_error(ERROR_INVALID_HANDLE), 0;
    if (!next_match(search, data)) return set_last_error(ERROR_NO_MORE_FILES), 0;
    return 1;
}

WINAPI BOOL k32_FindClose(HANDLE h) { return handles::close(h); }

WINAPI HANDLE k32_GetStdHandle(DWORD which) {
    switch (which) {
        case static_cast<DWORD>(-10): return handles::std_handle(0);
        case static_cast<DWORD>(-11): return handles::std_handle(1);
        case static_cast<DWORD>(-12): return handles::std_handle(2);
        default: return INVALID_HANDLE_VALUE;
    }
}

}  // extern "C"

#define EXPORT(name) {#name, reinterpret_cast<void *>(&k32_##name)}

namespace {
const Export exports[] = {
    EXPORT(CloseHandle), EXPORT(CreateDirectoryA), EXPORT(CreateFileA), EXPORT(DeleteFileA), EXPORT(FindClose),
    EXPORT(FindFirstFileA), EXPORT(FindNextFileA), EXPORT(FlushFileBuffers), EXPORT(GetCurrentDirectoryA),
    EXPORT(GetFileAttributesA), EXPORT(GetFileSize), EXPORT(GetFileSizeEx), EXPORT(GetFileType),
    EXPORT(GetFullPathNameA), EXPORT(GetStdHandle), EXPORT(GetTempPathA),
    EXPORT(MoveFileA), EXPORT(MoveFileExA), EXPORT(ReadFile), EXPORT(RemoveDirectoryA),
    EXPORT(SetCurrentDirectoryA), EXPORT(SetEndOfFile), EXPORT(SetFilePointer), EXPORT(SetFilePointerEx),
    EXPORT(WriteConsoleA), EXPORT(WriteFile),
};
}  // namespace

const BuiltinDll kernel32_files_dll{"kernel32.dll", exports, sizeof exports / sizeof exports[0]};
