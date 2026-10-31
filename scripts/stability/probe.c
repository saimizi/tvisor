#define _GNU_SOURCE
#define _POSIX_C_SOURCE 200809L
#include <errno.h>
#include <inttypes.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/wait.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

/* Single-vCPU timer/scheduler and RAM stress; no hypervisor test hooks. */
static double now(void) {
    struct timespec t;
    if (clock_gettime(CLOCK_MONOTONIC, &t)) { perror("clock_gettime"); exit(2); }
    return t.tv_sec + t.tv_nsec / 1e9;
}

static void memory_worker(volatile uint64_t *passes) {
    const size_t n = 32 * 1024 * 1024 / sizeof(uint64_t);
    volatile uint64_t *p = malloc(n * sizeof(*p));
    if (!p) _exit(3);
    for (uint64_t pass = 1;; ++pass) {
        for (size_t i = 0; i < n; ++i) p[i] = ((uint64_t)i * UINT64_C(0x9e3779b97f4a7c15)) ^ pass;
        for (size_t i = 0; i < n; ++i)
            if (p[i] != (((uint64_t)i * UINT64_C(0x9e3779b97f4a7c15)) ^ pass)) _exit(4);
        *passes = pass;
    }
}

int main(int argc, char **argv) {
    unsigned seconds = argc > 1 ? (unsigned)strtoul(argv[1], NULL, 10) : 60;
    if (!seconds || seconds > 86400) return 2;
    setvbuf(stdout, NULL, _IONBF, 0);
    volatile uint64_t *passes = mmap(NULL, sizeof(*passes), PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (passes == MAP_FAILED) { perror("mmap"); return 2; }
    *passes = 0;
    pid_t worker = fork();
    if (worker < 0) { perror("fork"); return 2; }
    if (!worker) memory_worker(passes);
    /* Start idle. Alternate idle/load every 30 monotonic seconds. */
    if (kill(worker, SIGSTOP)) return 2;
    double start = now(), previous = start, report = start, max_late = 0;
    uint64_t wakes = 0, late500 = 0, load_start_passes = 0;
    int loaded = 0, failed = 0;
    const long intervals[] = {1000000L, 10000000L, 100000000L, 1000000000L};
    printf("STAB START seconds=%u memory_bytes=%zu pid=%ld\n", seconds, (size_t)32 * 1024 * 1024, (long)getpid());
    while (now() - start < seconds) {
        double before = now();
        int desired_load = ((unsigned)((before - start) / 30)) % 2;
        if (desired_load != loaded) {
            if (loaded && *passes == load_start_passes) { printf("STAB FAIL memory_worker_no_progress\n"); failed = 1; break; }
            if (desired_load) load_start_passes = *passes;
            if (kill(worker, desired_load ? SIGCONT : SIGSTOP)) { failed = 1; break; }
            loaded = desired_load;
        }
        int status = 0;
        pid_t ended = waitpid(worker, &status, WNOHANG);
        if (ended != 0) { printf("STAB FAIL worker_status=%d waitpid=%ld\n", status, (long)ended); failed = 1; break; }
        long ns = intervals[wakes % 4];
        struct timespec request = { ns / 1000000000L, ns % 1000000000L }, remaining;
        while (nanosleep(&request, &remaining)) {
            if (errno != EINTR) { perror("nanosleep"); failed = 1; break; }
            request = remaining;
        }
        double after = now(), elapsed = after - before, late = elapsed - ns / 1e9;
        if (after < previous || elapsed + 0.000001 < ns / 1e9 || late > 2.0) {
            printf("STAB FAIL clock_or_wakeup elapsed=%.9f requested_ns=%ld\n", elapsed, ns); failed = 1; break;
        }
        if (failed) break;
        previous = after;
        if (late > max_late) max_late = late;
        if (late > 0.5) ++late500;
        ++wakes;
        if (after - report >= 30) {
            printf("STAB HEARTBEAT elapsed=%.3f wakes=%" PRIu64 " load=%d max_late_ms=%.3f late500=%" PRIu64 " memory_passes=%" PRIu64 "\n",
                   after - start, wakes, loaded, max_late * 1000, late500, *passes);
            report = after;
        }
    }
    /* Reap the worker and distinguish our SIGTERM from an earlier failure. */
    kill(worker, SIGCONT);
    kill(worker, SIGTERM);
    int status = 0;
    if (waitpid(worker, &status, 0) != worker || !WIFSIGNALED(status) || WTERMSIG(status) != SIGTERM) failed = 1;
    if (seconds >= 35 && *passes == 0) failed = 1;
    printf("STAB %s elapsed=%.3f wakes=%" PRIu64 " max_late_ms=%.3f late500=%" PRIu64 " memory_passes=%" PRIu64 "\n",
           failed ? "FAIL" : "PASS", now() - start, wakes, max_late * 1000, late500, *passes);
    return failed ? 1 : 0;
}
