# Rust project bootstrap standard

> Basis: 5 source reports in agents-of-anarchy-marketplace `research/bootstrap/sources/` (commit 5d3349c): `rust-toolkit-2026.md`, `rust-dont-panic.md`, `namtao-rust.md`, `clippy-stricter.md`, `std-traits-tour.md`. Date: 2026-10-15.
> Short names below: toolkit, dont-panic, namtao-rust, clippy-stricter, std-traits. (asserted) marks a claim carried by one source and marked asserted there.
> tokio and iroh are project givens. No source evaluates them.

## Toolchain and dev loop

| Item | Choice | Source | Reason |
|---|---|---|---|
| Installer | rustup, `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` | toolkit | Bootstrap step for any OS. |
| Channel | Stable, one exact version, 1.95 or later | clippy-stricter | Denying lints on an unpinned toolchain fails the build when new lints land. Lints in the set below may postdate versions before 1.95. |
| Channel conflict | Stable over nightly | toolkit vs clippy-stricter | Nightly's case rests on two asserted claims: "REALLY stable" and an 8x compile speedup on one crate (asserted). The pin argument holds. |
| Pin mechanism | `rust-toolchain.toml` with the exact version and the components below | none | No source names a mechanism; the pin itself is clippy-stricter's. |
| Components | `rustc`, `cargo`, `clippy`, `rustfmt`, `rust-analyzer` | namtao-rust | The list given there; toolkit's drops `rustfmt`. |
| Edition | 2024 | clippy-stricter | The only source naming an edition; it retires `if_let_mutex` (asserted). |
| Watch loop | `bacon clippy` | toolkit, namtao-rust | Two sources; reruns Clippy on save; "the first error ... usually is" the one to fix (toolkit). |
| Crate lookup | `cargo search`, then `cargo info <crate>` before `cargo add` | toolkit | Built into cargo; `info` shows the features a crate needs enabled. Zero added tools. |
| Gate | `cargo clippy -- -D warnings`, then `cargo test`, before every commit and on CI | clippy-stricter, toolkit | `-D warnings` also fails rustc warnings, which the lint table alone lets through. |
| Run | `cargo run` | toolkit | The run step of the watch chain given there. |

## Cargo.toml: profile and lints (verbatim block)

No source gives a `[profile]` section. None is adopted.

Level conflict: namtao-rust, dont-panic, and toolkit set every lint to `deny`. clippy-stricter sets `warn` and gates with `-D warnings`. This standard takes `deny` for every lint and keeps the `-D warnings` gate. Reason: the owner's strictest-standards rule; `deny` fails any Clippy run, not only the gated one.

```toml
[lints.clippy]
# Groups: namtao-rust, dont-panic, toolkit. clippy-stricter's warning against
# whole categories quotes the Clippy docs on `restriction`, which is not enabled.
pedantic = { level = "deny", priority = -1 }
nursery = { level = "deny", priority = -1 }

# Don't panic. In namtao-rust, dont-panic, toolkit, and clippy-stricter unless noted.
unwrap_used = "deny"
expect_used = "deny"                   # clippy-stricter: optional. Took namtao's deny.
indexing_slicing = "deny"
string_slice = "deny"
get_unwrap = "deny"                    # clippy-stricter only
unwrap_in_result = "deny"              # clippy-stricter only
panic = "deny"
panic_in_result_fn = "deny"
todo = "deny"
unimplemented = "deny"
unreachable = "deny"
unchecked_time_subtraction = "deny"
exit = "deny"                          # namtao set only
as_conversions = "deny"                # namtao set only
arithmetic_side_effects = "deny"       # clippy-stricter: optional, "85%" noise on one
                                       # codebase (anecdotal). Took namtao's deny.

# Don't fail silently: clippy-stricter
let_underscore_future = "deny"
let_underscore_must_use = "deny"
unused_result_ok = "deny"
map_err_ignore = "deny"
assertions_on_result_states = "deny"

# Async: clippy-stricter. The binary runs on tokio.
await_holding_lock = "deny"
await_holding_refcell_ref = "deny"
large_futures = "deny"

# Unsafe and memory: clippy-stricter. Tripwires; the crate writes no unsafe today.
mem_forget = "deny"
undocumented_unsafe_blocks = "deny"
multiple_unsafe_ops_per_block = "deny"
unnecessary_safety_doc = "deny"
unnecessary_safety_comment = "deny"

# Numbers: clippy-stricter. The three marked optional there are enabled; they fire
# only inside an `#[expect(clippy::as_conversions, reason = "...")]`.
float_cmp = "deny"
float_cmp_const = "deny"
lossy_float_literal = "deny"
cast_sign_loss = "deny"
cast_possible_wrap = "deny"
cast_precision_loss = "deny"
cast_possible_truncation = "deny"
invalid_upcast_comparisons = "deny"

# Easy to avoid: clippy-stricter
rc_mutex = "deny"
debug_assert_with_mut_call = "deny"
iter_not_returning_iterator = "deny"
expl_impl_clone_on_copy = "deny"
infallible_try_from = "deny"
dbg_macro = "deny"

# Every suppression is #[expect(..., reason = "...")], never a bare #[allow]: clippy-stricter
allow_attributes = "deny"
allow_attributes_without_reason = "deny"
```

At the workspace split, rename the table to `[workspace.lints.clippy]`. Every member crate then opts in with `lints.workspace = true`, because inheritance is not the default (clippy-stricter).

## clippy.toml (verbatim block)

The union of namtao-rust's four keys and clippy-stricter's five. `allow-dbg-in-tests` comes from clippy-stricter.

```toml
allow-unwrap-in-tests = true
allow-expect-in-tests = true
allow-panic-in-tests = true
allow-indexing-slicing-in-tests = true
allow-dbg-in-tests = true
```

No source gives a `rustfmt.toml`. None is adopted; rustfmt runs on defaults.

## Panics and errors (the rules, each cited)

| # | Rule | Source |
|---|---|---|
| 1 | No code path outside tests may panic. That covers `unwrap`, `expect`, `panic!`, `todo!`, `unimplemented!`, `unreachable!`, indexing, string slicing, `exit`, `as`, and unchecked arithmetic. The lint table enforces it. | dont-panic R5, namtao-rust, clippy-stricter |
| 2 | Replace `unwrap` with `Result` and `Option` combinators and the `?` operator. | dont-panic R1, R2; std-traits ("The most idiomatic way to propagate errors is to use the `?` operator") |
| 3 | Never write `is_ok()` followed by `unwrap()`. Use a combinator such as `unwrap_or_default()`. | dont-panic W8 |
| 4 | Panicking calls are allowed in tests only, through the clippy.toml keys. | dont-panic R8, namtao-rust, clippy-stricter |
| 5 | A lint is suppressed only per site, as `#[expect(lint, reason = "...")]`. A whole file or a bare `#[allow]` is not acceptable. dont-panic allows exemptions "if ... overly burdensome"; clippy-stricter's reasoned form is the one taken. | clippy-stricter; dont-panic R7 |
| 6 | Never drop an error silently. `let _ = fallible()`, `.ok();`, and `.map_err(\|_\| E)` are banned. | clippy-stricter |
| 7 | Never hold a `MutexGuard` or a `RefCell` borrow across `.await`. Never drop a future unawaited. | clippy-stricter |
| 8 | Errors are returned, not thrown. Keep their type: give the crate an error type implementing `Display` and `Error`, plus `From<Source>` impls so `?` converts. A stringified or boxed error throws away type information. | std-traits (Error, From) |
| 9 | Run the gate locally and in CI, so no teammate or agent can commit a panic path. | dont-panic R4, clippy-stricter |
| 10 | Allocation failure stays outside this guarantee. The target is "starting at 99%". | dont-panic R11 |

## Standard-library traits: what to derive and implement, and when

All rows come from std-traits (article dated 31 March 2021).

| Trait | Derive or implement when | Gotcha |
|---|---|---|
| Debug | Derive whenever all members are `Debug`. Output is for developers only. | `dbg!` is not stripped in release builds; the `dbg_macro` lint catches it. |
| Clone | Derive when an owned copy from `&T` is needed. | It "makes no promises about the efficiency" and is a common "escape hatch" from the borrow checker. |
| Copy | Derive together with `Clone`, for small bitwise-copyable types. It cannot be implemented by hand. | It changes move behavior. A manual `Clone` that disagrees with `Copy` is denied by `expl_impl_clone_on_copy`. |
| PartialEq / Eq | Derive when all members are `PartialEq`. Implement cross-type equality only between types holding "the same kind of data". | Hand impls must stay symmetric and transitive. Floats are `PartialEq` but not `Eq`. |
| Hash | Derive with `Eq`, or implement both by hand. Never mix the two. | `a == b` must imply `hash(a) == hash(b)`. |
| Default | Derive when all members are `Default`. | none stated |
| Display | Implement for anything an end user sees. | Never implement `ToString`; the blanket impl over `Display` provides it. |
| Error | Implement on the crate's error type, with `Display`. | Stringifying or boxing errors "throw[s] away type information". |
| From / TryFrom | Implement `From` for infallible conversions and `TryFrom` for fallible ones. | Never implement `Into` or `TryInto`; blanket impls provide them. `TryFrom` cannot coexist with `From`; `infallible_try_from` denies a `TryFrom` whose error is `Infallible`. `From` is not symmetric. |
| AsRef | Implement on a wrapper for cheap reference conversion between "semantically equivalent things". | Prefer it to `Deref`. `Deref` belongs only to smart pointers, since implicit coercion invites wrong expectations. |
| Iterator | Put iteration on a separate iterator type, not on the collection itself. | When the size is known, override `count`, `last`, `nth`, and implement `ExactSizeIterator`. Default impls call `next` repeatedly. `iter_not_returning_iterator` is denied. |
| Drop | Implement only to release an external resource on scope exit. | "We will rarely need to impl this". |

## Crates the sources recommend, and the verdict for this project

| Crate | Purpose | Source report | Verdict | Reason |
|---|---|---|---|---|
| serde (derive) | Serialization | toolkit, namtao-rust | adopt | The MCP surface exchanges structured messages; both sources carry it. See open question 1. |
| clap (derive) | CLI parsing | toolkit, namtao-rust | adopt (owner blessed, 2026-10-16) | The binary has a CLI surface; both sources prefer it over hand parsing. |
| color-eyre | Colored error reports | toolkit, namtao-rust | not today | Colored reports have no reader on an MCP stdio pipe. Rule 8's typed error needs no crate. |
| jiff | Datetimes | toolkit | not today | No datetime need yet; std time types first. If one comes, take jiff: toolkit's newer list demotes chrono to "the old master". |
| chrono | Datetimes | namtao-rust | never | Superseded by jiff in toolkit. |
| itertools | Iterator adaptors | toolkit, namtao-rust | not today | std iterators suffice. Toolkit's own erratum withdraws the zero-cost claim. |
| rayon | CPU parallelism | toolkit, namtao-rust | not today | The binary is I/O-bound on tokio. No CPU-parallel work exists. |
| criterion | Benchmarks | toolkit, namtao-rust | not today | No performance target exists to measure. |
| nanoserde | Zero-dep JSON and TOML derive | toolkit | not today | Honourable mention only. See open question 1. |
| no-panic | Prove a function cannot panic | dont-panic | not today | The lints cover the bootstrap. The crate "may produce false positives". |
| command-run | `Command` wrapper | toolkit | not today | The binary spawns no processes today. |
| reqwest | HTTP client | toolkit | not today | Networking goes through iroh. |
| sqlx | SQL | toolkit | never | No database. The spec forbids persisting the inbox (3.16). |
| utoipa | OpenAPI for REST | toolkit | never | No REST API; the surfaces are MCP over stdio and a CLI. |
| leptos, dioxus, tauri | Frontend and app UI | toolkit | never | No UI surface. |

## Not adopted

| Item | Source | Reason |
|---|---|---|
| Nightly toolchain | toolkit, namtao-rust | Its benefits are asserted. A pinned stable is required for a deny gate (clippy-stricter). |
| devenv, Nix, rust-overlay, `LD_LIBRARY_PATH` C libs | toolkit, namtao-rust | It brings Nix for isolation the pinned rustup toolchain already gives. "Without the overhead of containers" is asserted. |
| watchexec | toolkit, namtao-rust | It duplicates `bacon clippy`. |
| cargo-watch | toolkit | toolkit itself retires it for watchexec. |
| cargo-nextest | toolkit, namtao-rust | `cargo test` is built in. "Up to 3x faster" (asserted); "I dig" is opinion only. |
| cargo-shear | toolkit | No reason given there. A one-crate manifest with two adopted crates is read by eye. Revisit at the workspace split. |
| cargo-seek, cargo-generate | toolkit | A TUI over built-in commands; scaffolding for frameworks the project does not use. |
| Editor (LazyVim and others) | toolkit | Not a project decision. |
| One-liner `cargo clippy -- -D clippy::pedantic -D clippy::nursery` | namtao-rust | "80%" of the table (asserted). The table replaces it. |
| `if_let_mutex` | clippy-stricter | Irrelevant on edition 2024, per the same source (asserted). |
| Whole `restriction` category | clippy-stricter | The source quotes the Clippy docs against it. Its lints contradict each other (`big_endian_bytes` vs `little_endian_bytes`). |
| `missing_lints_inheritance` | clippy-stricter | Nightly-only, and there is no workspace yet. |
| Typestate pattern | namtao-rust | An API design choice, not a bootstrap item. The page's example is pseudocode. |

## Open questions the sources leave

| # | Question | Raised by |
|---|---|---|
| 1 | Which crate parses JSON for MCP? serde alone does not. No source names serde_json; toolkit names only nanoserde, as an honourable mention. | gap across toolkit, namtao-rust |
| 2 | Should the gate lint test targets? No source gives the flag. The clippy.toml test keys imply that tests are linted (extractor's inference). | clippy-stricter, namtao-rust |
| 3 | Should formatting be gated? namtao-rust installs rustfmt but no source gives a config or a check command. | namtao-rust |
| 4 | What happens on allocation failure? The panic ban does not cover it. | dont-panic R11 |
| 5 | Does `arithmetic_side_effects` at deny cost more than it catches here? The only figure is "85%" noise on one codebase. | clippy-stricter |
| 6 | Do the later lists differ? namtao.com/rust is named as the up-to-date list (dont-panic). The follow-up "Stricter-er" post was not read. | dont-panic, clippy-stricter |
| 7 | Is `no-panic`'s proof worth its false positives once the code exists? | dont-panic R9 |

## Confidence

| Claim | Status |
|---|---|
| Every quote and config block | Taken from raw HTML or raw markdown fetched with curl by each extractor. Model-written WebFetch summaries were discarded. |
| The 13-lint deny set | Identical in namtao-rust, dont-panic, and toolkit, three independent extractions of two pages. |
| Lint names, and their existence at 1.95 | Not checked against Clippy's lint list by any extractor. Verify on the first gate run. |
| Test exemptions ("clippy ignores those") | Asserted in dont-panic and namtao-rust; not checked against the Clippy docs. |
| Clippy-docs quotes against whole categories | Attributed by clippy-stricter; the docs were not fetched. |
| `if_let_mutex` fixed in edition 2024 | Asserted, uncited (clippy-stricter). |
| Unpinned deny breaks on new lints | Credited to a commenter, untested (clippy-stricter). |
| Nightly speed, nextest 3x, devenv isolation, jiff "pit of success" | Asserted (toolkit). No claim there was checked outside the page. |
| clippy-stricter authorship (Evan Schwartz) | Asserted; the raw page has no byline. Page date: 30 Apr 2026. |
| std-traits rules | Quoted from the full 5201-line article. Code was not compiled. The article dates from 2021. |
| Pin mechanism `rust-toolchain.toml`, verdict reasons tied to this project | This document's own synthesis, from blueprint 3.2 and 3.16. No source carries them. |
