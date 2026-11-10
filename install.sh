#!/usr/bin/env bash
# A.R.I.A interactive installer.
#
#   ./install.sh          ask before every step
#   ./install.sh --yes    accept every default, ask nothing
#
# It builds the `aria` binary with cargo and links it into a directory on your
# PATH. Nothing is deleted: an existing install is moved to a backup directory.

set -euo pipefail

REPO_URL="${ARIA_REPO:-https://github.com/Shishir-Kc/A.R.I.A}"
DEFAULT_INSTALL_DIR="$HOME/.A.R.I.A"
DEFAULT_BIN_DIR="$HOME/.local/bin"
DEFAULT_NAME="aria"

ASSUME_YES=0
for arg in "$@"; do
    case "$arg" in
        -y | --yes) ASSUME_YES=1 ;;
        -h | --help)
            sed -n '2,8s/^# \{0,1\}//p' "$0"
            exit 0
            ;;
        *)
            echo "unknown option: $arg (try --help)" >&2
            exit 2
            ;;
    esac
done

if [ -t 1 ]; then
    BOLD=$'\033[1m' DIM=$'\033[2m' RED=$'\033[31m' GREEN=$'\033[32m' YELLOW=$'\033[33m' CYAN=$'\033[36m' RESET=$'\033[0m'
else
    BOLD="" DIM="" RED="" GREEN="" YELLOW="" CYAN="" RESET=""
fi

step() { printf '\n%s==>%s %s%s%s\n' "$CYAN" "$RESET" "$BOLD" "$*" "$RESET"; }
info() { printf '    %s\n' "$*"; }
ok() { printf '    %s✓%s %s\n' "$GREEN" "$RESET" "$*"; }
warn() { printf '    %s!%s %s\n' "$YELLOW" "$RESET" "$*"; }
die() {
    printf '\n%serror:%s %s\n' "$RED" "$RESET" "$*" >&2
    exit 1
}

# Where answers come from: the terminal, even when the script itself is piped
# in (curl ... | bash); plain stdin when answers are piped to the script.
INPUT=/dev/stdin
if [ ! -t 0 ] && [ -z "${BASH_SOURCE[0]:-}" ] && [ -r /dev/tty ]; then
    INPUT=/dev/tty
fi

# ask <question> <default>  -> answer in $REPLY
ask() {
    local question=$1 default=$2
    if [ "$ASSUME_YES" = 1 ]; then
        REPLY=$default
        return
    fi
    printf '%s?%s %s %s[%s]%s ' "$GREEN" "$RESET" "$question" "$DIM" "$default" "$RESET"
    IFS= read -r REPLY <"$INPUT" || die "no input available; run with --yes to accept the defaults"
    REPLY=${REPLY:-$default}
}

# confirm <question> <y|n default>
confirm() {
    local hint="y/N"
    [ "$2" = y ] && hint="Y/n"
    while true; do
        ask "$1 ($hint)" "$2"
        case "${REPLY,,}" in
            y | yes) return 0 ;;
            n | no) return 1 ;;
            *) warn "please answer y or n" ;;
        esac
    done
}

expand_path() {
    local path=$1
    # shellcheck disable=SC2088  # matching a literal "~/" typed at the prompt
    case "$path" in
        "~") path=$HOME ;;
        "~/"*) path="$HOME/${path#"~/"}" ;;
    esac
    case "$path" in
        /*) ;;
        *) path="$PWD/$path" ;;
    esac
    printf '%s' "${path%/}"
}

printf '%s\n' "${BOLD}A.R.I.A installer${RESET}"
info "AI-powered home server, built from source with cargo."

# ---------------------------------------------------------------- requirements
step "Checking requirements"
missing=0
for tool in git cargo cc; do
    if command -v "$tool" >/dev/null 2>&1; then
        ok "$tool"
    else
        warn "$tool is not installed"
        missing=1
    fi
done
if [ "$missing" = 1 ]; then
    info "git and a C compiler come from your package manager (e.g. pacman -S git base-devel)."
    info "cargo comes with Rust: https://rustup.rs"
    die "install the missing tools and run the installer again"
fi

# ---------------------------------------------------------------------- source
SCRIPT_DIR=""
if [ -n "${BASH_SOURCE[0]:-}" ]; then
    SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
fi
HAVE_CHECKOUT=0
if [ -n "$SCRIPT_DIR" ] && grep -qs '^name = "aria"' "$SCRIPT_DIR/Cargo.toml"; then
    HAVE_CHECKOUT=1
fi

step "Where to install from"
MODE=clone
if [ "$HAVE_CHECKOUT" = 1 ]; then
    info "1) Download the latest release from GitHub  (normal install, supports 'aria update')"
    info "2) Use this checkout: $SCRIPT_DIR  (for development)"
    while true; do
        ask "Choose 1 or 2" "1"
        case "$REPLY" in
            1) break ;;
            2)
                MODE=local
                break
                ;;
            *) warn "please answer 1 or 2" ;;
        esac
    done
else
    info "Downloading from $REPO_URL"
fi

# ----------------------------------------------------------------- install dir
EXISTING=none
if [ "$MODE" = local ]; then
    INSTALL_DIR=$SCRIPT_DIR
else
    step "Install directory"
    info "'aria update' expects the default location."
    ask "Install A.R.I.A into" "$DEFAULT_INSTALL_DIR"
    INSTALL_DIR=$(expand_path "$REPLY")

    if [ -e "$INSTALL_DIR" ] && [ -n "$(ls -A "$INSTALL_DIR" 2>/dev/null)" ]; then
        if [ -f "$INSTALL_DIR/Cargo.toml" ] && [ -d "$INSTALL_DIR/.git" ]; then
            warn "$INSTALL_DIR already holds a Rust install of A.R.I.A"
            info "1) Update it in place (git pull, then rebuild)"
            info "2) Back it up and install fresh"
            info "3) Cancel"
            default_choice=1
        else
            warn "$INSTALL_DIR already exists (an older or different install)"
            info "2) Back it up and install fresh"
            info "3) Cancel"
            default_choice=2
        fi
        while true; do
            ask "Choose" "$default_choice"
            case "$REPLY" in
                1) if [ "$default_choice" = 1 ]; then
                    EXISTING=update
                    break
                fi ;;
                2)
                    EXISTING=backup
                    break
                    ;;
                3) die "cancelled, nothing was changed" ;;
            esac
            warn "please pick one of the listed numbers"
        done
    fi
fi

# ---------------------------------------------------------- command name + dir
step "Command"
while true; do
    ask "Command name" "$DEFAULT_NAME"
    NAME=$REPLY
    case "$NAME" in
        "" | -* | *[!A-Za-z0-9._-]*)
            warn "use only letters, digits, '.', '_' and '-'"
            [ "$ASSUME_YES" = 1 ] && die "invalid command name '$NAME'"
            ;;
        *) break ;;
    esac
done

ask "Directory to put '$NAME' in" "$DEFAULT_BIN_DIR"
BIN_DIR=$(expand_path "$REPLY")
TARGET="$BIN_DIR/$NAME"
BINARY="$INSTALL_DIR/target/release/aria"

# Anything else on PATH with this name would be shadowed (or shadow us).
clash=""
IFS=: read -r -a path_dirs <<<"$PATH"
for dir in "${path_dirs[@]}"; do
    [ -n "$dir" ] || continue
    [ "$(expand_path "$dir")" = "$BIN_DIR" ] && continue
    if [ -x "$dir/$NAME" ] && [ ! -d "$dir/$NAME" ]; then
        clash="$dir/$NAME"
        break
    fi
done
if [ -n "$clash" ]; then
    warn "another '$NAME' already exists: $clash"
    info "Programs that call '$NAME' may get the wrong one."
    confirm "Use the name '$NAME' anyway?" n || die "cancelled, nothing was changed; run again and pick another name"
fi

REPLACE_TARGET=0
if [ -e "$TARGET" ] || [ -L "$TARGET" ]; then
    if [ -L "$TARGET" ] && [ "$(readlink "$TARGET")" = "$BINARY" ]; then
        ok "$TARGET already points at this install"
    else
        warn "$TARGET already exists"
        confirm "Replace it? (the old one is kept as $NAME.bak)" y || die "cancelled, nothing was changed"
        REPLACE_TARGET=1
    fi
fi

# --------------------------------------------------------------------- summary
step "Summary"
case "$MODE:$EXISTING" in
    local:*) info "source   this checkout ($INSTALL_DIR)" ;;
    clone:update) info "source   update existing install in $INSTALL_DIR" ;;
    clone:backup) info "source   $REPO_URL -> $INSTALL_DIR (existing one backed up)" ;;
    *) info "source   $REPO_URL -> $INSTALL_DIR" ;;
esac
info "build    cargo build --release"
info "command  $TARGET -> $BINARY"
confirm "Proceed?" y || die "cancelled, nothing was changed"

# --------------------------------------------------------------------- install
if [ "$MODE" = clone ]; then
    case "$EXISTING" in
        update)
            step "Updating $INSTALL_DIR"
            git -C "$INSTALL_DIR" pull --ff-only
            ;;
        *)
            if [ "$EXISTING" = backup ]; then
                BACKUP="$INSTALL_DIR.bak-$(date +%Y%m%d-%H%M%S)"
                step "Backing up the existing install"
                mv "$INSTALL_DIR" "$BACKUP"
                ok "moved to $BACKUP"
            fi
            step "Downloading A.R.I.A"
            mkdir -p "$(dirname "$INSTALL_DIR")"
            git clone --depth 1 "$REPO_URL" "$INSTALL_DIR"
            ;;
    esac
fi

step "Building (the first build takes a minute or two)"
(cd "$INSTALL_DIR" && cargo build --release)
[ -x "$BINARY" ] || die "the build finished but $BINARY is missing"
ok "built $BINARY"

step "Installing the '$NAME' command"
mkdir -p "$BIN_DIR"
if [ "$REPLACE_TARGET" = 1 ]; then
    mv -f "$TARGET" "$TARGET.bak"
    ok "old command kept as $TARGET.bak"
fi
ln -sfn "$BINARY" "$TARGET"
ok "$TARGET"

case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *)
        warn "$BIN_DIR is not on your PATH"
        info "Add this line to your shell config (~/.bashrc, ~/.zshrc, ...):"
        info "    export PATH=\"$BIN_DIR:\$PATH\""
        ;;
esac

# ---------------------------------------------------------------- model config
step "Model config (optional)"
info "'$NAME chat' needs a model config. It can be downloaded later with '$NAME model make'."
if confirm "Download the default model config now?" n; then
    "$BINARY" model make || warn "could not download it; try '$NAME model make' later"
fi

step "Done"
ok "A.R.I.A $("$BINARY" version 2>/dev/null || echo "") is installed"
info "Try:  $NAME info"
info "      $NAME --help"
