#include <stdio.h>
#include <stdlib.h>
#include <fcntl.h>
#include <unistd.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#include <sys/stat.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <string.h>

int main() {
    printf("[Victim] Starting exhaustive syscall test...\n");

    // 1. stat
    struct stat st;
    stat("test.txt", &st);

    // 2. open, write, read, close
    int fd = open("test.txt", O_RDWR | O_CREAT, 0644);
    if (fd != -1) {
        write(fd, "hello", 5);
        lseek(fd, 0, SEEK_SET);
        char buf[16];
        read(fd, buf, 5);
        close(fd);
    }

    // 3. mmap, munmap
    void *mem = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (mem != MAP_FAILED) {
        munmap(mem, 4096);
    }

    // 4. socket, connect, send, recv
    int sock = socket(AF_INET, SOCK_STREAM, 0);
    if (sock != -1) {
        struct sockaddr_in server;
        server.sin_family = AF_INET;
        server.sin_port = htons(80);
        inet_pton(AF_INET, "93.184.216.34", &server.sin_addr); // example.com
        
        // This will block slightly or fail, but we just want to hit the hook
        connect(sock, (struct sockaddr *)&server, sizeof(server));
        
        send(sock, "GET / HTTP/1.0\r\n\r\n", 18, 0);
        char recv_buf[32];
        recv(sock, recv_buf, sizeof(recv_buf), 0);
        
        close(sock);
    }

    // 5. fork, execve, exit
    pid_t pid = fork();
    if (pid == 0) {
        // Child
        char *argv[] = {"/bin/echo", "Child process reporting in!", NULL};
        char *envp[] = {NULL};
        execve("/bin/echo", argv, envp);
        exit(0);
    } else if (pid > 0) {
        // Parent
        waitpid(pid, NULL, 0);
    }

    printf("[Victim] All tests completed! Exiting...\n");
    return 0;
}