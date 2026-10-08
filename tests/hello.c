/* An ordinary Linux C program, built with gcc and glibc and copied in unchanged.
 * If it runs on KernelProject2, so can other Linux programs that use the same calls. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/utsname.h>
#include <unistd.h>

int main(int argc, char **argv) {
    struct utsname u;
    uname(&u);
    char cwd[256];
    getcwd(cwd, sizeof cwd);
    char *buf = malloc(1 << 20); /* exercise brk/mmap */
    memset(buf, 'x', 1 << 20);
    printf("Hello from glibc! Running on %s %s (%s), pid %d, in %s\n", u.sysname, u.release, u.machine, getpid(), cwd);
    for (int i = 1; i < argc; i++) printf("  argument %d: %s\n", i, argv[i]);
    free(buf);
    return 0;
}
