/* Threads, checked the way an ordinary Linux program uses them (glibc pthreads, built by gcc).
 * Prints one line per check and "threads: N/N passed" at the end. */
#define _GNU_SOURCE
#include <pthread.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <unistd.h>

static int passed, total;
static void check(const char *what, int ok) {
    total++;
    passed += ok;
    printf("  %s %s\n", ok ? "ok  " : "FAIL", what);
}

enum { THREADS = 4, ROUNDS = 20000 };
static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t ready = PTHREAD_COND_INITIALIZER;
static long counter;
static int started;
static __thread int mine;
static pid_t pids[THREADS], tids[THREADS];

static void *work(void *arg) {
    long n = (long)arg;
    mine = (int)n + 100;
    pids[n] = getpid();
    tids[n] = (pid_t)syscall(SYS_gettid);
    pthread_mutex_lock(&lock);
    started++;
    pthread_cond_signal(&ready);
    pthread_mutex_unlock(&lock);
    for (int i = 0; i < ROUNDS; i++) {
        pthread_mutex_lock(&lock);
        counter++;
        pthread_mutex_unlock(&lock);
        if (i % 1000 == 0) sched_yield();
    }
    return (void *)(long)(mine * 2);
}

int main(void) {
    pthread_t t[THREADS];
    mine = 1;
    for (long i = 0; i < THREADS; i++) pthread_create(&t[i], NULL, work, (void *)i);

    pthread_mutex_lock(&lock);
    while (started < THREADS) pthread_cond_wait(&ready, &lock);
    pthread_mutex_unlock(&lock);
    check("condition variable: all threads started", started == THREADS);

    int results_ok = 1;
    for (long i = 0; i < THREADS; i++) {
        void *r;
        pthread_join(t[i], &r);
        results_ok &= (long)r == (i + 100) * 2;
    }
    check("pthread_join returns each thread's result", results_ok);
    check("a mutex keeps the shared counter exact", counter == (long)THREADS * ROUNDS);
    check("thread-local variables stay separate", mine == 1);

    int same_pid = 1, distinct_tids = 1;
    for (int i = 0; i < THREADS; i++) {
        same_pid &= pids[i] == getpid();
        distinct_tids &= tids[i] != getpid();
        for (int j = 0; j < i; j++) distinct_tids &= tids[i] != tids[j];
    }
    check("threads share the process id", same_pid);
    check("each thread has its own thread id", distinct_tids);

    printf("threads: %d/%d passed\n", passed, total);
    return passed == total ? 0 : 1;
}
