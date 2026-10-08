// kernel32.dll, threads and synchronization: CreateThread, waiting (WaitForSingleObject and
// WaitForMultipleObjects on threads, events and mutexes), events, mutexes and critical sections.
// Windows threads are Linux threads (pthreads); each gets its own TEB and TLS block first.

#include <cerrno>
#include <cstdlib>
#include <cstring>
#include <ctime>
#include <sys/syscall.h>
#include <unistd.h>
#include <vector>

#include "handles.hpp"

bool wait_for_change(const timespec *deadline);  // handles.cpp

namespace {
constexpr DWORD INFINITE = 0xFFFFFFFF;
constexpr DWORD WAIT_OBJECT_0 = 0;
constexpr DWORD WAIT_TIMEOUT = 0x102;
constexpr DWORD WAIT_FAILED = 0xFFFFFFFF;
constexpr DWORD CREATE_SUSPENDED = 4;

using ThreadStart = DWORD(WINAPI *)(void *);

struct StartInfo {
    ThreadObject *thread;
    ThreadStart start;
    void *parameter;
};

long current_tid() { return syscall(SYS_gettid); }

// Called with handles::lock held: thread ends, waiters wake.
void finish(ThreadObject *t, DWORD code) {
    t->exit_code = code;
    t->done = true;
    handles::changed();
}

thread_local ThreadObject *this_thread;

void *thread_main(void *arg) {
    StartInfo info = *static_cast<StartInfo *>(arg);
    delete static_cast<StartInfo *>(arg);
    pthread_mutex_lock(&handles::lock);
    info.thread->id = static_cast<DWORD>(current_tid());
    handles::changed();
    while (!info.thread->started) wait_for_change(nullptr);  // CREATE_SUSPENDED until ResumeThread
    pthread_mutex_unlock(&handles::lock);

    this_thread = info.thread;
    teb::attach_thread(teb::DLL_THREAD_ATTACH);
    DWORD code = info.start(info.parameter);
    pthread_mutex_lock(&handles::lock);
    finish(info.thread, code);
    pthread_mutex_unlock(&handles::lock);
    return nullptr;
}

// Can `o` be waited on right now? Takes it (mutex, auto-reset event) when `take` is set.
// Called with handles::lock held.
bool ready(Object *o, bool take) {
    switch (o->kind) {
        case Object::Kind::Thread: return static_cast<ThreadObject *>(o)->done;
        case Object::Kind::Event: {
            auto *e = static_cast<EventObject *>(o);
            if (!e->signaled) return false;
            if (take && !e->manual_reset) e->signaled = false;
            return true;
        }
        case Object::Kind::Mutex: {
            auto *m = static_cast<MutexObject *>(o);
            long me = current_tid();
            if (m->owner != 0 && m->owner != me) return false;
            if (take) {
                m->owner = me;
                m->recursion++;
            }
            return true;
        }
        default: return true;
    }
}

timespec deadline_after(DWORD ms) {
    timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    t.tv_sec += ms / 1000;
    t.tv_nsec += static_cast<long>(ms % 1000) * 1000000;
    if (t.tv_nsec >= 1000000000) t.tv_sec++, t.tv_nsec -= 1000000000;
    return t;
}

DWORD wait_for(DWORD count, const HANDLE *list, bool all, DWORD ms) {
    std::vector<Object *> objects;
    for (DWORD i = 0; i < count; i++) {
        Object *o = handles::get(list[i]);
        if (!o) {
            set_last_error(ERROR_INVALID_HANDLE);
            return WAIT_FAILED;
        }
        objects.push_back(o);
    }
    timespec deadline = deadline_after(ms == INFINITE ? 0 : ms);
    pthread_mutex_lock(&handles::lock);
    DWORD result = WAIT_TIMEOUT;
    for (;;) {
        if (all) {
            bool every = true;
            for (Object *o : objects) every = every && ready(o, false);
            if (every) {
                for (Object *o : objects) ready(o, true);
                result = WAIT_OBJECT_0;
                break;
            }
        } else {
            bool found = false;
            for (DWORD i = 0; i < count && !found; i++)
                if (ready(objects[i], true)) result = WAIT_OBJECT_0 + i, found = true;
            if (found) break;
        }
        if (ms == 0) break;
        if (!wait_for_change(ms == INFINITE ? nullptr : &deadline) && ms != INFINITE) {
            timespec now;
            clock_gettime(CLOCK_MONOTONIC, &now);
            if (now.tv_sec > deadline.tv_sec || (now.tv_sec == deadline.tv_sec && now.tv_nsec >= deadline.tv_nsec)) {
                result = WAIT_TIMEOUT;
                // One last look, so a change that raced with the timeout isn't missed.
                if (!all)
                    for (DWORD i = 0; i < count; i++)
                        if (ready(objects[i], true)) {
                            result = WAIT_OBJECT_0 + i;
                            break;
                        }
                break;
            }
        }
    }
    pthread_mutex_unlock(&handles::lock);
    return result;
}

// Critical sections: the Windows structure is 40 bytes; a recursive pthread mutex is kept aside
// and its address stored in the LockSemaphore field.
struct CriticalSection {
    void *debug_info;
    LONG lock_count;
    LONG recursion_count;
    HANDLE owning_thread;
    pthread_mutex_t *mutex;
    uintptr_t spin_count;
};

pthread_mutex_t *mutex_of(CriticalSection *cs) {
    if (!cs->mutex) {  // used without InitializeCriticalSection; be forgiving
        static pthread_mutex_t guard = PTHREAD_MUTEX_INITIALIZER;
        pthread_mutex_lock(&guard);
        if (!cs->mutex) {
            auto *m = static_cast<pthread_mutex_t *>(malloc(sizeof(pthread_mutex_t)));
            pthread_mutexattr_t attr;
            pthread_mutexattr_init(&attr);
            pthread_mutexattr_settype(&attr, PTHREAD_MUTEX_RECURSIVE);
            pthread_mutex_init(m, &attr);
            cs->mutex = m;
        }
        pthread_mutex_unlock(&guard);
    }
    return cs->mutex;
}
}  // namespace

extern "C" {

WINAPI HANDLE k32_CreateThread(void *, SIZE_T stack_size, ThreadStart start, void *parameter, DWORD flags, DWORD *id) {
    auto *t = new ThreadObject();
    t->started = !(flags & CREATE_SUSPENDED);
    pthread_attr_t attr;
    pthread_attr_init(&attr);
    pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED);
    if (stack_size) pthread_attr_setstacksize(&attr, stack_size < 65536 ? 65536 : stack_size);
    if (pthread_create(&t->thread, &attr, thread_main, new StartInfo{t, start, parameter}) != 0) {
        delete t;
        set_last_error(ERROR_NOT_ENOUGH_MEMORY);
        return nullptr;
    }
    // Wait until the new thread knows its id, so we can report it.
    pthread_mutex_lock(&handles::lock);
    while (t->id == 0) wait_for_change(nullptr);
    pthread_mutex_unlock(&handles::lock);
    if (id) *id = t->id;
    return handles::add(t);
}

WINAPI DWORD k32_ResumeThread(HANDLE h) {
    auto *t = handles::get_as<ThreadObject>(h, Object::Kind::Thread);
    if (!t) return set_last_error(ERROR_INVALID_HANDLE), 0xFFFFFFFF;
    pthread_mutex_lock(&handles::lock);
    DWORD was_suspended = t->started ? 0 : 1;
    t->started = true;
    handles::changed();
    pthread_mutex_unlock(&handles::lock);
    return was_suspended;
}

WINAPI void k32_ExitThread(DWORD code) {
    if (!this_thread) process::exit(static_cast<int>(code));  // the main thread
    pthread_mutex_lock(&handles::lock);
    finish(this_thread, code);
    pthread_mutex_unlock(&handles::lock);
    pthread_exit(nullptr);
}

WINAPI BOOL k32_GetExitCodeThread(HANDLE h, DWORD *code) {
    auto *t = handles::get_as<ThreadObject>(h, Object::Kind::Thread);
    if (!t) return set_last_error(ERROR_INVALID_HANDLE), 0;
    pthread_mutex_lock(&handles::lock);
    *code = t->exit_code;
    pthread_mutex_unlock(&handles::lock);
    return 1;
}

WINAPI HANDLE k32_GetCurrentThread() { return reinterpret_cast<HANDLE>(-2); }

WINAPI DWORD k32_WaitForMultipleObjects(DWORD count, const HANDLE *list, BOOL all, DWORD ms) {
    return wait_for(count, list, all, ms);
}
WINAPI DWORD k32_WaitForSingleObject(HANDLE h, DWORD ms) { return wait_for(1, &h, false, ms); }
WINAPI DWORD k32_WaitForSingleObjectEx(HANDLE h, DWORD ms, BOOL) { return wait_for(1, &h, false, ms); }

WINAPI HANDLE k32_CreateEventA(void *, BOOL manual_reset, BOOL initial, LPCSTR) {
    return handles::add(new EventObject(manual_reset, initial));
}
WINAPI HANDLE k32_CreateEventW(void *, BOOL manual_reset, BOOL initial, LPCWSTR) {
    return handles::add(new EventObject(manual_reset, initial));
}
static BOOL set_event_state(HANDLE h, bool state) {
    auto *e = handles::get_as<EventObject>(h, Object::Kind::Event);
    if (!e) return set_last_error(ERROR_INVALID_HANDLE), 0;
    pthread_mutex_lock(&handles::lock);
    e->signaled = state;
    handles::changed();
    pthread_mutex_unlock(&handles::lock);
    return 1;
}
WINAPI BOOL k32_SetEvent(HANDLE h) { return set_event_state(h, true); }
WINAPI BOOL k32_ResetEvent(HANDLE h) { return set_event_state(h, false); }

WINAPI HANDLE k32_CreateMutexA(void *, BOOL initial_owner, LPCSTR) {
    auto *m = new MutexObject();
    if (initial_owner) m->owner = current_tid(), m->recursion = 1;
    return handles::add(m);
}
WINAPI HANDLE k32_CreateMutexW(void *, BOOL initial_owner, LPCWSTR) { return k32_CreateMutexA(nullptr, initial_owner, nullptr); }
WINAPI BOOL k32_ReleaseMutex(HANDLE h) {
    auto *m = handles::get_as<MutexObject>(h, Object::Kind::Mutex);
    if (!m) return set_last_error(ERROR_INVALID_HANDLE), 0;
    pthread_mutex_lock(&handles::lock);
    bool mine = m->owner == current_tid();
    if (mine && --m->recursion == 0) {
        m->owner = 0;
        handles::changed();
    }
    pthread_mutex_unlock(&handles::lock);
    if (!mine) set_last_error(288);  // ERROR_NOT_OWNER
    return mine;
}

WINAPI void k32_InitializeCriticalSection(CriticalSection *cs) {
    memset(cs, 0, sizeof *cs);
    cs->lock_count = -1;
    mutex_of(cs);
}
WINAPI BOOL k32_InitializeCriticalSectionAndSpinCount(CriticalSection *cs, DWORD) {
    k32_InitializeCriticalSection(cs);
    return 1;
}
WINAPI void k32_DeleteCriticalSection(CriticalSection *cs) {
    if (cs->mutex) {
        pthread_mutex_destroy(cs->mutex);
        free(cs->mutex);
        cs->mutex = nullptr;
    }
}
WINAPI void k32_EnterCriticalSection(CriticalSection *cs) {
    pthread_mutex_lock(mutex_of(cs));
    cs->recursion_count++;
    cs->owning_thread = reinterpret_cast<HANDLE>(current_tid());
}
WINAPI BOOL k32_TryEnterCriticalSection(CriticalSection *cs) {
    if (pthread_mutex_trylock(mutex_of(cs)) != 0) return 0;
    cs->recursion_count++;
    cs->owning_thread = reinterpret_cast<HANDLE>(current_tid());
    return 1;
}
WINAPI void k32_LeaveCriticalSection(CriticalSection *cs) {
    if (--cs->recursion_count == 0) cs->owning_thread = nullptr;
    pthread_mutex_unlock(mutex_of(cs));
}

}  // extern "C"

#define EXPORT(name) {#name, reinterpret_cast<void *>(&k32_##name)}

namespace {
const Export exports[] = {
    EXPORT(CreateEventA), EXPORT(CreateEventW), EXPORT(CreateMutexA), EXPORT(CreateMutexW), EXPORT(CreateThread),
    EXPORT(DeleteCriticalSection), EXPORT(EnterCriticalSection), EXPORT(ExitThread), EXPORT(GetCurrentThread),
    EXPORT(GetExitCodeThread), EXPORT(InitializeCriticalSection), EXPORT(InitializeCriticalSectionAndSpinCount),
    EXPORT(LeaveCriticalSection), EXPORT(ReleaseMutex), EXPORT(ResetEvent), EXPORT(ResumeThread), EXPORT(SetEvent),
    EXPORT(TryEnterCriticalSection), EXPORT(WaitForMultipleObjects), EXPORT(WaitForSingleObject),
    EXPORT(WaitForSingleObjectEx),
};
}  // namespace

const BuiltinDll kernel32_threads_dll{"kernel32.dll", exports, sizeof exports / sizeof exports[0]};
