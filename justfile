fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

clippy:
    cargo clippy --all-targets -- -D warnings

test:
    cargo test

check: fmt-check clippy test

mod remote

# Check every record against .claude/memento.yaml; pinned by the commit sha of the v0.10.0 tag.
records:
    uvx --from git+https://github.com/ryzhakar/memento@702292b203b84a32d5d001ee78e085a504545c57 memento check
