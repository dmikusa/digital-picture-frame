# Testing

## Commands

```bash
cargo test        # 20 unit tests, all must pass
cargo clippy      # must be clean
```

## Methodology

- Aim for ~80% test coverage.
- Tests use `tempfile::NamedTempFile` and `tempfile::tempdir()`.
- Logger tests call `logger.log(&record)` directly to avoid the once-per-process global logger limit.
- Socket tests use `std::os::unix::net::UnixListener` for mock display servers.
