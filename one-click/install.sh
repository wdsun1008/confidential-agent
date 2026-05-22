#!/bin/sh
set -eu

DEFAULT_REPO="https://github.com/wdsun1008/confidential-agent.git"
DEFAULT_BRANCH="one-click"

usage() {
    cat <<'EOF'
Usage:
  curl -fsSL https://raw.githubusercontent.com/wdsun1008/confidential-agent/one-click/one-click/install.sh | sh
  sh one-click/install.sh [deploy-openclaw|install-only|cleanup] [options]

Bootstrap options:
  --repo URL             Git repository to clone when running through curl | sh
  --branch NAME          Git branch to checkout when running through curl | sh
  --source-dir PATH      Local source checkout directory
  --help                 Show this help

All other options are handled by the one-click installer after checkout.
EOF
}

append_pass_arg() {
    printf '%s\0' "$1" >>"$pass_args_file"
}

run_main() {
    main_script="$1"
    if [ -s "$pass_args_file" ]; then
        xargs -0 -a "$pass_args_file" bash "$main_script"
        exit $?
    fi
    exec bash "$main_script"
}

ensure_git() {
    if command -v git >/dev/null 2>&1; then
        return 0
    fi
    if [ "$(id -u)" != "0" ]; then
        echo "git is required. Re-run as root or install git first." >&2
        exit 2
    fi
    if command -v dnf >/dev/null 2>&1; then
        dnf install -y git ca-certificates
    elif command -v yum >/dev/null 2>&1; then
        yum install -y git ca-certificates
    else
        echo "git is required and no yum/dnf package manager was found." >&2
        exit 2
    fi
}

# Wrap the imperative body so `sh` parses the entire script before executing
# anything. Without this wrapper, `curl ... | sh` would hit `exec </dev/tty`
# while the rest of the script bytes are still in the curl pipe, then sh would
# try to read the remaining script from the terminal and silently hang.
main() {
    repo="${CA_ONE_CLICK_REPO:-$DEFAULT_REPO}"
    branch="${CA_ONE_CLICK_BRANCH:-$DEFAULT_BRANCH}"
    source_dir="${CA_ONE_CLICK_SOURCE_DIR:-${HOME:-/root}/.cache/confidential-agent/source}"
    pass_args_file="$(mktemp "${TMPDIR:-/tmp}/ca-one-click-args.XXXXXX")"
    trap 'rm -f "$pass_args_file"' EXIT HUP INT TERM

    if [ ! -t 0 ] && ( : </dev/tty ) 2>/dev/null; then
        exec </dev/tty
    fi

    while [ "$#" -gt 0 ]; do
        case "$1" in
            --repo)
                [ "$#" -ge 2 ] || { echo "missing value for --repo" >&2; exit 2; }
                repo="$2"
                shift 2
                ;;
            --branch)
                [ "$#" -ge 2 ] || { echo "missing value for --branch" >&2; exit 2; }
                branch="$2"
                shift 2
                ;;
            --source-dir)
                [ "$#" -ge 2 ] || { echo "missing value for --source-dir" >&2; exit 2; }
                source_dir="$2"
                shift 2
                ;;
            --help|-h)
                usage
                exit 0
                ;;
            *)
                append_pass_arg "$1"
                shift
                ;;
        esac
    done

    script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" 2>/dev/null && pwd -P 2>/dev/null || pwd)
    local_root=$(CDPATH= cd -- "$script_dir/.." 2>/dev/null && pwd -P 2>/dev/null || true)
    if [ -n "$local_root" ] && [ -f "$local_root/Cargo.toml" ] && [ -f "$local_root/one-click/lib/main.sh" ]; then
        run_main "$local_root/one-click/lib/main.sh"
    fi

    ensure_git
    mkdir -p "$(dirname "$source_dir")"

    if [ -d "$source_dir/.git" ]; then
        git -C "$source_dir" fetch --depth 1 origin "$branch"
        git -C "$source_dir" checkout -B "$branch" "FETCH_HEAD"
    else
        rm -rf "$source_dir"
        git clone --depth 1 --branch "$branch" "$repo" "$source_dir"
    fi

    if [ ! -f "$source_dir/one-click/lib/main.sh" ]; then
        echo "one-click installer not found in checkout: $source_dir" >&2
        exit 1
    fi

    run_main "$source_dir/one-click/lib/main.sh"
}

main "$@"
