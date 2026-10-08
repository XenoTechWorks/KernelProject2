/* Threads and synchronization through the Windows API, built with mingw-w64.
 * Prints one line per check and "winthreads: N/N passed" at the end. */
#include <stdio.h>
#include <windows.h>

static int passed, total;
static void check(const char *what, int ok) {
    total++;
    passed += ok;
    printf("  %s %s\n", ok ? "ok  " : "FAIL", what);
}

enum { THREADS = 4, ROUNDS = 20000 };
static CRITICAL_SECTION lock;
static volatile LONG counter, interlocked;
static HANDLE go;
static __thread int mine;
static DWORD thread_ids[THREADS];

static DWORD WINAPI work(void *arg) {
    int n = (int)(INT_PTR)arg;
    mine = n + 100;
    thread_ids[n] = GetCurrentThreadId();
    WaitForSingleObject(go, INFINITE);
    for (int i = 0; i < ROUNDS; i++) {
        EnterCriticalSection(&lock);
        counter++;
        LeaveCriticalSection(&lock);
        InterlockedIncrement(&interlocked);
    }
    return (DWORD)mine;
}

int main(void) {
    InitializeCriticalSection(&lock);
    go = CreateEventA(NULL, TRUE, FALSE, NULL);
    mine = 1;
    HANDLE t[THREADS];
    for (int i = 0; i < THREADS; i++) t[i] = CreateThread(NULL, 0, work, (void *)(INT_PTR)i, 0, NULL);
    check("CreateThread", t[0] && t[THREADS - 1]);
    check("an unset event times out", WaitForSingleObject(go, 10) == WAIT_TIMEOUT);
    SetEvent(go);

    check("WaitForMultipleObjects (all threads finish)", WaitForMultipleObjects(THREADS, t, TRUE, INFINITE) < WAIT_OBJECT_0 + THREADS);
    int codes_ok = 1;
    for (int i = 0; i < THREADS; i++) {
        DWORD code = 0;
        GetExitCodeThread(t[i], &code);
        codes_ok &= code == (DWORD)(i + 100);
        CloseHandle(t[i]);
    }
    check("GetExitCodeThread returns each thread's result", codes_ok);
    check("a critical section keeps the counter exact", counter == THREADS * ROUNDS);
    check("InterlockedIncrement", interlocked == THREADS * ROUNDS);
    check("thread-local variables stay separate", mine == 1);
    int distinct = 1;
    for (int i = 0; i < THREADS; i++)
        for (int j = 0; j < i; j++) distinct &= thread_ids[i] != thread_ids[j];
    check("each thread has its own id", distinct && thread_ids[0] != GetCurrentThreadId());

    HANDLE m = CreateMutexA(NULL, FALSE, NULL);
    check("a mutex can be taken and released", WaitForSingleObject(m, 0) == WAIT_OBJECT_0 && ReleaseMutex(m));
    CloseHandle(m);
    DeleteCriticalSection(&lock);

    printf("winthreads: %d/%d passed\n", passed, total);
    return passed == total ? 0 : 1;
}
