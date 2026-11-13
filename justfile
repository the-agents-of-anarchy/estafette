# Root: a thin index. Every task lives in one module under just/; `just --list --list-submodules` shows all.

set shell := ["bash", "-euo", "pipefail", "-c"]

mod gate 'just/gate.just'
mod records 'just/records.just'
mod remote 'just/remote.just'
mod spec 'just/spec.just'
mod dev 'just/dev.just'
mod awake 'just/awake.just'
mod usage 'just/usage.just'

# `just records` runs records::check, the [default] recipe of that module.

alias check := gate::check
alias check-light := gate::check-light
alias check-heavy := gate::check-heavy

[doc('List every recipe of every module')]
[private]
default:
    @just --list --list-submodules
