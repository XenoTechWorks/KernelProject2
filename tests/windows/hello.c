/* An ordinary Windows console program, built with mingw-w64 (the GCC Windows toolchain) into a real
 * hello.exe. Nothing about it knows about KernelProject2. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int main(int argc, char **argv) {
    printf("Hello from Windows! %d argument(s), program %s\n", argc, argv[0]);
    for (int i = 1; i < argc; i++) printf("  argument %d: %s\n", i, argv[i]);
    char *copy = malloc(64);
    strcpy(copy, "malloc and strcpy work");
    puts(copy);
    free(copy);
    return 0;
}
