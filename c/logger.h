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

#ifndef LOGGER_H
#define LOGGER_H

#include <stddef.h>

/* Rotate once the current file would grow past this size (tmpfs, not the SD
 * card, but still bounded so a long-running frame cannot fill RAM). */
#define DEFAULT_LOG_MAX_BYTES   (256L * 1024L)

#define LOG_PATH_MAX            512

enum log_level {
    LOG_DEBUG = 0,
    LOG_INFO,
    LOG_WARN,
    LOG_ERROR,
};

#if defined(__GNUC__)
#define LOG_PRINTF(fmt_idx, arg_idx) __attribute__((format(printf, fmt_idx, arg_idx)))
#else
#define LOG_PRINTF(fmt_idx, arg_idx)
#endif

/* Open `path` for append, creating it with mode 0600 if needed. Returns 0 on
 * success and -1 when the file cannot be opened; logging then goes to stderr. */
int  log_open(const char *path);
void log_close(void);

/* Change the rotation threshold. Defaults to DEFAULT_LOG_MAX_BYTES. */
void log_set_max_bytes(long bytes);

/* Timestamp as UTC: YYYY-MM-DDTHH:MM:SSZ. Returns bytes written (excluding NUL). */
size_t log_format_timestamp(char *buf, size_t buflen, long long epoch_seconds);

/* 1 when writing `line_len` more bytes should rotate first. */
int log_should_rotate(long current_bytes, long max_bytes, size_t line_len);

const char *log_level_name(enum log_level level);

/* Write one timestamped line: "<ts> <LEVEL> <message>\n". */
void log_msg(enum log_level level, const char *fmt, ...) LOG_PRINTF(2, 3);

/* Log an ERROR (with file, line, errno) to the log and stderr, then exit(1). */
_Noreturn void log_fatal(const char *file, int line, const char *fmt, ...)
    LOG_PRINTF(3, 4);

#endif
