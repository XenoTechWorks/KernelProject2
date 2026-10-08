/* Signal handling, checked the way an ordinary Linux program uses it (glibc, built by gcc).
 * Prints one line per check and "signals: N/N passed" at the end. */
#include <setjmp.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

static volatile sig_atomic_t got_usr1, got_chld, got_int;
static sigjmp_buf recover;
static int passed, total;

static void check(const char *what, int ok) {
    total++;
    passed += ok;
    printf("  %s %s\n", ok ? "ok  " : "FAIL", what);
}

static void on_usr1(int sig) { got_usr1 = sig; }
static void on_chld(int sig) { (void)sig; got_chld++; }
static void on_int(int sig) {
    /* Floating point inside a handler must not disturb the interrupted code. */
    volatile double x = 1.5;
    for (int i = 0; i < 10; i++) x *= 3.0;
    got_int = sig + (x > 0 ? 0 : 1000);
}
static void on_segv(int sig, siginfo_t *info, void *uc) {
    (void)uc;
    siglongjmp(recover, info->si_addr == (void *)16 ? sig : -1);
}

int main(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);

    sa.sa_handler = on_usr1;
    sigaction(SIGUSR1, &sa, NULL);
    raise(SIGUSR1);
    check("handler runs and the program continues", got_usr1 == SIGUSR1);

    sa.sa_handler = on_int;
    sigaction(SIGINT, &sa, NULL);
    volatile double before = 2.25;
    kill(getpid(), SIGINT);
    check("kill() to self runs the handler", got_int == SIGINT);
    check("floating point survives a handler", before * 2 == 4.5);

    sigset_t block, old;
    sigemptyset(&block);
    sigaddset(&block, SIGUSR1);
    got_usr1 = 0;
    sigprocmask(SIG_BLOCK, &block, &old);
    raise(SIGUSR1);
    sigset_t pending;
    sigpending(&pending);
    int waited = got_usr1 == 0 && sigismember(&pending, SIGUSR1);
    sigprocmask(SIG_SETMASK, &old, NULL);
    check("a blocked signal waits until it's unblocked", waited && got_usr1 == SIGUSR1);

    sa.sa_handler = on_chld;
    sigaction(SIGCHLD, &sa, NULL);
    pid_t child = fork();
    if (child == 0) _exit(7);
    int status = 0;
    while (waitpid(child, &status, 0) < 0) {}
    check("SIGCHLD arrives when a child exits", got_chld >= 1 && WEXITSTATUS(status) == 7);

    sa.sa_handler = SIG_IGN;
    sigaction(SIGUSR2, &sa, NULL);
    raise(SIGUSR2);
    check("an ignored signal does nothing", 1);

    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = on_segv;
    sa.sa_flags = SA_SIGINFO;
    sigaction(SIGSEGV, &sa, NULL);
    int r = sigsetjmp(recover, 1);
    if (r == 0) {
        *(volatile int *)16 = 1; /* fault on purpose */
        check("recover from a segfault", 0);
    } else {
        check("recover from a segfault (with the right fault address)", r == SIGSEGV);
    }

    printf("signals: %d/%d passed\n", passed, total);
    return passed == total ? 0 : 1;
}
