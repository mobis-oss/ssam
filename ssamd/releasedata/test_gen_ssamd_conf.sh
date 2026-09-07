#!/bin/bash
# Copyright 2025-2026 Hyundai Mobis Co., Ltd.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GENERATOR="${SCRIPT_DIR}/gen_ssamd_conf.sh"
OUTPUT_ROOT="$(mktemp -d)"
trap 'rm -rf "$OUTPUT_ROOT"' EXIT

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

assert_file_readable() {
    local file="$1"
    [[ -f "$file" ]] || fail "${file} does not exist or is not a regular file"
    [[ -r "$file" ]] || fail "${file} is not readable"
}

assert_contains() {
    local file="$1"
    local text="$2"
    local status
    assert_file_readable "$file"
    if rg -F -q -- "$text" "$file"; then
        return 0
    else
        status=$?
    fi
    if [[ "$status" -eq 1 ]]; then
        fail "${file} lacks: ${text}"
    fi
    fail "Failed to search ${file} (rg exit status: ${status})"
}

assert_not_contains() {
    local file="$1"
    local text="$2"
    local status
    assert_file_readable "$file"
    if rg -F -q -- "$text" "$file"; then
        fail "${file} contains forbidden text: ${text}"
    else
        status=$?
    fi
    if [[ "$status" -ne 1 ]]; then
        fail "Failed to search ${file} (rg exit status: ${status})"
    fi
}

assert_no_placeholders() {
    local file="$1"
    local status
    assert_file_readable "$file"
    if rg -q '@[A-Z_]+@' "$file"; then
        fail "${file} contains unresolved placeholder"
    else
        status=$?
    fi
    if [[ "$status" -ne 1 ]]; then
        fail "Failed to search ${file} (rg exit status: ${status})"
    fi
}

verify_apparmor_syntax() {
    local file="$1"
    local parser_output

    command -v apparmor_parser >/dev/null 2>&1 || return 0
    assert_file_readable "$file"
    if parser_output=$(apparmor_parser -Q "$file" 2>&1); then
        return 0
    fi
    fail "AppArmor parser rejected ${file}: ${parser_output}"
}

run_generator() {
    local name="$1"
    shift
    local output="${OUTPUT_ROOT}/${name}"
    mkdir -p "$output"
    "$GENERATOR" \
        --output-path "$output" \
        --ssamd-install-path /usr/bin/ssamd \
        --oci-bundle-dir /tmp/.ssamd \
        --pkgs-data-root /var/lib/ssamd/data \
        --builtin-pkgs-dir /var/lib/ssamd/builtin \
        --downloaded-pkgs-dir /var/lib/ssamd/downloaded \
        "$@" >/dev/null
    echo "$output"
}

assert_generator_fails() {
    local name="$1"
    shift
    if "$GENERATOR" \
        --output-path "${OUTPUT_ROOT}/${name}" \
        "$@" >/dev/null 2>&1; then
        fail "generator accepted invalid input: ${name}"
    fi
}

assert_common_outputs() {
    local output="$1"
    assert_contains "${output}/ssamd.toml" '[package]'
    assert_contains "${output}/ssamd.toml" '[package.dir]'
    assert_contains "${output}/ssamd.toml" 'oci_bundle = '
    assert_not_contains "${output}/ssamd.toml" '[common]'
    assert_not_contains "${output}/ssamd.toml" '[integrity]'
    assert_not_contains "${output}/ssamd.toml" 'disable_runtime_integrity'
    assert_no_placeholders "${output}/ssamd.toml"
}

full_output="$(run_generator full \
    --gen-service-unit \
    --gen-apparmor-profile \
    --ssamd-install-path /usr/bin/ssamd \
    --pkgs-overlayfs-root /var/run/ssamd/overlay \
    --pkgs-cgroup ssamd.slice \
    --public-key-file /etc/ssamd/public.key \
    --packages-ext ssam \
    --disable-runtime-integrity false \
    --disable-package-verify false)"
assert_common_outputs "$full_output"
assert_contains "${full_output}/ssamd.toml" 'builtin = "/var/lib/ssamd/builtin"'
assert_contains "${full_output}/ssamd.toml" 'downloaded = "/var/lib/ssamd/downloaded"'
assert_contains "${full_output}/ssamd.toml" 'public_key = "/etc/ssamd/public.key"'
assert_contains "${full_output}/ssamd.toml" 'extension = "ssam"'
assert_contains "${full_output}/ssamd.toml" 'cgroup = "ssamd.slice"'
assert_contains "${full_output}/ssamd.toml" 'pkgfs_overlay_dir = "/var/run/ssamd/overlay"'
assert_contains "${full_output}/ssamd.toml" 'data = "/var/lib/ssamd/data"'
assert_contains "${full_output}/ssamd.toml" 'disable_package_verify = false'
assert_contains "${full_output}/ssamd.toml" 'disable_pkgfs_integrity = false'
assert_contains "${full_output}/ssamd.service" \
    'RequiresMountsFor=/tmp /var/lib/ssamd/builtin /var/lib/ssamd/downloaded /var/lib/ssamd/data /tmp/.ssamd'
assert_contains "${full_output}/usr.bin.ssamd" 'mount -> /tmp/.ssamd/*/pkgfs/'
assert_contains "${full_output}/usr.bin.ssamd" 'umount /tmp/.ssamd/*/pkgfs/'
assert_no_placeholders "${full_output}/ssamd.service"
assert_no_placeholders "${full_output}/usr.bin.ssamd"
verify_apparmor_syntax "${full_output}/usr.bin.ssamd"

custom_bundle="$(run_generator custom-bundle \
    --oci-bundle-dir /var/run/custom-bundle \
    --gen-service-unit \
    --gen-apparmor-profile \
    --ssamd-install-path /usr/bin/ssamd)"
assert_contains "${custom_bundle}/ssamd.toml" \
    'oci_bundle = "/var/run/custom-bundle"'
assert_contains "${custom_bundle}/ssamd.service" \
    'RequiresMountsFor=/tmp /var/lib/ssamd/builtin /var/lib/ssamd/downloaded /var/lib/ssamd/data /var/run/custom-bundle'
assert_contains "${custom_bundle}/usr.bin.ssamd" \
    'mount -> @{run}custom-bundle/*/pkgfs/,'
assert_contains "${custom_bundle}/usr.bin.ssamd" \
    'umount @{run}custom-bundle/*/pkgfs/,'
assert_contains "${custom_bundle}/usr.bin.ssamd" \
    '@{run}{custom-bundle/} w,'
assert_contains "${custom_bundle}/usr.bin.ssamd" \
    '@{run}custom-bundle/** rwk,'
assert_no_placeholders "${custom_bundle}/ssamd.toml"
assert_no_placeholders "${custom_bundle}/ssamd.service"
assert_no_placeholders "${custom_bundle}/usr.bin.ssamd"
verify_apparmor_syntax "${custom_bundle}/usr.bin.ssamd"

builtin_only="$(run_generator builtin-only --downloaded-pkgs-dir "")"
assert_contains "${builtin_only}/ssamd.toml" 'downloaded = ""'
assert_not_contains "${builtin_only}/ssamd.toml" '[debug]'

downloaded_only="$(run_generator downloaded-only --builtin-pkgs-dir "" --gen-service-unit)"
assert_contains "${downloaded_only}/ssamd.toml" 'builtin = ""'
assert_contains "${downloaded_only}/ssamd.service" \
    'RequiresMountsFor=/tmp /var/lib/ssamd/downloaded /var/lib/ssamd/data /tmp/.ssamd'

omitted_optional="${OUTPUT_ROOT}/omitted-optional"
mkdir -p "$omitted_optional"
"$GENERATOR" \
    --output-path "$omitted_optional" \
    --oci-bundle-dir /tmp/.ssamd \
    --pkgs-data-root /var/lib/ssamd/data \
    --builtin-pkgs-dir /var/lib/ssamd/builtin >/dev/null
assert_contains "${omitted_optional}/ssamd.toml" '[package.dir]'
assert_not_contains "${omitted_optional}/ssamd.toml" 'extension = '
assert_not_contains "${omitted_optional}/ssamd.toml" 'public_key = '
assert_not_contains "${omitted_optional}/ssamd.toml" '[debug]'
assert_not_contains "${omitted_optional}/ssamd.toml" '[network.bridge]'
assert_not_contains "${omitted_optional}/ssamd.toml" '[rpc]'

verify_disabled="$(run_generator verify-disabled \
    --public-key-file "" \
    --disable-package-verify true)"
assert_contains "${verify_disabled}/ssamd.toml" 'public_key = ""'
assert_contains "${verify_disabled}/ssamd.toml" 'disable_package_verify = true'

common_generator_args=(
    --oci-bundle-dir /tmp/.ssamd
    --pkgs-data-root /var/lib/ssamd/data
    --builtin-pkgs-dir /var/lib/ssamd/builtin
    --downloaded-pkgs-dir /var/lib/ssamd/downloaded
)

network_output="$(run_generator network \
    --bridge-enabled true \
    --bridge-pool-base 10.10.0.0/16 \
    --bridge-pool-size 28)"
assert_contains "${network_output}/ssamd.toml" '[network.bridge]'
assert_contains "${network_output}/ssamd.toml" 'enabled = true'
assert_contains "${network_output}/ssamd.toml" '[network.bridge.addr_pool]'
assert_contains "${network_output}/ssamd.toml" 'base = "10.10.0.0/16"'
assert_contains "${network_output}/ssamd.toml" 'size = 28'
assert_generator_fails network-toml-injection \
    "${common_generator_args[@]}" \
    --bridge-enabled true \
    --bridge-pool-base '172.20.0.0/16"; extra = 1 #' \
    --bridge-pool-size 29

rpc_output="$(run_generator rpc --rpc-allowed-gids 0,4294967295)"
assert_contains "${rpc_output}/ssamd.toml" 'allowed_gids = [0, 4294967295]'

for invalid_rpc in "1000,,1001" "1000," ",1000" "-1" "4294967296" "abc" "1000, 1001" "[1000]"; do
    assert_generator_fails "invalid-rpc-${invalid_rpc}" \
        "${common_generator_args[@]}" \
        --rpc-allowed-gids "$invalid_rpc"
done
assert_generator_fails repeated-rpc \
    "${common_generator_args[@]}" \
    --rpc-allowed-gids 1000 \
    --rpc-allowed-gids 1001

for path_option in \
    --ssamd-install-path \
    --oci-bundle-dir \
    --builtin-pkgs-dir \
    --downloaded-pkgs-dir \
    --pkgs-data-root \
    --pkgs-overlayfs-root \
    --public-key-file; do
    assert_generator_fails "relative-${path_option#--}" \
        "${common_generator_args[@]}" \
        "$path_option" relative/path
done

if "$GENERATOR" \
    --output-path "" \
    "${common_generator_args[@]}" >/dev/null 2>&1; then
    fail "empty output path must fail"
fi

assert_generator_fails same-package-source \
    --oci-bundle-dir /tmp/.ssamd \
    --pkgs-data-root /var/lib/ssamd/data \
    --builtin-pkgs-dir /var/lib/ssamd/packages \
    --downloaded-pkgs-dir /var/lib/ssamd/packages

if "$GENERATOR" \
    --output-path "${OUTPUT_ROOT}/invalid-key" \
    --oci-bundle-dir /tmp/.ssamd \
    --pkgs-data-root /var/lib/ssamd/data \
    --builtin-pkgs-dir /var/lib/ssamd/builtin \
    --public-key-file "" >/dev/null 2>&1; then
    fail "empty public key must fail when verification is enabled"
fi

if "$GENERATOR" \
    --output-path "${OUTPUT_ROOT}/missing-bundle" \
    --builtin-pkgs-dir /var/lib/ssamd/builtin >/dev/null 2>&1; then
    fail "missing OCI bundle directory must fail"
fi

if "$GENERATOR" \
    --output-path "${OUTPUT_ROOT}/missing-source" \
    --oci-bundle-dir /tmp/.ssamd \
    --pkgs-data-root /var/lib/ssamd/data >/dev/null 2>&1; then
    fail "missing package source must fail"
fi

echo "PASS: generator contract tests"
