// Anonymous PROT_NONE reservations, as used by glibc's per-thread malloc arenas.
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

static int passed, total;
static void check(const char *name, int ok) {
    total++;
    passed += ok;
    printf("%s  %s\n", ok ? "ok  " : "FAIL", name);
}

int main(void) {
    const size_t page = 4096, arena = 64UL << 20;
    char *areas[16];
    int n = 0;
    for (; n < 16; n++) {
        areas[n] = mmap(NULL, arena, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (areas[n] == MAP_FAILED) break;
    }
    check("reserve 1 GiB of arenas without committing 1 GiB of RAM", n == 16);
    for (int i = 0; i < n; i++) munmap(areas[i], arena);

    char *p = mmap(NULL, 3 * page, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (p == MAP_FAILED) return 1;
    unsigned char resident[3] = { 1, 1, 1 };
    check("reservations are mapped but not resident", mincore(p, 3 * page, resident) == 0
          && !(resident[0] & 1) && !(resident[1] & 1) && !(resident[2] & 1));
    errno = 0;
    void *collision = mmap(p, page, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED_NOREPLACE, -1, 0);
    check("NOREPLACE detects an uncommitted reservation", collision == MAP_FAILED && errno == EEXIST);
    int committed = mprotect(p + page, page, PROT_READ | PROT_WRITE) == 0;
    check("mprotect commits just the requested page", committed && p[page] == 0);
    if (!committed) return 1;
    strcpy(p + page, "kept");
    check("only the committed page becomes resident", mincore(p, 3 * page, resident) == 0
          && !(resident[0] & 1) && (resident[1] & 1) && !(resident[2] & 1));
    check("PROT_NONE and back preserves data", mprotect(p + page, page, PROT_NONE) == 0
          && mprotect(p + page, page, PROT_READ | PROT_WRITE) == 0 && strcmp(p + page, "kept") == 0);

    pid_t child = fork();
    if (child == 0) {
        int ok = mprotect(p, page, PROT_READ | PROT_WRITE) == 0 && p[0] == 0;
        if (ok) p[0] = 42;
        _exit(ok ? 0 : 1);
    }
    int status = 0;
    check("fork preserves reservations independently", child > 0 && waitpid(child, &status, 0) == child
          && WIFEXITED(status) && WEXITSTATUS(status) == 0
          && mincore(p, page, resident) == 0 && !(resident[0] & 1));
    child = fork();
    if (child == 0) { *(volatile char *)p = 1; _exit(1); }
    check("an untouched reservation remains inaccessible", child > 0 && waitpid(child, &status, 0) == child
          && WIFSIGNALED(status) && WTERMSIG(status) == SIGSEGV);

    // A PROT_NONE reservation within the kernel's potential stack-growth area must
    // not be mistaken for a missing stack page and silently made writable.
    char *stack_slot = (char *)(0x00007ffffffff000ULL - (6ULL << 20));
    char *stack_reserved = mmap(stack_slot, page, PROT_NONE,
                                MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED_NOREPLACE, -1, 0);
    int blocked = 0;
    if (stack_reserved == stack_slot) {
        child = fork();
        if (child == 0) { *(volatile char *)stack_reserved = 1; _exit(1); }
        blocked = child > 0 && waitpid(child, &status, 0) == child
                  && WIFSIGNALED(status) && WTERMSIG(status) == SIGSEGV;
        munmap(stack_reserved, page);
    }
    check("PROT_NONE reservation cannot auto-grow into a stack page", blocked);

    munmap(p, 3 * page);
    p = mmap(NULL, 3 * page, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    char *moved = mremap(p, 3 * page, 5 * page, MREMAP_MAYMOVE);
    check("mremap preserves and extends an uncommitted reservation", moved != MAP_FAILED
          && mincore(moved + 2 * page, 3 * page, resident) == 0
          && !(resident[0] & 1) && !(resident[1] & 1) && !(resident[2] & 1));
    if (moved != MAP_FAILED) { p = moved; munmap(p, 5 * page); }
    else munmap(p, 3 * page);
    errno = 0;
    check("munmap removes the reservation", mincore(p, page, resident) == -1 && errno == ENOMEM);
    printf("anonmem: %d/%d passed\n", passed, total);
    return passed == total ? 0 : 1;
}
