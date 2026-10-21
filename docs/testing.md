# Testing

## Commands

```bash
make test              # Run all tests (Rust + C in container)
make test-rust         # Run Rust tests only
make test-c            # Run C build + lint in container
make build-c-container # Build the container image for C testing
```

### Rust tests

```bash
cargo test        # all unit + integration tests must pass
cargo clippy      # must be clean
```

The suite has three parts:

- **Unit tests** live next to the code in `src/`: config parsing and validation,
  index metadata/compaction/rotation, the log rotator, the display and control
  clients, QR screen rendering and password generation, the config-mode state
  machine, admin-server validation, and the USB/remote import modules.
- **Integration tests** in `tests/integration_tests.rs` exercise the display
  loop against a mock Unix-socket server, the import pipeline, concurrent
  imports sharing one index, and an end-to-end import-then-display path.
  ImageMagick-dependent tests print a skip message and return when
  `magick`/`convert` is not installed.
- **Admin server tests** in `tests/admin_server_tests.rs` start a real server and
  drive it over HTTP with `reqwest::blocking`, covering Basic Auth, form and
  JSON requests, redirects, staged-source behavior, sync status, secret
  redaction in API responses, the Finish-time config save (including a failed
  save that leaves the server running), and finish validation.

Remote-source tests use a local mock HTTP server (`src/import/test_http.rs`), so
they do not need network access or real Dropbox/Google credentials. The
service-account tests generate a temporary RSA key and verify the JWT signature.

### C tests

The C display app requires DRM/GBM/EGL headers (Linux-only). On macOS or other non-Linux systems, the C code is built and linted inside a container:

```bash
make test-c
```

This uses Podman (or Docker) to run a Debian container with the necessary dependencies. The container:
1. Compiles `photo-frame-display.c` with strict warnings (`-Wall -Wextra -Werror`)
2. Runs `cppcheck` static analysis

Override the container tool:
```bash
make test-c CONTAINER=docker
```

## Methodology

- Aim for ~80% test coverage.
- Tests use `tempfile::NamedTempFile` and `tempfile::tempdir()`.
- Logger tests call `logger.log(&record)` directly to avoid the once-per-process global logger limit.
- Socket tests use `std::os::unix::net::UnixListener` for mock display servers.
