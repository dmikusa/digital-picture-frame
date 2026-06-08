# Testing

## Commands

```bash
make test              # Run all tests (Rust + C in container)
make test-rust         # Run Rust tests only (20 unit tests)
make test-c            # Run C build + lint in container
make build-c-container # Build the container image for C testing
```

### Rust tests

```bash
cargo test        # 20 unit tests, all must pass
cargo clippy      # must be clean
```

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
