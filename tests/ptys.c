/* Pseudo-terminals, checked the way an ordinary Linux program uses them (glibc, built by gcc): the
 * outside and inside talking through the line discipline, raw mode, a controlling terminal with
 * Ctrl+C and window sizes, hanging up, and forkpty. Prints one line per check and
 * "ptys: N/N passed" at the end; it passes on Linux too. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pty.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <termios.h>
#include <unistd.h>

static int passed, total;

static void check(const char *what, int ok) {
    total++;
    passed += ok;
    printf("  %s %s\n", ok ? "ok  " : "FAIL", what);
}

/* Reads what's there within half a second (more than one read if it comes in pieces). */
static int read_some(int fd, char *buf, int size) {
    int n = 0;
    struct pollfd p = {.fd = fd, .events = POLLIN};
    while (n < size - 1 && poll(&p, 1, n ? 50 : 500) > 0) {
        int k = read(fd, buf + n, size - 1 - n);
        if (k <= 0) break;
        n += k;
    }
    buf[n] = 0;
    return n;
}

static int new_pty(char *name) {
    int master = posix_openpt(O_RDWR | O_NOCTTY);
    grantpt(master);
    unlockpt(master);
    strcpy(name, ptsname(master));
    return master;
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    char name[64], buf[256];

    /* Locked until unlockpt. */
    int master = posix_openpt(O_RDWR | O_NOCTTY);
    char *locked_name = ptsname(master);
    int early = open(locked_name, O_RDWR | O_NOCTTY);
    check("the inside can't be opened before unlockpt", early < 0 && errno == EIO);
    unlockpt(master);
    int inside = open(locked_name, O_RDWR | O_NOCTTY);
    struct stat st;
    fstat(inside, &st);
    check("ptsname names /dev/pts/N, a character device that's ours", !strncmp(locked_name, "/dev/pts/", 9) && S_ISCHR(st.st_mode) && st.st_uid == getuid());
    check("isatty, and ttyname gives the same name", isatty(inside) && !strcmp(ttyname(inside), locked_name));

    /* Typed into the outside: a line arrives inside, and the echo comes back out. */
    write(master, "hello\n", 6);
    int n = read(inside, buf, sizeof buf);
    check("what the outside types, the inside reads as a line", n == 6 && !memcmp(buf, "hello\n", 6));
    read_some(master, buf, sizeof buf);
    check("...and the outside sees it echoed (newline as CR LF)", !strcmp(buf, "hello\r\n"));
    write(inside, "out\n", 4);
    read_some(master, buf, sizeof buf);
    check("what the inside writes, the outside reads", !strcmp(buf, "out\r\n"));

    write(master, "abc\x7f\x7f" "d\n", 7);
    n = read(inside, buf, sizeof buf);
    check("backspace edits the line before the inside sees it", n == 3 && !memcmp(buf, "ad\n", 3));
    read_some(master, buf, sizeof buf);

    struct termios raw;
    tcgetattr(inside, &raw);
    cfmakeraw(&raw);
    tcsetattr(inside, TCSANOW, &raw);
    write(master, "x", 1);
    n = read(inside, buf, sizeof buf);
    int echoed = read_some(master, buf + 10, 100);
    check("raw mode: each key at once, and no echo", n == 1 && buf[0] == 'x' && echoed == 0);
    close(inside);
    close(master);

    /* A session with this terminal in control: Ctrl+C and the window size reach it. */
    master = new_pty(name);
    struct winsize ws = {.ws_row = 30, .ws_col = 100};
    int sync[2];
    pipe(sync);
    pid_t child = fork();
    if (child == 0) {
        close(master);
        setsid();
        int fd = open(name, O_RDWR); /* becomes the controlling terminal */
        int ok = tcgetpgrp(fd) == getpid() && tcgetsid(fd) == getpid();
        int tty = open("/dev/tty", O_RDWR);
        ok = ok && tty >= 0;
        sigset_t winch;
        sigemptyset(&winch);
        sigaddset(&winch, SIGWINCH);
        sigprocmask(SIG_BLOCK, &winch, NULL);
        write(sync[1], ok ? "y" : "n", 1);
        int sig = 0;
        sigwait(&winch, &sig);
        struct winsize got;
        ioctl(fd, TIOCGWINSZ, &got);
        write(sync[1], got.ws_row == 30 && got.ws_col == 100 ? "y" : "n", 1);
        pause(); /* until Ctrl+C */
        _exit(0);
    }
    char ok[2] = {0};
    read(sync[0], ok, 1);
    check("opening it after setsid makes it the controlling terminal (/dev/tty too)", ok[0] == 'y');
    ioctl(master, TIOCSWINSZ, &ws);
    read(sync[0], ok, 1);
    check("a new window size: SIGWINCH, and TIOCGWINSZ says it", ok[0] == 'y');
    write(master, "\x03", 1);
    int status;
    waitpid(child, &status, 0);
    check("Ctrl+C typed into it interrupts its foreground", WIFSIGNALED(status) && WTERMSIG(status) == SIGINT);
    close(master);

    /* Hanging up: the outside closes. */
    master = new_pty(name);
    child = fork();
    if (child == 0) {
        close(master);
        setsid();
        int fd = open(name, O_RDWR);
        signal(SIGHUP, SIG_IGN);
        write(sync[1], "r", 1);
        char c;
        int got = read(fd, &c, 1);
        int read_errno = errno;
        int wrote = write(fd, "x", 1);
        _exit(got < 0 && read_errno == EIO && wrote < 0 && errno == EIO ? 0 : 1);
    }
    read(sync[0], ok, 1);
    close(master);
    waitpid(child, &status, 0);
    check("when the outside closes, the inside can't read or write any more (EIO)", WIFEXITED(status) && WEXITSTATUS(status) == 0);

    master = new_pty(name);
    inside = open(name, O_RDWR | O_NOCTTY);
    close(inside);
    check("when the inside closes, the outside reads EIO", read(master, buf, 1) < 0 && errno == EIO);
    close(master);

    /* Opening and closing /dev/tty (as sshd does, to check) leaves the terminal working. */
    master = new_pty(name);
    child = fork();
    if (child == 0) {
        close(master);
        setsid();
        int fd = open(name, O_RDWR);
        close(open("/dev/tty", O_RDWR));
        write(fd, "still here\n", 11);
        read(sync[0], ok, 1); /* until the parent has read it */
        _exit(0);
    }
    n = read_some(master, buf, sizeof buf);
    write(sync[1], "k", 1);
    waitpid(child, &status, 0);
    check("opening and closing /dev/tty inside doesn't cut the terminal off", n > 0 && strstr(buf, "still here"));
    close(master);

    /* forkpty: the usual way to run a program in a terminal. */
    child = forkpty(&master, NULL, NULL, &ws);
    if (child == 0) {
        struct winsize got;
        ioctl(0, TIOCGWINSZ, &got);
        printf("child on %s, %dx%d\n", ttyname(0) ? "a tty" : "no tty", got.ws_col, got.ws_row);
        _exit(7);
    }
    n = 0;
    for (;;) {
        int k = read(master, buf + n, sizeof buf - 1 - n);
        if (k <= 0) break;
        n += k;
    }
    buf[n] = 0;
    waitpid(child, &status, 0);
    check("forkpty runs a child on a terminal of the size asked for", strstr(buf, "child on a tty, 100x30\r\n") && WEXITSTATUS(status) == 7);
    close(master);

    printf("ptys: %d/%d passed\n", passed, total);
    return passed == total ? 0 : 1;
}
