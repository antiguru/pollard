#include <stdio.h>

// Sized so samply at its default 1 kHz reliably catches frames inside
// inner_loop. The previous values (10_000 × 100 ≈ 100ms total) made the
// e2e test flaky — sometimes the program exited before any user-code
// sample landed, leaving the profile with only dynamic-linker frames.
__attribute__((noinline))
int inner_loop(int n) {
    volatile int sum = 0;
    for (int i = 0; i < n; i++) {
        sum += i * i;
    }
    return sum;
}

int main(void) {
    long long total = 0;
    for (int i = 0; i < 200; i++) {
        total += inner_loop(200000);
    }
    printf("%lld\n", total);
    return 0;
}
