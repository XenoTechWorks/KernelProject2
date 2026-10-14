// Shared memory, checked the same way on Linux and on KernelProject2: memfd_create, two
// MAP_SHARED mappings of it seeing each other's writes, write() and read() agreeing with them,
// ftruncate and fallocate, and MAP_SHARED | MAP_ANONYMOUS memory shared with a child after fork.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

static int passed, total;

static void check(const char *name, int ok)
{
    total++;
    passed += ok;
    printf("%s  %s\n", ok ? "ok  " : "FAIL", name);
}

int main(void)
{
    int fd = memfd_create("test", MFD_CLOEXEC | MFD_ALLOW_SEALING);
    check("memfd_create gives a file descriptor", fd >= 0);
    check("ftruncate sets its size", ftruncate(fd, 3 * 4096) == 0);
    struct stat st;
    check("fstat sees the size", fstat(fd, &st) == 0 && st.st_size == 3 * 4096);

    char *a = mmap(NULL, 3 * 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    char *b = mmap(NULL, 3 * 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    check("it maps twice", a != MAP_FAILED && b != MAP_FAILED && a != b);
    strcpy(a + 4096 + 10, "through a");
    check("a write through one mapping shows in the other", strcmp(b + 4096 + 10, "through a") == 0);

    char buf[16] = { 0 };
    check("read() sees it too", pread(fd, buf, 9, 4096 + 10) == 9 && strcmp(buf, "through a") == 0);
    check("write() shows in the mappings", pwrite(fd, "from write", 10, 100) == 10 && memcmp(b + 100, "from write", 10) == 0);
    check("seals are accepted", fcntl(fd, F_ADD_SEALS, F_SEAL_SHRINK) == 0);
    check("fallocate grows it", fallocate(fd, 0, 0, 5 * 4096) == 0 && fstat(fd, &st) == 0 && st.st_size == 5 * 4096);

    munmap(b, 3 * 4096);
    close(fd);
    check("a mapping outlives the file being closed", strcmp(a + 4096 + 10, "through a") == 0);
    munmap(a, 3 * 4096);

    int *shared = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    int *private_ = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    *shared = 1;
    *private_ = 1;
    pid_t child = fork();
    if (child == 0) {
        *shared = 42;
        *private_ = 42;
        _exit(0);
    }
    int status;
    waitpid(child, &status, 0);
    check("MAP_SHARED | MAP_ANONYMOUS memory is shared with a child", *shared == 42);
    check("MAP_PRIVATE memory is not", *private_ == 1);

    printf("memfd: %d/%d passed\n", passed, total);
    return passed == total ? 0 : 1;
}
