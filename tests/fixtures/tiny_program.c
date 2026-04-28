#include <stdio.h>

__attribute__((noinline))
void inner_loop(int n) {
    volatile int sum = 0;
    for (int i = 0; i < n; i++) {
        sum += i * i;
    }
    printf("%d\n", sum);
}

int main(void) {
    for (int i = 0; i < 100; i++) {
        inner_loop(10000);
    }
    return 0;
}
