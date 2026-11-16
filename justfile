default:
    @just --list --unsorted

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

# Write tests/vd_local goldens by running each script (or the named ones) in
# VisiData, from the ../visidata checkout. Review the diff before committing.
vd-golden *NAMES:
    #!/usr/bin/env bash
    set -euo pipefail
    vd=../visidata
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
