# Test tools and builds live in the user cache, not next to the checkout, so
# the checkout and every worktree find the same ones. `just setup` fills it.
cache := cache_directory() / "vdrs"
visidata_rev := "5f67ff8af28c59a8f7429e396601606f2ac8dd47"
export VDRS_VISIDATA := env("VDRS_VISIDATA", cache / "visidata")
export VDRS_PYTHON := env("VDRS_PYTHON", cache / "venv/bin/python")
export CARGO_TARGET_DIR := env("CARGO_TARGET_DIR", cache / "target")
# A missing corpus, DuckDB CLI or polars fails the tests instead of skipping them.
export VDRS_REQUIRE_TOOLS := "1"

default:
    @just --list --unsorted

# Fetch the pinned VisiData checkout and a Python venv with polars; check duckdb.
# Rerun-safe. A VDRS_VISIDATA or VDRS_PYTHON you set yourself is only checked.
setup:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ "$VDRS_VISIDATA" = "{{cache}}/visidata" ]; then
        if [ "$(git -C "$VDRS_VISIDATA" rev-parse HEAD 2>/dev/null)" != "{{visidata_rev}}" ]; then
            rm -rf "$VDRS_VISIDATA"
            git init -q "$VDRS_VISIDATA"
            git -C "$VDRS_VISIDATA" fetch -q --depth 1 https://github.com/saulpw/visidata {{visidata_rev}}
            git -C "$VDRS_VISIDATA" checkout -q FETCH_HEAD
        fi
    elif [ ! -d "$VDRS_VISIDATA/tests" ]; then
        echo "setup: VDRS_VISIDATA=$VDRS_VISIDATA has no tests/ folder" >&2
        exit 1
    fi
    if ! "$VDRS_PYTHON" -c "import polars" 2>/dev/null; then
        if [ "$VDRS_PYTHON" != "{{cache}}/venv/bin/python" ]; then
            echo "setup: VDRS_PYTHON=$VDRS_PYTHON can't import polars" >&2
            exit 1
        fi
        python3 -m venv "{{cache}}/venv"
        "{{cache}}/venv/bin/pip" install -q --disable-pip-version-check polars
    fi
    if ! duckdb --version >/dev/null 2>&1; then
        echo "setup: install the DuckDB CLI on PATH: https://duckdb.org/docs/installation" >&2
        exit 1
    fi
    echo "setup: VisiData {{visidata_rev}} at $VDRS_VISIDATA, polars in $VDRS_PYTHON, $(duckdb --version)"

build:
    cargo build --locked

# no doctests in the crate, so nextest covers everything
test *ARGS:
    cargo nextest run --locked {{ARGS}}

lint:
    cargo fmt --all -- --check
    cargo clippy --locked --no-deps --all-targets -- -D warnings

# Done-gate for humans, agents and CI
check *ARGS: lint (test ARGS)

# Line coverage of the test run (cargo install cargo-llvm-cov), HTML report.
# Set VDRS_PYTHON to cover the Python export too. Without rustup's
# llvm-tools, the system's llvm-cov/llvm-profdata stand in.
cov *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! rustup component list --installed 2>/dev/null | grep -q llvm-tools; then
        export LLVM_COV=${LLVM_COV:-$(command -v llvm-cov)} LLVM_PROFDATA=${LLVM_PROFDATA:-$(command -v llvm-profdata)}
    fi
    cargo llvm-cov nextest --locked --html {{ARGS}}

# Write tests/vd_local goldens by running each script (or the named ones) in
# VisiData, from the $VDRS_VISIDATA checkout. Review the diff before committing.
vd-golden *NAMES:
    #!/usr/bin/env bash
    set -euo pipefail
    vd=$VDRS_VISIDATA
    names=({{NAMES}})
    if [ ${#names[@]} -eq 0 ]; then
        for f in tests/vd_local/*.vd tests/vd_local/*.vdj tests/vd_local/*.vdx; do
            [ -e "$f" ] && names+=("$(basename "${f%.*}")")
        done
    fi
    mkdir -p tests/vd_local/golden
    VD_TESTTMP=$(mktemp -d)
    export VD_TESTTMP NO_COLOR=1
    trap 'rm -rf "$VD_TESTTMP"' EXIT
    for n in "${names[@]}"; do
        f=$(ls tests/vd_local/"$n".vd* | head -1)
        out=$(PYTHONPATH=$vd python3 -m visidata --config $vd/tests/.visidatarc \
            --visidata-dir $vd/tests/.visidata --batch -p "$f" -o "tests/vd_local/golden/$n.tsv" 2>&1) || true
        if grep -q "replay aborted" <<<"$out" || [ ! -s "tests/vd_local/golden/$n.tsv" ]; then
            echo "FAILED $n: $out" >&2
            rm -f "tests/vd_local/golden/$n.tsv"
        else
            echo "golden: $n"
        fi
    done
