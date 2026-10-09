// kernel32.dll, file mappings: CreateFileMapping / MapViewOfFile, memory shared with files and between
// processes. A mapping is a file descriptor (a file, or an anonymous one for "page file" mappings),
// so like other file handles it is inherited by child processes.
//
// Our kernel copies file contents into a mapping rather than sharing the memory, so writes to a
// shared view are written back to the file before a child process starts (flush_shared_views), and
// on FlushViewOfFile / UnmapViewOfFile. That covers the common pattern of handing state to a child
// through a mapping.

#include <cerrno>
#include <cstring>
#include <fcntl.h>
#include <map>
#include <string>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

#include "handles.hpp"

namespace {
constexpr DWORD PAGE_READWRITE = 0x04, PAGE_WRITECOPY = 0x08, PAGE_EXECUTE_READWRITE = 0x40;
constexpr DWORD FILE_MAP_COPY = 0x1, FILE_MAP_WRITE = 0x2;

struct View {
    size_t size;
    int fd;
    uint64_t offset;
    bool writable;
};
std::map<uint8_t *, View> views;
pthread_mutex_t views_lock = PTHREAD_MUTEX_INITIALIZER;

void write_back(uint8_t *at, const View &v) {
    if (!v.writable) return;
    for (size_t done = 0; done < v.size;) {
        ssize_t n = pwrite(v.fd, at + done, v.size - done, static_cast<off_t>(v.offset + done));
        if (n <= 0) break;
        done += n;
    }
}

bool inheritable(const void *security) {
    if (!security) return false;
    BOOL inherit;
    memcpy(&inherit, static_cast<const uint8_t *>(security) + 16, sizeof inherit);  // bInheritHandle
    return inherit;
}

std::string narrow(LPCWSTR w) {
    std::string s;
    for (; w && *w; w++) s += static_cast<char>(*w < 0x80 ? *w : '_');
    return s;
}

HANDLE create_mapping(HANDLE file, const void *security, DWORD protect, uint64_t size, const std::string &name) {
    int fd;
    if (file == reinterpret_cast<HANDLE>(-1)) {
        // Backed by "the page file": a temporary file, or a named one other processes can open.
        std::string path = name.empty() ? "/tmp/.winmap-XXXXXX" : "/tmp/.winmap-" + name;
        if (name.empty()) {
            fd = mkstemp(path.data());
            if (fd >= 0) unlink(path.c_str());
        } else {
            fd = open(path.c_str(), O_RDWR | O_CREAT, 0600);
        }
        if (fd < 0) return set_error_from_errno(errno), nullptr;
        struct stat st;
        if (fstat(fd, &st) == 0 && static_cast<uint64_t>(st.st_size) < size) (void)!ftruncate(fd, static_cast<off_t>(size));
    } else {
        int source = handles::fd_of(file);
        if (source < 0) return set_last_error(ERROR_INVALID_HANDLE), nullptr;
        fd = dup(source);
        struct stat st;
        bool can_grow = protect == PAGE_READWRITE || protect == PAGE_EXECUTE_READWRITE;
        if (fstat(fd, &st) == 0 && can_grow && size > static_cast<uint64_t>(st.st_size)) (void)!ftruncate(fd, static_cast<off_t>(size));
    }
    fcntl(fd, F_SETFD, inheritable(security) ? 0 : FD_CLOEXEC);
    return handles::for_fd(fd);
}
}  // namespace

// Writes every shared, writable view back to its file (before a child process starts).
void flush_shared_views() {
    pthread_mutex_lock(&views_lock);
    for (auto &[at, v] : views) write_back(at, v);
    pthread_mutex_unlock(&views_lock);
}

extern "C" {

WINAPI HANDLE k32_CreateFileMappingA(HANDLE file, void *security, DWORD protect, DWORD high, DWORD low, LPCSTR name) {
    return create_mapping(file, security, protect, (static_cast<uint64_t>(high) << 32) | low, name ? name : "");
}
WINAPI HANDLE k32_CreateFileMappingW(HANDLE file, void *security, DWORD protect, DWORD high, DWORD low, LPCWSTR name) {
    return create_mapping(file, security, protect, (static_cast<uint64_t>(high) << 32) | low, name ? narrow(name) : "");
}
WINAPI HANDLE k32_OpenFileMappingA(DWORD, BOOL inherit, LPCSTR name) {
    int fd = open(("/tmp/.winmap-" + std::string(name)).c_str(), O_RDWR | (inherit ? 0 : O_CLOEXEC));
    if (fd < 0) return set_last_error(ERROR_FILE_NOT_FOUND), nullptr;
    return handles::for_fd(fd);
}

WINAPI void *k32_MapViewOfFileEx(HANDLE mapping, DWORD access, DWORD high, DWORD low, SIZE_T size, void *at) {
    int fd = handles::fd_of(mapping);
    if (fd < 0) return set_last_error(ERROR_INVALID_HANDLE), nullptr;
    uint64_t offset = (static_cast<uint64_t>(high) << 32) | low;
    if (size == 0) {
        struct stat st;
        if (fstat(fd, &st) != 0) return set_error_from_errno(errno), nullptr;
        size = static_cast<size_t>(st.st_size - offset);
    }
    bool writable = access & FILE_MAP_WRITE;
    bool copy = access & FILE_MAP_COPY;
    int prot = PROT_READ | (writable || copy ? PROT_WRITE : 0);
    void *p = mmap(at, size, prot, (copy ? MAP_PRIVATE : MAP_SHARED) | (at ? MAP_FIXED_NOREPLACE : 0), fd, static_cast<off_t>(offset));
    if (p == MAP_FAILED) return set_error_from_errno(errno), nullptr;
    // The view keeps its own descriptor: closing the mapping handle doesn't end its views.
    int view_fd = fcntl(fd, F_DUPFD_CLOEXEC, 0);
    pthread_mutex_lock(&views_lock);
    views[static_cast<uint8_t *>(p)] = View{size, view_fd, offset, writable && !copy};
    pthread_mutex_unlock(&views_lock);
    return p;
}
WINAPI void *k32_MapViewOfFile(HANDLE mapping, DWORD access, DWORD high, DWORD low, SIZE_T size) {
    return k32_MapViewOfFileEx(mapping, access, high, low, size, nullptr);
}

WINAPI BOOL k32_FlushViewOfFile(const void *address, SIZE_T) {
    pthread_mutex_lock(&views_lock);
    auto it = views.upper_bound(const_cast<uint8_t *>(static_cast<const uint8_t *>(address)));
    if (it != views.begin()) {
        --it;
        write_back(it->first, it->second);
    }
    pthread_mutex_unlock(&views_lock);
    return 1;
}

WINAPI BOOL k32_UnmapViewOfFile(const void *address) {
    pthread_mutex_lock(&views_lock);
    auto it = views.find(const_cast<uint8_t *>(static_cast<const uint8_t *>(address)));
    if (it == views.end()) {
        pthread_mutex_unlock(&views_lock);
        set_last_error(ERROR_INVALID_PARAMETER);
        return 0;
    }
    View v = it->second;
    views.erase(it);
    pthread_mutex_unlock(&views_lock);
    write_back(const_cast<uint8_t *>(static_cast<const uint8_t *>(address)), v);
    munmap(const_cast<void *>(address), v.size);
    close(v.fd);
    return 1;
}

}  // extern "C"

#define EXPORT(name) {#name, reinterpret_cast<void *>(&k32_##name)}

namespace {
const Export exports[] = {
    EXPORT(CreateFileMappingA), EXPORT(CreateFileMappingW), EXPORT(FlushViewOfFile), EXPORT(MapViewOfFile),
    EXPORT(MapViewOfFileEx), EXPORT(OpenFileMappingA), EXPORT(UnmapViewOfFile),
};
}  // namespace

const BuiltinDll kernel32_memory_dll{"kernel32.dll", exports, sizeof exports / sizeof exports[0]};
