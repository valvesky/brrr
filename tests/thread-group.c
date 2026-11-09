/* Test helper for test_daemon.py: a process whose leader thread has exited
 * (a zombie leader) while another thread keeps running until stdin closes. */
#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <sys/prctl.h>
#include <unistd.h>

static void *worker(void *data) {
    (void)data;
    char byte;
    if (write(STDOUT_FILENO, "ready\n", 6) != 6) _exit(2);
    while (read(STDIN_FILENO, &byte, 1) < 0 && errno == EINTR) {}
    return NULL;
}

int main(void) {
    pthread_t thread;
    if (prctl(PR_SET_NAME, "oma_evt_test") != 0 || pthread_create(&thread, NULL, worker, NULL) != 0) return 2;
    pthread_exit(NULL);
}
