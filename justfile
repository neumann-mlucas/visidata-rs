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
