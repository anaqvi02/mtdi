#include <stdio.h>
#include <fcntl.h>
#include <unistd.h>
#include <sys/time.h>
#include <sys/stat.h>
#include <sys/socket.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <netinet/in.h>
#include <stdlib.h>

#define ITERS 500000
#define FORK_ITERS 5000

double get_time() {
    struct timeval tv;
    gettimeofday(&tv, NULL);
    return tv.tv_sec + tv.tv_usec * 1e-6;
}

int main() {
    double start, end;
    struct stat st;
    char buf[16];
    struct sockaddr_in dummy_addr = {0};
    dummy_addr.sin_family = AF_INET;

    printf("Starting Exhaustive Benchmark Suite...\n\n");

    // 1. STAT
    start = get_time();
    for (int i = 0; i < ITERS; i++) {
        stat("/dev/null", &st);
    }
    end = get_time();
    printf("STAT           (%6d): %.6f seconds\n", ITERS, end - start);

    // 2. OPEN / CLOSE
    start = get_time();
    for (int i = 0; i < ITERS; i++) {
        int fd = open("/dev/null", O_RDONLY);
        if (fd >= 0) close(fd);
    }
    end = get_time();
    printf("OPEN/CLOSE     (%6d): %.6f seconds\n", ITERS, end - start);

    // 3. READ
    int fd_zero = open("/dev/zero", O_RDONLY);
    start = get_time();
    for (int i = 0; i < ITERS; i++) {
        read(fd_zero, buf, 1);
    }
    end = get_time();
    close(fd_zero);
    printf("READ           (%6d): %.6f seconds\n", ITERS, end - start);

    // 4. WRITE
    int fd_null = open("/dev/null", O_WRONLY);
    start = get_time();
    for (int i = 0; i < ITERS; i++) {
        write(fd_null, "a", 1);
    }
    end = get_time();
    close(fd_null);
    printf("WRITE          (%6d): %.6f seconds\n", ITERS, end - start);

    // 5. SOCKET
    start = get_time();
    for (int i = 0; i < ITERS; i++) {
        int s = socket(AF_INET, SOCK_STREAM, 0);
        if (s >= 0) close(s);
    }
    end = get_time();
    printf("SOCKET/CLOSE   (%6d): %.6f seconds\n", ITERS, end - start);

    // 6. CONNECT, SEND, RECV
    int s = socket(AF_INET, SOCK_STREAM, 0);
    start = get_time();
    for (int i = 0; i < ITERS; i++) {
        connect(s, (struct sockaddr*)&dummy_addr, sizeof(dummy_addr));
        send(s, buf, 1, 0);
        recv(s, buf, 1, 0);
    }
    end = get_time();
    close(s);
    printf("CONN/SEND/RECV (%6d): %.6f seconds\n", ITERS, end - start);

    // 7. MMAP / MUNMAP
    start = get_time();
    for (int i = 0; i < ITERS; i++) {
        void *mem = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (mem != MAP_FAILED) {
            munmap(mem, 4096);
        }
    }
    end = get_time();
    printf("MMAP/MUNMAP    (%6d): %.6f seconds\n", ITERS, end - start);

    // 8. FORK / EXECVE / EXIT (doing less iterations so we don't fork bomb)
    char *argv[] = {"/usr/bin/true", NULL};
    char *envp[] = {NULL};
    start = get_time();
    for (int i = 0; i < FORK_ITERS; i++) {
        pid_t pid = fork();
        if (pid == 0) {
            execve("/usr/bin/true", argv, envp);
            exit(0);
        } else if (pid > 0) {
            waitpid(pid, NULL, 0);
        }
    }
    end = get_time();
    printf("FORK/EXEC/EXIT (%6d): %.6f seconds\n", FORK_ITERS, end - start);

    return 0;
}
