/* chroot, checked the way an ordinary Linux program uses it (glibc, built by gcc): a child shut in
 * a folder sees it as "/", can't climb out with "..", follows absolute links inside it, and can't
 * reach what's outside; and only root may do it. Prints one line per check and "jail: N/N passed"
 * at the end; it passes on Linux too (as root). */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

static int passed, total;

static void check(const char *what, int ok) {
    total++;
    passed += ok;
    printf("  %s %s\n", ok ? "ok  " : "FAIL", what);
}

/* Runs `test` in a child shut in /tmp/jail; its exit status is a bit per failed part. */
static int in_jail(int (*test)(void)) {
    pid_t child = fork();
    if (child == 0) {
        if (chroot("/tmp/jail") != 0 || chdir("/") != 0) _exit(255);
        _exit(test());
    }
    int status;
    waitpid(child, &status, 0);
    return WIFEXITED(status) ? WEXITSTATUS(status) : 254;
}

static int read_file(const char *path, char *buf, int size) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) return -1;
    int n = read(fd, buf, size - 1);
    close(fd);
    buf[n > 0 ? n : 0] = 0;
    return n;
}

static int sees_inside(void) {
    char buf[64], cwd[64];
    int bad = 0;
    if (read_file("/inside.txt", buf, sizeof buf) < 0 || strcmp(buf, "in the jail\n")) bad |= 1;
    if (!getcwd(cwd, sizeof cwd) || strcmp(cwd, "/")) bad |= 2;
    if (read_file("/../../inside.txt", buf, sizeof buf) < 0) bad |= 4;
    chdir("sub");
    if (read_file("../../../inside.txt", buf, sizeof buf) < 0) bad |= 8;
    return bad;
}

static int follows_links_inside(void) {
    char buf[64];
    return read_file("/sub/link", buf, sizeof buf) > 0 && !strcmp(buf, "in the jail\n") ? 0 : 1;
}

static int outside_is_gone(void) {
    struct stat st;
    return stat("/tmp/jail", &st) < 0 && errno == ENOENT && stat("/outside.txt", &st) < 0 ? 0 : 1;
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    mkdir("/tmp/jail", 0755);
    mkdir("/tmp/jail/sub", 0755);
    FILE *f = fopen("/tmp/jail/inside.txt", "w");
    fputs("in the jail\n", f);
    fclose(f);
    f = fopen("/tmp/outside.txt", "w");
    fputs("outside\n", f);
    fclose(f);
    unlink("/tmp/jail/sub/link");
    symlink("/inside.txt", "/tmp/jail/sub/link");

    check("inside, the folder is \"/\", and \"..\" doesn't climb out of it", in_jail(sees_inside) == 0);
    check("an absolute symlink is followed inside it", in_jail(follows_links_inside) == 0);
    check("what's outside can't be reached", in_jail(outside_is_gone) == 0);

    pid_t child = fork();
    if (child == 0) {
        setuid(65534);
        _exit(chroot("/tmp/jail") < 0 && errno == EPERM ? 0 : 1);
    }
    int status;
    waitpid(child, &status, 0);
    check("only root may chroot", WIFEXITED(status) && WEXITSTATUS(status) == 0);

    char buf[64];
    check("the caller itself is still outside", read_file("/tmp/outside.txt", buf, sizeof buf) > 0);
    printf("jail: %d/%d passed\n", passed, total);
    return passed == total ? 0 : 1;
}
