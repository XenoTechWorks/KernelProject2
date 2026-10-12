/* ptrace, checked the way strace and debuggers use it (glibc, built by gcc): a traced child
 * stopping at its system calls, its registers and memory read and changed, a signal held back,
 * the exec and fork events, and attaching to a running process. Prints one line per check and
 * "trace: N/N passed" at the end; it passes on Linux too. */
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ptrace.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <sys/user.h>
#include <sys/wait.h>
#include <unistd.h>

static int passed, total;

static void check(const char *what, int ok) {
    total++;
    passed += ok;
    printf("  %s %s\n", ok ? "ok  " : "FAIL", what);
}

static char secret[32] = "read across processes";

/* Lets the tracee go on until it stops for something other than a signal (passing signals on,
 * as strace does); that stop's status, or how it ended. */
static int next_event(pid_t pid) {
    int status;
    ptrace(PTRACE_CONT, pid, 0, 0);
    for (;;) {
        waitpid(pid, &status, 0);
        if (!WIFSTOPPED(status) || status >> 16 != 0) return status;
        ptrace(PTRACE_CONT, pid, 0, WSTOPSIG(status));
    }
}

/* Lets the tracee go on to its next system call stop; the stop's status. */
static int next_syscall(pid_t pid) {
    int status;
    ptrace(PTRACE_SYSCALL, pid, 0, 0);
    waitpid(pid, &status, 0);
    return status;
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);

    /* 1. TRACEME: the child stops at its own SIGSTOP; then each system call twice. */
    pid_t child = fork();
    if (child == 0) {
        ptrace(PTRACE_TRACEME, 0, 0, 0);
        raise(SIGSTOP);
        syscall(SYS_getppid); /* the tracer changes this one into getpid */
        syscall(SYS_write, 1, "", 0);
        _exit(3);
    }
    int status;
    waitpid(child, &status, 0);
    check("a child that asks to be traced stops at its first signal", WIFSTOPPED(status) && WSTOPSIG(status) == SIGSTOP);
    ptrace(PTRACE_SETOPTIONS, child, 0, PTRACE_O_TRACESYSGOOD);

    /* Its memory: PEEKDATA and process_vm_readv see the same bytes. */
    errno = 0;
    long word = ptrace(PTRACE_PEEKDATA, child, secret, 0);
    char copy[32] = {0};
    struct iovec local = {copy, sizeof secret}, remote = {secret, sizeof secret};
    ssize_t got = process_vm_readv(child, &local, 1, &remote, 1, 0);
    check("its memory can be read (PEEKDATA, process_vm_readv)",
          errno == 0 && !memcmp(&word, secret, sizeof word) && got == sizeof secret && !strcmp(copy, secret));

    /* Going into getppid: change it into getpid; coming out, it returns the child's own id. */
    status = next_syscall(child);
    struct user_regs_struct regs;
    ptrace(PTRACE_GETREGS, child, 0, &regs);
    int entry = WIFSTOPPED(status) && WSTOPSIG(status) == (SIGTRAP | 0x80) && regs.orig_rax == SYS_getppid;
    regs.orig_rax = SYS_getpid;
    ptrace(PTRACE_SETREGS, child, 0, &regs);
    status = next_syscall(child);
    ptrace(PTRACE_GETREGS, child, 0, &regs);
    check("system calls stop going in, with their number (TRACESYSGOOD)", entry);
    check("...and the tracer can change the call: coming out, its result",
          WSTOPSIG(status) == (SIGTRAP | 0x80) && (pid_t)regs.rax == child);
    status = next_syscall(child); /* write in */
    status = next_syscall(child); /* write out */
    status = next_syscall(child); /* exit_group in */
    ptrace(PTRACE_CONT, child, 0, 0);
    waitpid(child, &status, 0);
    check("it runs to the end, and its exit is seen", WIFEXITED(status) && WEXITSTATUS(status) == 3);

    /* 2. A signal stops on its way, and the tracer drops it. */
    child = fork();
    if (child == 0) {
        ptrace(PTRACE_TRACEME, 0, 0, 0);
        raise(SIGUSR1); /* would end it, but the tracer drops it */
        _exit(4);
    }
    waitpid(child, &status, 0);
    siginfo_t info;
    ptrace(PTRACE_GETSIGINFO, child, 0, &info);
    int seen = WIFSTOPPED(status) && WSTOPSIG(status) == SIGUSR1 && info.si_signo == SIGUSR1;
    ptrace(PTRACE_CONT, child, 0, 0); /* with no signal: dropped */
    waitpid(child, &status, 0);
    check("a signal stops on its way to the tracee; the tracer can drop it", seen && WIFEXITED(status) && WEXITSTATUS(status) == 4);

    /* 3. Events: exec and fork, with the new child followed. */
    child = fork();
    if (child == 0) {
        ptrace(PTRACE_TRACEME, 0, 0, 0);
        raise(SIGSTOP);
        pid_t grandchild = fork();
        if (grandchild == 0) _exit(0);
        waitpid(grandchild, NULL, 0);
        execl("/bin/cat", "cat", "/dev/null", (char *)NULL);
        _exit(9);
    }
    waitpid(child, &status, 0);
    ptrace(PTRACE_SETOPTIONS, child, 0, PTRACE_O_TRACEFORK | PTRACE_O_TRACEEXEC);
    ptrace(PTRACE_CONT, child, 0, 0);
    waitpid(child, &status, 0);
    unsigned long new_pid = 0;
    ptrace(PTRACE_GETEVENTMSG, child, 0, &new_pid);
    int forked = WIFSTOPPED(status) && status >> 8 == (SIGTRAP | PTRACE_EVENT_FORK << 8) && new_pid > 0;
    int child_status;
    waitpid((pid_t)new_pid, &child_status, __WALL);
    int follows = WIFSTOPPED(child_status);
    /* The traced grandchild's end comes to the tracer first: only then can its parent collect it. */
    ptrace(PTRACE_CONT, (pid_t)new_pid, 0, 0);
    waitpid((pid_t)new_pid, &child_status, __WALL);
    status = next_event(child);
    int execed = WIFSTOPPED(status) && status >> 8 == (SIGTRAP | PTRACE_EVENT_EXEC << 8);
    status = next_event(child);
    check("fork is an event, and the new child is traced from its start", forked && follows);
    check("exec is an event", execed && WIFEXITED(status) && WEXITSTATUS(status) == 0);

    /* 4. Attaching to a running process: SEIZE, then INTERRUPT to stop it. */
    int ready[2];
    pipe(ready);
    child = fork();
    if (child == 0) {
        close(ready[0]);
        write(ready[1], "r", 1);
        for (;;) pause();
    }
    char c;
    read(ready[0], &c, 1);
    int seized = ptrace(PTRACE_SEIZE, child, 0, 0) == 0;
    ptrace(PTRACE_INTERRUPT, child, 0, 0);
    waitpid(child, &status, 0);
    int stopped = WIFSTOPPED(status) && status >> 16 == PTRACE_EVENT_STOP;
    ptrace(PTRACE_DETACH, child, 0, 0);
    kill(child, SIGKILL);
    waitpid(child, &status, 0);
    check("SEIZE attaches to a running process, INTERRUPT stops it", seized && stopped && WIFSIGNALED(status));

    errno = 0;
    check("nobody else's tracee: ESRCH", ptrace(PTRACE_CONT, getppid(), 0, 0) < 0 && errno == ESRCH);

    printf("trace: %d/%d passed\n", passed, total);
    return passed == total ? 0 : 1;
}
