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

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "logger.h"

#define TEST_ASSERT(cond) \
    do { \
        if (!(cond)) { \
            fprintf(stderr, "FAIL: %s at %s:%d\n", #cond, __FILE__, __LINE__); \
            return 1; \
        } \
    } while(0)

#define TEST_BUF 8192

static int read_file(const char *path, char *buf, size_t buflen)
{
    FILE *fp = fopen(path, "r");
    if (!fp) return -1;
    size_t n = fread(buf, 1, buflen - 1, fp);
    fclose(fp);
    buf[n] = '\0';
    return (int)n;
}

static long file_size(const char *path)
{
    struct stat st;
    if (stat(path, &st) != 0) return -1;
    return (long)st.st_size;
}

static int test_format_timestamp(void)
{
    char buf[32];

    size_t n = log_format_timestamp(buf, sizeof(buf), 0);
    TEST_ASSERT(n == strlen("1970-01-01T00:00:00Z"));
    TEST_ASSERT(strcmp(buf, "1970-01-01T00:00:00Z") == 0);

    n = log_format_timestamp(buf, sizeof(buf), 1760000000LL);
    TEST_ASSERT(n == 20);
    TEST_ASSERT(buf[4] == '-');
    TEST_ASSERT(buf[10] == 'T');
    TEST_ASSERT(buf[19] == 'Z');

    /* Too-small buffer fails cleanly */
    TEST_ASSERT(log_format_timestamp(buf, 0, 0) == 0);
    TEST_ASSERT(log_format_timestamp(NULL, sizeof(buf), 0) == 0);

    printf("PASS: log_format_timestamp\n");
    return 0;
}

static int test_should_rotate(void)
{
    TEST_ASSERT(log_should_rotate(0, 100, 50) == 0);
    TEST_ASSERT(log_should_rotate(60, 100, 30) == 0);
    TEST_ASSERT(log_should_rotate(60, 100, 41) == 1);
    TEST_ASSERT(log_should_rotate(100, 0, 50) == 0);
    TEST_ASSERT(log_should_rotate(100, -1, 50) == 0);

    printf("PASS: log_should_rotate\n");
    return 0;
}

static int test_write_and_permissions(const char *dir)
{
    char path[512];
    char buf[TEST_BUF];
    struct stat st;

    snprintf(path, sizeof(path), "%s/display.log", dir);

    TEST_ASSERT(log_open(path) == 0);
    log_msg(LOG_INFO, "hello world %d", 42);
    log_msg(LOG_WARN, "a warning");
    log_close();

    TEST_ASSERT(stat(path, &st) == 0);
    TEST_ASSERT((st.st_mode & 0777) == 0600);

    TEST_ASSERT(read_file(path, buf, sizeof(buf)) > 0);

    /* "<YYYY-MM-DDTHH:MM:SSZ> INFO hello world 42" */
    TEST_ASSERT(strlen(buf) >= 20);
    TEST_ASSERT(buf[4] == '-');
    TEST_ASSERT(buf[10] == 'T');
    TEST_ASSERT(buf[19] == 'Z');
    TEST_ASSERT(buf[20] == ' ');
    TEST_ASSERT(strstr(buf, " INFO hello world 42\n") != NULL);
    TEST_ASSERT(strstr(buf, " WARN a warning\n") != NULL);

    /* Appending, not truncating, on reopen */
    TEST_ASSERT(log_open(path) == 0);
    log_msg(LOG_INFO, "second open");
    log_close();
    TEST_ASSERT(read_file(path, buf, sizeof(buf)) > 0);
    TEST_ASSERT(strstr(buf, "hello world 42") != NULL);
    TEST_ASSERT(strstr(buf, "second open") != NULL);

    printf("PASS: log write + permissions\n");
    return 0;
}

static int test_rotation(const char *dir)
{
    char path[512];
    char rotated[520];
    char buf[TEST_BUF];
    long max = 300;

    snprintf(path, sizeof(path), "%s/rotate.log", dir);
    snprintf(rotated, sizeof(rotated), "%s.1", path);

    unlink(path);
    unlink(rotated);

    log_set_max_bytes(max);
    TEST_ASSERT(log_open(path) == 0);
    for (int i = 0; i < 30; ++i) {
        log_msg(LOG_INFO, "rotation line %02d paddingpaddingpadding", i);
    }
    log_close();
    log_set_max_bytes(DEFAULT_LOG_MAX_BYTES);

    TEST_ASSERT(file_size(path) >= 0);
    TEST_ASSERT(file_size(rotated) > 0);
    TEST_ASSERT(file_size(path) <= max);
    TEST_ASSERT(file_size(rotated) <= max);

    /* The backup holds older lines, the live file the newest one. */
    TEST_ASSERT(read_file(rotated, buf, sizeof(buf)) > 0);
    TEST_ASSERT(strstr(buf, "rotation line ") != NULL);
    TEST_ASSERT(strstr(buf, "rotation line 29") == NULL);

    TEST_ASSERT(read_file(path, buf, sizeof(buf)) > 0);
    TEST_ASSERT(strstr(buf, "rotation line 29") != NULL);

    unlink(path);
    unlink(rotated);

    printf("PASS: log rotation\n");
    return 0;
}

static int test_open_failure_falls_back(void)
{
    log_set_max_bytes(DEFAULT_LOG_MAX_BYTES);
    TEST_ASSERT(log_open("/nonexistent-photo-frame-dir/display.log") == -1);
    /* Must not crash, and must still accept messages (stderr fallback). */
    log_msg(LOG_ERROR, "fallback message %d", 7);
    log_close();

    printf("PASS: log open failure falls back\n");
    return 0;
}

int main(void)
{
    char dir[] = "/tmp/pf-logger-XXXXXX";
    if (!mkdtemp(dir)) {
        fprintf(stderr, "FAIL: mkdtemp\n");
        return 1;
    }

    int failures = 0;
    failures += test_format_timestamp();
    failures += test_should_rotate();
    failures += test_write_and_permissions(dir);
    failures += test_rotation(dir);
    failures += test_open_failure_falls_back();

    rmdir(dir);

    if (failures == 0) {
        printf("\nAll logger tests passed.\n");
    } else {
        printf("\n%d logger test(s) failed.\n", failures);
    }
    return failures;
}
