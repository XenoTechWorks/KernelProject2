/* What event loops and servers lean on, checked the way an ordinary Linux program uses it (glibc,
 * built by gcc): eventfd, epoll, Unix sockets (pairs, files, abstract names, datagrams) and timers
 * (alarm, setitimer, poll timeouts). Prints one line per check and "events: N/N passed" at the end;
 * it passes on Linux too. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/file.h>
#include <sys/inotify.h>
#include <sys/pidfd.h>
#include <sys/signalfd.h>
#include <sys/ioctl.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <sys/timerfd.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static int passed, total;

static void check(const char *what, int ok) {
    total++;
    passed += ok;
    printf("  %s %s\n", ok ? "ok  " : "FAIL", what);
}

static double now(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec / 1e9;
}

/* How many entries epoll reports right now (without waiting). */
static int ready_now(int ep) {
    struct epoll_event ev[8];
    return epoll_wait(ep, ev, 8, 0);
}

static void add(int ep, int fd, uint32_t events) {
    struct epoll_event ev = {.events = events, .data.fd = fd};
    epoll_ctl(ep, EPOLL_CTL_ADD, fd, &ev);
}

static socklen_t unix_addr(struct sockaddr_un *a, const char *path, int abstract) {
    memset(a, 0, sizeof *a);
    a->sun_family = AF_UNIX;
    if (abstract) {
        memcpy(a->sun_path + 1, path, strlen(path));
        return offsetof(struct sockaddr_un, sun_path) + 1 + strlen(path);
    }
    strcpy(a->sun_path, path);
    return sizeof *a;
}

static volatile sig_atomic_t alarms;
static void on_alarm(int sig) { (void)sig; alarms++; }

static void eventfds(void) {
    uint64_t v = 3;
    int e = eventfd(0, EFD_NONBLOCK);
    write(e, &v, 8);
    v = 4;
    write(e, &v, 8);
    v = 0;
    read(e, &v, 8);
    check("eventfd adds up what's written, and reading takes it all", v == 7);
    check("an empty eventfd says try again (non-blocking)", read(e, &v, 8) < 0 && errno == EAGAIN);
    close(e);

    e = eventfd(2, EFD_NONBLOCK | EFD_SEMAPHORE);
    uint64_t a = 0, b = 0;
    read(e, &a, 8);
    read(e, &b, 8);
    check("a semaphore eventfd counts down one at a time", a == 1 && b == 1 && read(e, &v, 8) < 0);
    close(e);
}

static void epolls(void) {
    uint64_t one = 1, v;
    int e = eventfd(0, EFD_NONBLOCK);
    int ep = epoll_create1(EPOLL_CLOEXEC);
    add(ep, e, EPOLLIN);
    int before = ready_now(ep);
    write(e, &one, 8);
    int twice = ready_now(ep) == 1 && ready_now(ep) == 1;
    read(e, &v, 8);
    check("epoll (level-triggered) reports a ready eventfd for as long as it's ready", before == 0 && twice && ready_now(ep) == 0);

    struct epoll_event ev = {.events = EPOLLIN | EPOLLET, .data.u64 = 42};
    epoll_ctl(ep, EPOLL_CTL_MOD, e, &ev);
    write(e, &one, 8);
    struct epoll_event got;
    int first = epoll_wait(ep, &got, 1, 0);
    int again = ready_now(ep);
    write(e, &one, 8);
    int after_more = ready_now(ep);
    check("edge-triggered: reported once, then again only when more arrives", first == 1 && got.data.u64 == 42 && again == 0 && after_more == 1);

    ev.events = EPOLLIN | EPOLLONESHOT;
    epoll_ctl(ep, EPOLL_CTL_MOD, e, &ev);
    int once = ready_now(ep), off = ready_now(ep);
    epoll_ctl(ep, EPOLL_CTL_MOD, e, &ev);
    check("one-shot: reported once, until it's re-armed", once == 1 && off == 0 && ready_now(ep) == 1);
    read(e, &v, 8);

    /* Back to plain level-triggered (the one-shot entry is spent). */
    ev.events = EPOLLIN;
    epoll_ctl(ep, EPOLL_CTL_MOD, e, &ev);

    int file = open("/etc/hostname", O_RDONLY), p[2];
    pipe(p);
    int eperm = epoll_ctl(ep, EPOLL_CTL_ADD, file, &ev) < 0 && errno == EPERM;
    int eexist = epoll_ctl(ep, EPOLL_CTL_ADD, e, &ev) < 0 && errno == EEXIST;
    int enoent = epoll_ctl(ep, EPOLL_CTL_DEL, p[0], NULL) < 0 && errno == ENOENT;
    check("epoll refuses plain files, doubles, and removing what isn't there", eperm && eexist && enoent);
    close(file);
    close(p[0]);
    close(p[1]);

    double t = now();
    int n = epoll_wait(ep, &got, 1, 100);
    double waited = now() - t;
    check("epoll_wait times out when nothing happens", n == 0 && waited >= 0.09 && waited < 0.5);

    /* An epoll inside an epoll, and no loops. */
    int outer = epoll_create1(0);
    add(outer, ep, EPOLLIN);
    int quiet = ready_now(outer);
    write(e, &one, 8);
    int loud = ready_now(outer);
    ev.events = EPOLLIN;
    int loop = epoll_ctl(ep, EPOLL_CTL_ADD, outer, &ev) < 0 && errno == ELOOP;
    check("an epoll can watch an epoll, but not one that watches it back", quiet == 0 && loud == 1 && loop);
    read(e, &v, 8);

    /* Closing the file takes its entry away. */
    close(e);
    check("closing a watched descriptor removes it from epoll", ready_now(ep) == 0);
    close(outer);
    close(ep);
}

static void pairs(void) {
    int sv[2];
    char buf[16] = {0};
    socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, sv);
    write(sv[0], "ping", 4);
    int pending = 0;
    ioctl(sv[1], FIONREAD, &pending);
    ssize_t n = read(sv[1], buf, sizeof buf);
    check("socketpair: what one end writes, the other reads (FIONREAD says how much)", pending == 4 && n == 4 && !memcmp(buf, "ping", 4));
    write(sv[1], "pong", 4);
    n = read(sv[0], buf, sizeof buf);
    check("...both ways", n == 4 && !memcmp(buf, "pong", 4));

    shutdown(sv[0], SHUT_WR);
    check("shutdown(SHUT_WR): the other end reads end-of-file", read(sv[1], buf, sizeof buf) == 0);
    close(sv[0]);
    check("writing to a closed peer: EPIPE (with MSG_NOSIGNAL, no SIGPIPE)", send(sv[1], "x", 1, MSG_NOSIGNAL) < 0 && errno == EPIPE);
    close(sv[1]);

    socketpair(AF_UNIX, SOCK_DGRAM, 0, sv);
    send(sv[0], "ab", 2, 0);
    send(sv[0], "cde", 3, 0);
    ssize_t a = recv(sv[1], buf, sizeof buf, 0);
    ssize_t b = recv(sv[1], buf, sizeof buf, 0);
    check("datagram pairs keep each message whole", a == 2 && b == 3 && !memcmp(buf, "cde", 3));
    close(sv[0]);
    close(sv[1]);
}

/* Sends `data` with `fd` along (SCM_RIGHTS). */
static int send_fd(int sock, const char *data, int fd) {
    char control[CMSG_SPACE(sizeof(int))] = {0};
    struct iovec iov = {.iov_base = (void *)data, .iov_len = strlen(data)};
    struct msghdr m = {.msg_iov = &iov, .msg_iovlen = 1, .msg_control = control, .msg_controllen = sizeof control};
    struct cmsghdr *c = CMSG_FIRSTHDR(&m);
    c->cmsg_level = SOL_SOCKET;
    c->cmsg_type = SCM_RIGHTS;
    c->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(c), &fd, sizeof fd);
    return sendmsg(sock, &m, 0);
}

/* Receives into `buf`; the descriptor that came along, or -1. */
static int recv_fd(int sock, char *buf, int size, int *len) {
    char control[CMSG_SPACE(sizeof(int))] = {0};
    struct iovec iov = {.iov_base = buf, .iov_len = size};
    struct msghdr m = {.msg_iov = &iov, .msg_iovlen = 1, .msg_control = control, .msg_controllen = sizeof control};
    *len = recvmsg(sock, &m, MSG_CMSG_CLOEXEC);
    struct cmsghdr *c = CMSG_FIRSTHDR(&m);
    int fd = -1;
    if (c && c->cmsg_type == SCM_RIGHTS) memcpy(&fd, CMSG_DATA(c), sizeof fd);
    return fd;
}

static void passing(void) {
    int sv[2], p[2], len;
    char buf[16] = {0};
    socketpair(AF_UNIX, SOCK_STREAM, 0, sv);
    pipe(p);
    write(sv[0], "ab", 2);
    send_fd(sv[0], "cd", p[1]);
    write(sv[0], "ef", 2);
    close(p[1]);
    int got = recv_fd(sv[1], buf, sizeof buf, &len);
    int first = len == 4 && !memcmp(buf, "abcd", 4);
    int cloexec = fcntl(got, F_GETFD) == FD_CLOEXEC;
    write(got, "via", 3);
    close(got);
    int n = read(p[0], buf, sizeof buf);
    check("SCM_RIGHTS: a descriptor travels with its bytes, and works at the other end",
          first && got >= 0 && n == 3 && !memcmp(buf, "via", 3));
    int rest = recv_fd(sv[1], buf, sizeof buf, &len);
    check("...the read that brings it ends with its bytes, and MSG_CMSG_CLOEXEC marks it",
          cloexec && rest < 0 && len == 2 && !memcmp(buf, "ef", 2));
    close(p[0]);
    close(sv[0]);
    close(sv[1]);

    socketpair(AF_UNIX, SOCK_DGRAM, 0, sv);
    int e = eventfd(0, 0);
    send_fd(sv[0], "e", e);
    uint64_t v = 5;
    int other = recv_fd(sv[1], buf, sizeof buf, &len);
    write(other, &v, 8);
    v = 0;
    read(e, &v, 8);
    check("...on datagrams too: the same eventfd at both ends", other >= 0 && other != e && v == 5);
    close(e);
    close(other);
    close(sv[0]);
    close(sv[1]);
}

static void named(void) {
    const char *path = "/tmp/events-test.sock";
    struct sockaddr_un addr, got;
    socklen_t len = unix_addr(&addr, path, 0), got_len;
    unlink(path);

    int server = socket(AF_UNIX, SOCK_STREAM, 0);
    int bound = bind(server, (struct sockaddr *)&addr, len) == 0;
    struct stat st;
    check("bind makes a socket file", bound && stat(path, &st) == 0 && S_ISSOCK(st.st_mode));
    listen(server, 4);

    int other = socket(AF_UNIX, SOCK_STREAM, 0);
    check("binding a name that's taken: EADDRINUSE", bind(other, (struct sockaddr *)&addr, len) < 0 && errno == EADDRINUSE);
    close(other);

    int ep = epoll_create1(0);
    add(ep, server, EPOLLIN);
    int idle = ready_now(ep);

    pid_t child = fork();
    if (child == 0) {
        int c = socket(AF_UNIX, SOCK_STREAM, 0);
        char buf[16] = {0};
        if (connect(c, (struct sockaddr *)&addr, len) < 0) _exit(1);
        struct sockaddr_un peer;
        socklen_t peer_len = sizeof peer;
        getpeername(c, (struct sockaddr *)&peer, &peer_len);
        write(c, "hello", 5);
        if (read(c, buf, sizeof buf) != 5 || memcmp(buf, "world", 5)) _exit(2);
        _exit(strcmp(peer.sun_path, path) == 0 ? 0 : 3);
    }
    struct epoll_event ev;
    int waiting = epoll_wait(ep, &ev, 1, 2000);
    int conn = accept(server, NULL, NULL);
    struct ucred cred;
    socklen_t cred_len = sizeof cred;
    getsockopt(conn, SOL_SOCKET, SO_PEERCRED, &cred, &cred_len);
    char buf[16] = {0};
    ssize_t n = read(conn, buf, sizeof buf);
    write(conn, "world", 5);
    int status = -1;
    waitpid(child, &status, 0);
    check("epoll says when a connection is waiting", idle == 0 && waiting == 1);
    check("a forked client connects by path and they talk both ways", n == 5 && !memcmp(buf, "hello", 5) && WIFEXITED(status) && WEXITSTATUS(status) == 0);
    check("SO_PEERCRED names the client's process and user", cred.pid == child && cred.uid == getuid());
    got_len = sizeof got;
    getsockname(conn, (struct sockaddr *)&got, &got_len);
    check("getsockname gives the path", strcmp(got.sun_path, path) == 0);
    close(conn);
    close(ep);
    close(server);

    int c = socket(AF_UNIX, SOCK_STREAM, 0);
    int refused = connect(c, (struct sockaddr *)&addr, len) < 0 && errno == ECONNREFUSED;
    unlink(path);
    int missing = connect(c, (struct sockaddr *)&addr, len) < 0 && errno == ENOENT;
    close(c);
    check("connecting: refused once nobody listens, ENOENT once the file is gone", refused && missing);

    /* An abstract name: no file at all. */
    len = unix_addr(&addr, "events-test", 1);
    server = socket(AF_UNIX, SOCK_STREAM | SOCK_NONBLOCK, 0);
    bind(server, (struct sockaddr *)&addr, len);
    listen(server, 1);
    c = socket(AF_UNIX, SOCK_STREAM, 0);
    int ok = connect(c, (struct sockaddr *)&addr, len) == 0;
    conn = accept(server, NULL, NULL);
    write(c, "abc", 3);
    ok = ok && conn >= 0 && read(conn, buf, sizeof buf) == 3;
    check("an abstract name connects without a file", ok);
    close(c);
    close(conn);
    close(server);

    /* Datagrams to a bound name; the sender's name comes along. */
    struct sockaddr_un from_addr;
    socklen_t from_len = unix_addr(&from_addr, "/tmp/events-from.sock", 0);
    len = unix_addr(&addr, "/tmp/events-to.sock", 0);
    unlink(from_addr.sun_path);
    unlink(addr.sun_path);
    int to = socket(AF_UNIX, SOCK_DGRAM, 0), from = socket(AF_UNIX, SOCK_DGRAM, 0);
    bind(to, (struct sockaddr *)&addr, len);
    bind(from, (struct sockaddr *)&from_addr, from_len);
    sendto(from, "note", 4, 0, (struct sockaddr *)&addr, len);
    got_len = sizeof got;
    n = recvfrom(to, buf, sizeof buf, 0, (struct sockaddr *)&got, &got_len);
    check("a datagram to a path arrives with its sender's path", n == 4 && strcmp(got.sun_path, from_addr.sun_path) == 0);
    close(to);
    close(from);
    unlink(from_addr.sun_path);
    unlink(addr.sun_path);
}

static void signalfds(void) {
    sigset_t set;
    sigemptyset(&set);
    sigaddset(&set, SIGCHLD);
    sigaddset(&set, SIGUSR1);
    sigprocmask(SIG_BLOCK, &set, NULL);
    int sfd = signalfd(-1, &set, SFD_CLOEXEC);
    struct pollfd p = {.fd = sfd, .events = POLLIN};
    int quiet = poll(&p, 1, 0) == 0;
    pid_t child = fork();
    if (child == 0) _exit(3);
    int ready = poll(&p, 1, 2000) == 1;
    struct signalfd_siginfo si;
    int n = read(sfd, &si, sizeof si);
    waitpid(child, NULL, 0);
    check("signalfd: a child's exit can be read, with who and how (CLD_EXITED, status 3)",
          quiet && ready && n == sizeof si && si.ssi_signo == SIGCHLD && si.ssi_code == CLD_EXITED &&
          si.ssi_pid == (uint32_t)child && si.ssi_status == 3);

    kill(getpid(), SIGUSR1);
    siginfo_t info;
    int sig = sigwaitinfo(&set, &info);
    check("sigwaitinfo takes a blocked signal, saying who sent it", sig == SIGUSR1 && info.si_code == SI_USER && info.si_pid == getpid());
    close(sfd);
    sigprocmask(SIG_UNBLOCK, &set, NULL);
}

static void locks(void) {
    const char *path = "/tmp/events-lock";
    int a = open(path, O_RDWR | O_CREAT, 0644), b = open(path, O_RDWR);
    int took = flock(a, LOCK_EX) == 0;
    int blocked = flock(b, LOCK_EX | LOCK_NB) < 0 && errno == EWOULDBLOCK;
    int no_share = flock(b, LOCK_SH | LOCK_NB) < 0 && errno == EWOULDBLOCK;
    close(a);
    int freed = flock(b, LOCK_EX | LOCK_NB) == 0;
    check("flock: a file locked through one open can't be locked through another, until closed",
          took && blocked && no_share && freed);
    close(b);
    unlink(path);
}

static volatile sig_atomic_t usr1s;
static void on_usr1(int sig) { (void)sig; usr1s++; }

/* What an inotify descriptor has to say right now, as "MASK:name" items (names may be empty),
 * with the cookie of the last rename half seen. */
struct seen { int count; uint32_t mask[16]; char name[16][32]; uint32_t cookie[16]; };

static struct seen take_events(int fd) {
    struct seen s = {0};
    char buf[4096] __attribute__((aligned(8)));
    ssize_t n = read(fd, buf, sizeof buf);
    for (char *p = buf; n > 0 && p < buf + n && s.count < 16;) {
        struct inotify_event *e = (struct inotify_event *)p;
        s.mask[s.count] = e->mask;
        s.cookie[s.count] = e->cookie;
        snprintf(s.name[s.count], sizeof s.name[0], "%s", e->len ? e->name : "");
        s.count++;
        p += sizeof *e + e->len;
    }
    return s;
}

static int has(struct seen *s, uint32_t mask, const char *name) {
    for (int i = 0; i < s->count; i++)
        if (s->mask[i] == mask && !strcmp(s->name[i], name)) return i + 1;
    return 0;
}

static void watching(void) {
    const char *dir = "/tmp/notify-test";
    mkdir(dir, 0755);
    unlink("/tmp/notify-test/a");
    unlink("/tmp/notify-test/b");
    unlink("/tmp/notify-test/c");
    rmdir("/tmp/notify-test/sub");
    int in = inotify_init1(IN_NONBLOCK | IN_CLOEXEC);
    int wd = inotify_add_watch(in, dir, IN_CREATE | IN_DELETE | IN_MODIFY | IN_MOVED_FROM | IN_MOVED_TO | IN_CLOSE_WRITE);

    int ep = epoll_create1(0);
    add(ep, in, EPOLLIN);
    int quiet = ready_now(ep) == 0;
    int fd = open("/tmp/notify-test/a", O_WRONLY | O_CREAT, 0644);
    write(fd, "x", 1);
    close(fd);
    int woke = ready_now(ep) == 1;
    struct seen s = take_events(in);
    check("inotify: a folder's watch sees a file made, written and closed, by name (and wakes epoll)",
          quiet && woke && has(&s, IN_CREATE, "a") && has(&s, IN_MODIFY, "a") && has(&s, IN_CLOSE_WRITE, "a"));

    rename("/tmp/notify-test/a", "/tmp/notify-test/b");
    s = take_events(in);
    int from = has(&s, IN_MOVED_FROM, "a"), to = has(&s, IN_MOVED_TO, "b");
    check("...a rename as two halves with the same cookie", from && to && s.cookie[from - 1] && s.cookie[from - 1] == s.cookie[to - 1]);

    unlink("/tmp/notify-test/b");
    mkdir("/tmp/notify-test/sub", 0755);
    s = take_events(in);
    check("...a file removed, a folder made", has(&s, IN_DELETE, "b") && has(&s, IN_CREATE | IN_ISDIR, "sub"));

    /* A watch on one file: it hears about itself (no name), and its own deletion ends it. */
    fd = open("/tmp/notify-test/c", O_WRONLY | O_CREAT, 0644);
    take_events(in);
    int self = inotify_add_watch(in, "/tmp/notify-test/c", IN_MODIFY | IN_DELETE_SELF);
    write(fd, "y", 1);
    close(fd);
    unlink("/tmp/notify-test/c");
    s = take_events(in);
    int modified = 0, deleted = 0, ignored = 0;
    for (int i = 0; i < s.count; i++) {
        /* (the folder's watch also reports c: those carry its name) */
        if (s.name[i][0]) continue;
        modified |= s.mask[i] == IN_MODIFY;
        deleted |= s.mask[i] == IN_DELETE_SELF;
        ignored |= s.mask[i] == IN_IGNORED;
    }
    check("...a file's own watch: changes, then its deletion, which ends the watch", self > 0 && modified && deleted && ignored);

    check("...and removing a watch says so (IN_IGNORED)", inotify_rm_watch(in, wd) == 0 && take_events(in).mask[0] == IN_IGNORED);
    rmdir("/tmp/notify-test/sub");
    rmdir(dir);
    close(ep);
    close(in);
}

static void more_fds(void) {
    /* timerfd: first after 30 ms, then every 20 ms. */
    int t = timerfd_create(CLOCK_MONOTONIC, TFD_CLOEXEC);
    struct itimerspec spec = {.it_interval = {0, 20000000}, .it_value = {0, 30000000}}, now_spec;
    double start = now();
    timerfd_settime(t, 0, &spec, NULL);
    uint64_t count = 0;
    read(t, &count, 8);
    double first = now() - start;
    struct pollfd p = {.fd = t, .events = POLLIN};
    int later = poll(&p, 1, 500) == 1 && read(t, &count, 8) == 8;
    timerfd_gettime(t, &now_spec);
    check("timerfd goes off on time, then every so often, and can be watched with poll",
          first >= 0.025 && first < 0.3 && later && count >= 1 && now_spec.it_interval.tv_nsec == 20000000);
    close(t);

    /* pidfd: readable when the child ends; waitid takes it through the pidfd. */
    pid_t child = fork();
    if (child == 0) {
        usleep(100000);
        _exit(5);
    }
    int pfd = pidfd_open(child, 0);
    p = (struct pollfd){.fd = pfd, .events = POLLIN};
    int early = poll(&p, 1, 0) == 0;
    int ended = poll(&p, 1, 3000) == 1;
    siginfo_t info;
    memset(&info, 0, sizeof info);
    waitid(P_PIDFD, pfd, &info, WEXITED);
    check("pidfd: readable once its child ends, and waitid through it says how",
          early && ended && info.si_pid == child && info.si_code == CLD_EXITED && info.si_status == 5);
    close(pfd);

    child = fork();
    if (child == 0) {
        pause();
        _exit(0);
    }
    pfd = pidfd_open(child, 0);
    int sent = pidfd_send_signal(pfd, SIGTERM, NULL, 0) == 0;
    memset(&info, 0, sizeof info);
    int peeked = waitid(P_PID, child, &info, WEXITED | WNOWAIT) == 0 && info.si_code == CLD_KILLED && info.si_status == SIGTERM;
    int reaped = waitpid(child, NULL, 0) == child;
    check("pidfd_send_signal; waitid with WNOWAIT leaves the child to be waited for", sent && peeked && reaped);
    close(pfd);

    /* ppoll with a signal mask: a signal blocked outside it gets in during the wait. */
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = on_usr1;
    sigaction(SIGUSR1, &sa, NULL);
    sigset_t block, empty, after;
    sigemptyset(&block);
    sigaddset(&block, SIGUSR1);
    sigemptyset(&empty);
    sigprocmask(SIG_BLOCK, &block, NULL);
    raise(SIGUSR1);
    int pipe_fds[2];
    pipe(pipe_fds);
    p = (struct pollfd){.fd = pipe_fds[0], .events = POLLIN};
    struct timespec second = {1, 0};
    double t0 = now();
    int r = ppoll(&p, 1, &second, &empty);
    int interrupted = r < 0 && errno == EINTR && usr1s == 1 && now() - t0 < 0.5;
    sigprocmask(SIG_BLOCK, NULL, &after);
    check("ppoll's mask lets a blocked signal in while it waits, then the old mask is back",
          interrupted && sigismember(&after, SIGUSR1));
    sigprocmask(SIG_UNBLOCK, &block, NULL);
    close(pipe_fds[0]);
    close(pipe_fds[1]);
}

static void timers(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = on_alarm;
    sigaction(SIGALRM, &sa, NULL);

    double t = now();
    alarm(1);
    pause();
    double took = now() - t;
    check("alarm(1) interrupts pause() a second later", alarms == 1 && took >= 0.9 && took < 1.6);

    alarm(10);
    unsigned left = alarm(0);
    check("alarm returns the seconds the last one had left", left == 10 || left == 9);

    struct itimerval every = {.it_interval = {0, 20000}, .it_value = {0, 20000}}, read_back;
    alarms = 0;
    t = now();
    setitimer(ITIMER_REAL, &every, NULL);
    while (alarms < 5) pause();
    took = now() - t;
    getitimer(ITIMER_REAL, &read_back);
    struct itimerval off = {0};
    setitimer(ITIMER_REAL, &off, NULL);
    check("setitimer repeats every 20 ms", took >= 0.09 && took < 0.5 && read_back.it_interval.tv_usec == 20000);

    int p[2];
    pipe(p);
    struct pollfd fd = {.fd = p[0], .events = POLLIN};
    struct timespec wait = {0, 50 * 1000 * 1000};
    t = now();
    int n = ppoll(&fd, 1, &wait, NULL);
    took = now() - t;
    check("ppoll gives up after its timeout", n == 0 && took >= 0.04 && took < 0.5);
    close(p[0]);
    close(p[1]);
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    eventfds();
    epolls();
    pairs();
    passing();
    named();
    signalfds();
    locks();
    watching();
    more_fds();
    timers();
    printf("events: %d/%d passed\n", passed, total);
    return passed == total ? 0 : 1;
}
