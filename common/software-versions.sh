# Shared exact defaults. This file is also staged into every CA-built guest.
ca_software_version() {
    local version_file="${CA_SOFTWARE_VERSIONS_FILE:-}" value
    if [[ -z "$version_file" ]]; then
        if [[ -f /usr/local/share/confidential-agent/software-versions.json ]]; then
            version_file=/usr/local/share/confidential-agent/software-versions.json
        else
            version_file="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/software-versions.json"
        fi
    fi
    value="$(awk -F '"' -v key="$1" '$2 == key {print $4; exit}' "$version_file")" || return
    [[ -n "$value" && "$value" != latest ]] || { printf 'missing exact software pin: %s\n' "$1" >&2; return 1; }
    printf '%s\n' "$value"
}
