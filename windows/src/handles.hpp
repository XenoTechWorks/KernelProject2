// Windows handles: one table of kernel-object-like things (files, threads, events, mutexes,
// directory searches) that programs refer to by number, and waiting on them.
#pragma once

#include <dirent.h>
#include <pthread.h>
#include <string>

#include "win.hpp"

struct Object {
    enum class Kind { File, Thread, Event, Mutex, Search } kind;
    explicit Object(Kind k) : kind(k) {}
    virtual ~Object() = default;
};

struct FileObject : Object {
    int fd;
    explicit FileObject(int fd) : Object(Kind::File), fd(fd) {}
    ~FileObject() override;
};

struct ThreadObject : Object {
    pthread_t thread{};
    DWORD id = 0;
    bool started = false;  // false while CREATE_SUSPENDED
    bool done = false;
    DWORD exit_code = 259;  // STILL_ACTIVE
    ThreadObject() : Object(Kind::Thread) {}
};

struct EventObject : Object {
    bool manual_reset, signaled;
    EventObject(bool manual, bool initial) : Object(Kind::Event), manual_reset(manual), signaled(initial) {}
};

struct MutexObject : Object {
    long owner = 0;  // thread id, 0 when free
    int recursion = 0;
    MutexObject() : Object(Kind::Mutex) {}
};

struct SearchObject : Object {
    DIR *dir;
    std::string pattern;
    SearchObject(DIR *d, std::string p) : Object(Kind::Search), dir(d), pattern(std::move(p)) {}
    ~SearchObject() override;
};

namespace handles {
HANDLE add(Object *object);
// The object for a handle, if it is one of `kind`.
Object *get(HANDLE handle);
template <typename T> T *get_as(HANDLE handle, Object::Kind kind) {
    Object *o = get(handle);
    return o && o->kind == kind ? static_cast<T *>(o) : nullptr;
}
bool close(HANDLE handle);

// The Linux file descriptor behind a handle (standard handles included), or -1.
int fd_of(HANDLE handle);
HANDLE std_handle(int fd);

// Waiting: object states change under `lock`, and every change wakes waiters (`changed`).
extern pthread_mutex_t lock;
void changed();
}  // namespace handles
