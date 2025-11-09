# Contributing to Digital Picture Frame Project

This document outlines the guidelines and conventions for contributing to this project.

## General Rules
- **Do not output summaries** of the work done, that is not needed

## Code Generation Rules

This file contains persistent rules and conventions that should be followed when generating or modifying code in this project.

## Project Structure Rules

### Workspace Organization
- **Use uv workspace structure** with separate packages in `packages/` directory
- **Three main packages**:
  - `packages/ui/` - Picture frame UI components and main application
  - `packages/photo-manager/` - Photo loading, importing, and management
  - `packages/frame-server/` - Web server for uploading photos
- **Root directory** contains workspace configuration and main entry point

### Import Rules
- **Use workspace dependencies** with `{ workspace = true }` in pyproject.toml
- **Cross-package imports** should use full package names:
  - `from photo_manager.photos import PhotoLoader`
  - `from picture_frame_ui.config import FrameConfig`
  - `from frame_server.server import run_server`
- **No relative imports** between packages
- **Within package imports** can use relative imports (e.g., `from .config import FrameConfig`)

## Code Style and Standards

### Python Conventions
- **Follow PEP 8** for code style
- **Use type hints** for all function parameters and return values
- **Favor specific types** for hints, example use Path instead of str when referencing file paths
- **Include comprehensive docstrings** for all public functions and classes
- **Use pathlib.Path** instead of os.path for file operations
- **Prefer f-strings** for string formatting

### Error Handling
- **Do not try..catch imports** if the import fails, it will throw an exception and that is fine
- **Use specific exception types** rather than bare `except:`
- **Log errors with context** using the logging module
- **Fail fast if something is wrong** do not try to keep going, unless otherwise instructed
- **Return meaningful error codes** from main functions (0 for success, 1+ for errors)

### Logging
- **Use Python logging module** with structured format
- **Include logger name, level, and timestamp** in log format
- **Use appropriate log levels**:
  - DEBUG: Detailed diagnostic info
  - INFO: General operational messages
  - WARNING: Something unexpected but handled
  - ERROR: Serious problem that prevented operation
- **Support DEBUG environment variable** for verbose logging

## Architecture Rules

### Threading and Concurrency
- **Use daemon threads** for background services (like the web server)
- **Proper thread naming** for debugging
- **Clean shutdown handling** with appropriate signal handling

### Configuration Management
- **Use JSON configuration files** with reasonable defaults
- **Environment variable support** for overrides
- **Validate configuration** on startup with helpful error messages
- **Store configuration in workspace root** (`frame-config.json`)

### Dependencies
- **Minimize external dependencies** - only add what's necessary
- **Pin major versions** in pyproject.toml for stability
- **Use workspace dependencies** for internal packages
- **Separate dev dependencies** in dependency-groups

## Platform-Specific Rules

### macOS Compatibility
- **GTK4 support** with proper library path configuration
- **Environment variable setup** for DYLD_LIBRARY_PATH and GI_TYPELIB_PATH
- **Homebrew library detection** with fallbacks for common paths
- **Platform detection** using `platform.system() == "Darwin"`

### Cross-Platform Considerations
- **Use pathlib.Path** for cross-platform file operations
- **Environment variable defaults** that work on multiple platforms
- **Conditional imports** and setup based on platform when needed

## API and Interface Rules

### Web Server API
- **RESTful endpoints** with clear naming
- **JSON responses** with consistent structure
- **Error responses** include helpful error messages
- **CORS headers** for web client compatibility
- **Health check endpoint** for monitoring

### UI Guidelines
- **GTK4 application structure** with proper lifecycle management
- **Clean shutdown** on application quit signals
- **Screen dimension detection** for optimal photo sizing
- **Cross-platform keyboard shortcuts** (Cmd+Q on macOS, Ctrl+Q elsewhere)

## Testing Rules

### Test Structure
- **Tests alongside source code** in the same package
- **Test files named** `*_test.py`
- **Use pytest** as the testing framework
- **Include test configuration** in pyproject.toml

### Test Coverage
- **Unit tests** for individual functions and classes
- **Integration tests** for component interactions
- **Mock external dependencies** when appropriate
- **Test error conditions** and edge cases

## Git and Version Control

- **Create new branches** for new workstreams, work is done on a branch
- **Merge branches only after confirmation**, do not merge without confirming first

### Commit Guidelines
- **Clear, descriptive commit messages**
- **Separate commits** for different types of changes (features, fixes, refactoring)
- **Reference issues** in commit messages when applicable

---

## Development Setup

### Prerequisites
- Python 3.9+
- uv package manager
- GTK4 (for UI components)
- On macOS: Homebrew for GTK4 installation

### Installation
```bash
# Clone the repository
git clone <repository-url>
cd picture-frame-ui

# Install dependencies
uv sync

# On macOS, set up GTK environment
export DYLD_LIBRARY_PATH="/opt/homebrew/lib:$DYLD_LIBRARY_PATH"
export GI_TYPELIB_PATH="/opt/homebrew/lib/girepository-1.0:$GI_TYPELIB_PATH"

# Run the application
uv run python main.py
```
