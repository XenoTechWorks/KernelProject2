// The handle table, Windows error codes, and Windows paths.

#include "handles.hpp"

#include <cerrno>
#include <map>
#include <unistd.h>

FileObject::~FileObject() { ::close(fd); }
SearchObject::~SearchObject() { closedir(dir); }

namespace {
std::map<uintptr_t, Object *> table;
uintptr_t next_handle = 0x1000;
pthread_mutex_t table_lock = PTHREAD_MUTEX_INITIALIZER;
}  // namespace

pthread_mutex_t handles::lock = PTHREAD_MUTEX_INITIALIZER;

HANDLE handles::add(Object *object) {
    pthread_mutex_lock(&table_lock);
    uintptr_t h = next_handle;
    next_handle += 4;
    table[h] = object;
    pthread_mutex_unlock(&table_lock);
    return reinterpret_cast<HANDLE>(h);
}

Object *handles::get(HANDLE handle) {
    pthread_mutex_lock(&table_lock);
    auto it = table.find(reinterpret_cast<uintptr_t>(handle));
    Object *o = it == table.end() ? nullptr : it->second;
    pthread_mutex_unlock(&table_lock);
    return o;
}

bool handles::close(HANDLE handle) {
    pthread_mutex_lock(&table_lock);
    auto it = table.find(reinterpret_cast<uintptr_t>(handle));
    Object *o = nullptr;
    if (it != table.end()) {
        o = it->second;
        table.erase(it);
    }
    pthread_mutex_unlock(&table_lock);
    // A thread object stays alive until its thread ends; it frees itself then (threads.cpp).
    if (o && o->kind != Object::Kind::Thread) delete o;
    return o != nullptr;
}

// Standard handles are small numbers, like Windows' pseudo-handles for the console.
HANDLE handles::std_handle(int fd) { return reinterpret_cast<HANDLE>(static_cast<uintptr_t>(fd + 1) * 4); }

int handles::fd_of(HANDLE handle) {
    uintptr_t h = reinterpret_cast<uintptr_t>(handle);
    if (h >= 4 && h <= 12 && h % 4 == 0) return static_cast<int>(h / 4) - 1;
    auto *f = get_as<FileObject>(handle, Object::Kind::File);
    return f ? f->fd : -1;
}

namespace {
pthread_cond_t changed_cond = [] {
    pthread_cond_t c;
    pthread_condattr_t attr;
    pthread_condattr_init(&attr);
    pthread_condattr_setclock(&attr, CLOCK_MONOTONIC);
    pthread_cond_init(&c, &attr);
    return c;
}();
}  // namespace

void handles::changed() { pthread_cond_broadcast(&changed_cond); }

// Shared with threads.cpp: wait on `changed_cond` (with handles::lock held) until `deadline`.
bool wait_for_change(const timespec *deadline) {
    if (!deadline) return pthread_cond_wait(&changed_cond, &handles::lock) == 0;
    return pthread_cond_timedwait(&changed_cond, &handles::lock, deadline) == 0;
}

DWORD set_error_from_errno(int e) {
    DWORD error;
    switch (e) {
        case ENOENT: error = ERROR_FILE_NOT_FOUND; break;
        case ENOTDIR: error = ERROR_PATH_NOT_FOUND; break;
        case EACCES:
        case EPERM:
        case EISDIR: error = ERROR_ACCESS_DENIED; break;
        case EEXIST: error = ERROR_ALREADY_EXISTS; break;
        case ENOTEMPTY: error = ERROR_DIR_NOT_EMPTY; break;
        case EBADF: error = ERROR_INVALID_HANDLE; break;
        case ENOMEM: error = ERROR_NOT_ENOUGH_MEMORY; break;
        case ENOSPC: error = 112; break;  // ERROR_DISK_FULL
        default: error = ERROR_INVALID_PARAMETER; break;
    }
    set_last_error(error);
    return error;
}

std::string unix_path(const char *path) {
    std::string p = path;
    if (p.size() >= 2 && p[1] == ':' && isalpha(static_cast<unsigned char>(p[0]))) p.erase(0, 2);
    for (char &c : p)
        if (c == '\\') c = '/';
    return p.empty() ? "/" : p;
}

std::string windows_path(const std::string &path) {
    std::string p = path;
    for (char &c : p)
        if (c == '/') c = '\\';
    return path.starts_with('/') ? "C:" + p : p;
}
