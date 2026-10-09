// Windows handles: one table of kernel-object-like things (files, threads, events, mutexes,
// directory searches) that programs refer to by number, and waiting on them.
#pragma once

#include <dirent.h>
#include <pthread.h>
#include <string>

#include "win.hpp"

// Handles come in two kinds. Anything that is a Linux file descriptor underneath (files, pipes,
// shared memory, the console) is the descriptor itself in disguise: (fd + 1) * 4. A child process
// inherits descriptors, so the same handle number works there too, as inherited handles do on
// Windows. Everything else (threads, events, mutexes, processes, searches) is an object in this
// process's table, numbered from 0x100000.
struct Object {
    enum class Kind { Thread, Event, Mutex, Search, Process } kind;
    explicit Object(Kind k) : kind(k) {}
    virtual ~Object() = default;
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

// A child process started with CreateProcess (a Linux process underneath).
struct ProcessObject : Object {
    int pid;
    bool done = false;
    DWORD exit_code = 259;  // STILL_ACTIVE
    explicit ProcessObject(int pid) : Object(Kind::Process), pid(pid) {}
    // Collects the exit status if it has ended (without waiting). True when it has.
    bool check();
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
// The handle for a file descriptor.
HANDLE for_fd(int fd);
inline HANDLE std_handle(int fd) { return for_fd(fd); }

// Waiting: object states change under `lock`, and every change wakes waiters (`changed`).
extern pthread_mutex_t lock;
void changed();
}  // namespace handles
