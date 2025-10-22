typecheck:
    uv run --frozen pyright

format:
    uv run --frozen ruff check src/picture_frame_ui --fix --exit-non-zero-on-fix
    uv run --frozen ruff format src/picture_frame_ui

test-all:
    uv run --frozen pytest -vv

test TEST:
    uv run --frozen pytest -vv {{TEST}}

coverage-all:
    uv run --frozen coverage erase
    uv run --frozen coverage run --source=src/picture_frame_ui --omit "*_test.py" -m pytest -vv
    uv run --frozen coverage report -m

coverage TEST:
    uv run --frozen coverage erase
    uv run --frozen coverage run --source=src/picture_frame_ui --omit "*_test.py" -m pytest -vv {{TEST}}
    uv run --frozen coverage report -m

watch-all:
    watchexec -f "**/*.py" uv run --frozen pytest -vv

watch TEST:
    watchexec -f "**/*.py" uv run --frozen pytest -vv {{TEST}}

deps-upgrade:
    uv lock --upgrade
    uv sync

run:
    uv run python main.py

run-debug:
    DEBUG=1 uv run python main.py