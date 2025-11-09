typecheck:
    uv run --frozen pyright

format:
    uv run --frozen ruff check packages/ --fix --exit-non-zero-on-fix
    uv run --frozen ruff format packages/

test-all:
    uv run --frozen pytest -vv

test TEST:
    uv run --frozen pytest -vv {{TEST}}

coverage-ci:
    uv run --frozen coverage run --source=packages/ --omit "*_test.py" -m pytest -vv --junitxml=results.xml
    uv run --frozen coverage xml -o coverage.xml

coverage-all:
    uv run --frozen coverage erase
    uv run --frozen coverage run --source=packages/ --omit "*_test.py" -m pytest -vv
    uv run --frozen coverage report -m

coverage TEST:
    uv run --frozen coverage erase
    uv run --frozen coverage run --source=packages/ --omit "*_test.py" -m pytest -vv {{TEST}}
    uv run --frozen coverage report -m

watch-all:
    watchexec -f "**/*.py" uv run --frozen pytest -vv

watch TEST:
    watchexec -f "**/*.py" uv run --frozen pytest -vv {{TEST}}

deps-upgrade:
    uv lock --upgrade
    uv sync

run:
    #!/usr/bin/env fish
    set -x DYLD_LIBRARY_PATH "/opt/homebrew/lib:$DYLD_LIBRARY_PATH"
    set -x PKG_CONFIG_PATH "/opt/homebrew/lib/pkgconfig:$PKG_CONFIG_PATH"
    uv run python main.py

run-debug:
    #!/usr/bin/env fish
    set -x DYLD_LIBRARY_PATH "/opt/homebrew/lib:$DYLD_LIBRARY_PATH"
    set -x PKG_CONFIG_PATH "/opt/homebrew/lib/pkgconfig:$PKG_CONFIG_PATH"
    set -x DEBUG 1
    uv run python main.py