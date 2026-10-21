/*
 * Photo Frame Display — DRM/GBM/EGL digital photo frame.
 * Copyright (C) 2026 Daniel Mikusa <dan@mikusa.com>
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU Affero General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
 * GNU Affero General Public License for more details.
 *
 * You should have received a copy of the GNU Affero General Public License
 * along with this program. If not, see <https://www.gnu.org/licenses/>.
 */

/*
 * Tiny tmpfs logger for the display app.
 *
 * systemd suppresses the display app's stdout/stderr so a chatty service does
 * not write to the SD card. Diagnostics go here instead: /tmp is a tmpfs on the
 * frame, the file is capped at DEFAULT_LOG_MAX_BYTES with one rotated backup,
 * and nothing is ever fsynced.
 */

#include "logger.h"

#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

static FILE *log_fp = NULL;
static char  log_path[LOG_PATH_MAX];
static char  log_rotated_path[LOG_PATH_MAX + 3];
static long  log_bytes = 0;
static long  log_max_bytes = DEFAULT_LOG_MAX_BYTES;

static const char *const level_names[] = {
    "DEBUG", "INFO", "WARN", "ERROR"
};

const char *log_level_name(enum log_level level)
{
    if (level < LOG_DEBUG || level > LOG_ERROR) return "LOG";
    return level_names[level];
}

size_t log_format_timestamp(char *buf, size_t buflen, long long epoch_seconds)
{
    if (!buf || buflen == 0) return 0;

    time_t t = (time_t)epoch_seconds;
    struct tm tm_utc;
    if (!gmtime_r(&t, &tm_utc)) {
        buf[0] = '\0';
        return 0;
    }
    return strftime(buf, buflen, "%Y-%m-%dT%H:%M:%SZ", &tm_utc);
}

int log_should_rotate(long current_bytes, long max_bytes, size_t line_len)
{
    if (max_bytes <= 0) return 0;
    if (current_bytes <= 0) return 0;
    return current_bytes + (long)line_len > max_bytes;
}

static FILE *open_log_file(const char *path, int truncate)
{
    int flags = O_WRONLY | O_CREAT | O_CLOEXEC | (truncate ? O_TRUNC : O_APPEND);
    int fd = open(path, flags, 0600);
    if (fd < 0) return NULL;

    /* The file may predate this process with looser permissions. */
    (void)fchmod(fd, 0600);

    FILE *fp = fdopen(fd, truncate ? "w" : "a");
    if (!fp) {
        close(fd);
        return NULL;
    }
    setvbuf(fp, NULL, _IOLBF, 0);
    return fp;
}

int log_open(const char *path)
{
    log_close();

    log_path[0] = '\0';
    log_rotated_path[0] = '\0';

    if (!path || path[0] == '\0' || strlen(path) >= sizeof(log_path)) {
        return -1;
    }

    snprintf(log_path, sizeof(log_path), "%s", path);
    snprintf(log_rotated_path, sizeof(log_rotated_path), "%s.1", path);

    log_fp = open_log_file(log_path, 0);
    if (!log_fp) return -1;

    struct stat st;
    log_bytes = (fstat(fileno(log_fp), &st) == 0) ? (long)st.st_size : 0;
    return 0;
}

void log_close(void)
{
    if (log_fp) {
        fflush(log_fp);
        fclose(log_fp);
        log_fp = NULL;
    }
    log_bytes = 0;
}

void log_set_max_bytes(long bytes)
{
    log_max_bytes = bytes;
}

/* Rename the current file to .1 and start a fresh one. Best effort: if
 * anything fails we fall back to stderr rather than crashing. */
static void rotate_log(void)
{
    if (!log_fp) return;

    fflush(log_fp);
    fclose(log_fp);
    log_fp = NULL;

    unlink(log_rotated_path);
    if (rename(log_path, log_rotated_path) != 0) {
        /* Could not rotate; keep appending to the existing file. */
        log_fp = open_log_file(log_path, 0);
        return;
    }

    log_fp = open_log_file(log_path, 1);
    log_bytes = 0;
}

static void write_stderr_line(const char *line, size_t len)
{
    fwrite(line, 1, len, stderr);
    fflush(stderr);
}

void log_msg(enum log_level level, const char *fmt, ...)
{
    char msg[1024];
    char stamp[32];
    char line[1200];

    va_list ap;
    va_start(ap, fmt);
    vsnprintf(msg, sizeof(msg), fmt, ap);
    va_end(ap);

    log_format_timestamp(stamp, sizeof(stamp), (long long)time(NULL));

    int n = snprintf(line, sizeof(line), "%s %s %s\n",
                     stamp, log_level_name(level), msg);
    if (n < 0) return;
    size_t len = ((size_t)n < sizeof(line)) ? (size_t)n : sizeof(line) - 1;

    if (!log_fp) {
        write_stderr_line(line, len);
        return;
    }

    if (log_should_rotate(log_bytes, log_max_bytes, len)) {
        rotate_log();
        if (!log_fp) {
            write_stderr_line(line, len);
            return;
        }
    }

    size_t wrote = fwrite(line, 1, len, log_fp);
    fflush(log_fp);
    log_bytes += (long)wrote;
    if (wrote != len) {
        write_stderr_line(line, len);
    }
}

_Noreturn void log_fatal(const char *file, int line, const char *fmt, ...)
{
    int saved_errno = errno;

    char msg[896];
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(msg, sizeof(msg), fmt, ap);
    va_end(ap);

    log_msg(LOG_ERROR, "Exiting at %s:%d: %s (errno=%d, %s)",
            file, line, msg, saved_errno, strerror(saved_errno));

    fprintf(stderr, "ERROR at %s:%d: %s (errno=%d, %s)\n",
            file, line, msg, saved_errno, strerror(saved_errno));

    log_close();
    exit(1);
}
