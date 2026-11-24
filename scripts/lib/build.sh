# SPDX-License-Identifier: GPL-3.0-or-later
# Sourced by scripts/build-linux.sh and scripts/build-macos.sh (from the repository
# root): the build-dependency checks they share. Each check appends what is missing to
# PROBLEMS; report_problems then prints the whole list at once, so one run names
# everything to install instead of failing on the first thing.

PROBLEMS=()
problem() { PROBLEMS+=("$*"); }

info() { echo "==> $*"; }
warn() { echo "warning: $*" >&2; }

# report_problems HINT...: when anything is missing, print it and the HINT lines, then
# exit 1.
report_problems() {
    (( ${#PROBLEMS[@]} )) || return 0
    {
        echo "Missing build dependencies:"
        printf '  - %s\n' "${PROBLEMS[@]}"
        (( $# )) && printf '%s\n' "" "$@"
    } >&2
    exit 1
}

# The workspace's rust-version (Cargo.toml).
msrv() { sed -n 's/^rust-version = "\(.*\)"/\1/p' Cargo.toml | head -1; }

# check_rust [TARGET]: cargo and a rustc at least the MSRV, with TARGET's standard
# library installed. Distribution Rust is often older (Debian 13 ships 1.85).
check_rust() {
    local target=${1:-} need have libdir
    need=$(msrv)
    if ! command -v cargo >/dev/null || ! command -v rustc >/dev/null; then
        problem "Rust (cargo and rustc): install it with rustup, https://rustup.rs"
        return
    fi
    have=$(rustc --version | awk '{print $2}')
    if [[ $(printf '%s\n%s\n' "$need" "${have%%-*}" | sort -V | head -1) != "$need" ]]; then
        problem "Rust $need or newer (rustc is $have; $(command -v rustc)): rustup update, or install rustup (https://rustup.rs) in place of the distribution's Rust"
        return
    fi
    if [[ -n $target ]]; then
        libdir=$(rustc --print target-libdir --target "$target" 2>/dev/null || true)
        if [[ -z $libdir || ! -d $libdir ]]; then
            problem "the Rust standard library for $target: rustup target add $target"
        fi
    fi
}

# check_modules PKG_CONFIG "MODULE [MIN]"...: each pkg-config module present, at least
# MIN. Reports the module and, when too old, the version found.
check_modules() {
    local pc=$1 spec mod min have
    shift
    for spec in "$@"; do
        read -r mod min <<< "$spec"
        if ! have=$("$pc" --modversion "$mod" 2>/dev/null); then
            problem "$mod${min:+ >= $min} (pkg-config finds no $mod.pc)"
        elif [[ -n $min ]] && ! "$pc" --atleast-version="$min" "$mod"; then
            problem "$mod >= $min (found $have)"
        fi
    done
}

# check_component SUBCOMMAND COMPONENT: `cargo SUBCOMMAND` runs (a rustup component of
# the active toolchain, such as clippy or rustfmt).
check_component() {
    command -v cargo >/dev/null || return 0   # check_rust reports it
    cargo "$1" --version >/dev/null 2>&1 || problem "cargo $1: rustup component add $2"
}

# check_llvm_cov: cargo-llvm-cov and the LLVM tools it runs, from the llvm-tools rustup
# component (where cargo-llvm-cov looks) unless LLVM_COV and LLVM_PROFDATA name them.
# Callers run it with CARGO_LLVM_COV_SETUP=no, so it never installs the component.
check_llvm_cov() {
    local sysroot host
    check_tool_version cargo-llvm-cov 0.8.7 "cargo install --locked cargo-llvm-cov --version 0.8.7"
    if [[ -n ${LLVM_COV:-} && -n ${LLVM_PROFDATA:-} ]]; then
        # cargo-llvm-cov uses them unchecked, and would fail only after the tests.
        command -v "$LLVM_COV" >/dev/null || problem "LLVM_COV=$LLVM_COV: not an executable"
        command -v "$LLVM_PROFDATA" >/dev/null || problem "LLVM_PROFDATA=$LLVM_PROFDATA: not an executable"
        return 0
    fi
    command -v rustc >/dev/null || return 0   # check_rust reports it
    sysroot=$(rustc --print sysroot)
    host=$(rustc -vV | sed -n 's/^host: //p')
    [[ -x $sysroot/lib/rustlib/$host/bin/llvm-cov ]] \
        || problem "LLVM's coverage tools: rustup component add llvm-tools-preview (or set LLVM_COV and LLVM_PROFDATA)"
}

# check_cmd CMD WHAT: CMD on PATH, else report WHAT (what provides it).
check_cmd() { command -v "$1" >/dev/null || problem "$1: $2"; }

# check_tool_version CMD VERSION INSTALL: CMD (a cargo subcommand binary) installed;
# warn when it isn't the release workflow's VERSION, as its output may differ.
check_tool_version() {
    local cmd=$1 want=$2 install=$3 have
    if ! command -v "$cmd" >/dev/null; then
        problem "$cmd $want: $install"
        return
    fi
    have=$(cargo "${cmd#cargo-}" --version 2>/dev/null | awk '{print $NF}')
    [[ $have == "$want" ]] || warn "$cmd $have, not $want (the release workflow's); its output may differ"
}
