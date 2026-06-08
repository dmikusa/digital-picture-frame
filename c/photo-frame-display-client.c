/*
 * sender.c
 *
 * Simple client for photo-frame-display.c.
 * Opens a persistent connection, sends one image path, waits for READY,
 * then sends the next.  This gives natural backpressure because the
 * display app pauses socket reads when it is full.
 *
 * Build: gcc sender.c -o sender
 *
 * Usage:
 *   ./sender /tmp/photo-frame.sock \
 *       /path/to/photo1.jpg \
 *       /path/to/photo2.jpg \
 *       /path/to/photo3.jpg
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/socket.h>
#include <sys/un.h>

static int connect_socket(const char *path)
{
    int fd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (fd < 0) {
        perror("socket");
        return -1;
    }
    struct sockaddr_un addr = { .sun_family = AF_UNIX };
    strncpy(addr.sun_path, path, sizeof(addr.sun_path) - 1);
    if (connect(fd, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        perror("connect");
        close(fd);
        return -1;
    }
    return fd;
}

static int wait_for_ready(int fd)
{
    char buf[64];
    size_t len = 0;
    while (1) {
        ssize_t n = read(fd, buf + len, sizeof(buf) - len - 1);
        if (n < 0) {
            perror("read");
            return -1;
        }
        if (n == 0) {
            fprintf(stderr, "Server closed connection before sending READY\n");
            return -1;
        }
        len += n;
        buf[len] = '\0';
        if (strstr(buf, "READY")) {
            return 0;
        }
        if (len >= sizeof(buf) - 1) {
            fprintf(stderr, "Server sent unexpected response\n");
            return -1;
        }
    }
}

int main(int argc, char **argv)
{
    if (argc < 3) {
        fprintf(stderr, "Usage: %s <socket_path> <image1> [image2] ...\n", argv[0]);
        return 1;
    }

    const char *sock_path = argv[1];
    int fd = connect_socket(sock_path);
    if (fd < 0) return 1;

    for (int i = 2; i < argc; ++i) {
        printf("Sending: %s\n", argv[i]);
        if (dprintf(fd, "IMG %s\n", argv[i]) < 0) {
            perror("dprintf");
            close(fd);
            return 1;
        }

        /* Wait for READY before sending the next image.
         * This naturally blocks until the display app has a free slot. */
        if (wait_for_ready(fd) < 0) {
            close(fd);
            return 1;
        }
        printf("  -> got READY, can send next\n");
    }

    close(fd);
    printf("All images sent.\n");
    return 0;
}
