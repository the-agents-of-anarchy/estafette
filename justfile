# Root: a thin index. Every task lives in one module under just/; `just --list --list-submodules` shows all.

set shell := ["bash", "-euo", "pipefail", "-c"]

mod ci 'just/ci.just'
mod gate 'just/gate.just'
mod records 'just/records.just'
mod remote 'just/remote.just'
mod spec 'just/spec.just'
mod dev 'just/dev.just'

# `just records` runs records::check, the [default] recipe of that module.
# `just check` is the one gate that CI, pre-commit and every commit call.

alias check := gate::check

[doc('List every recipe of every module')]
[private]
default:
    @just --list --list-submodules
