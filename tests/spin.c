/* spin: keeps one CPU busy for a fixed amount of work (no system calls on the way), then prints how
 * long it took, in milliseconds. Several at once take as long as one if they run on different CPUs,
 * and that many times longer if they share one (tests/smp.py). */
#include <stdio.h>
#include <stdlib.h>
#include <time.h>

int main(int argc, char **argv) {
    long rounds = argc > 1 ? atol(argv[1]) : 100;
    struct timespec start, end;
    clock_gettime(CLOCK_MONOTONIC, &start);
    volatile unsigned long x = 1;
    for (long r = 0; r < rounds; r++)
        for (long i = 0; i < 1000000; i++)
            x = x * 6364136223846793005UL + 1442695040888963407UL;
    clock_gettime(CLOCK_MONOTONIC, &end);
    long ms = (end.tv_sec - start.tv_sec) * 1000 + (end.tv_nsec - start.tv_nsec) / 1000000;
    printf("%ld\n", ms);
    return 0;
}
