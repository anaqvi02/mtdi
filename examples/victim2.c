#include <stdio.h>
#include <fcntl.h>
#include <unistd.h>
#include <errno.h>
#include <string.h>

int main() {
    printf("Opening /dev/null...\n");
    int fd1 = open("/dev/null", O_RDONLY);
    if (fd1 < 0) {
        printf("Failed to open /dev/null: %s\n", strerror(errno));
    } else {
        printf("Successfully opened /dev/null\n");
        close(fd1);
    }
    
    printf("\nOpening /etc/passwd...\n");
    int fd2 = open("/etc/passwd", O_RDONLY);
    if (fd2 < 0) {
        printf("Failed to open /etc/passwd: %s\n", strerror(errno));
    } else {
        printf("Successfully opened /etc/passwd (THIS SHOULD NOT HAPPEN IN SANDBOX)\n");
        close(fd2);
    }
    return 0;
}
