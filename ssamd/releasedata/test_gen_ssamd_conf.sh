#!/bin/bash
# Copyright 2025-2026 Hyundai Mobis Co., Ltd.
# SPDX-License-Identifier: Apache-2.0

# Test script for gen_ssamd_conf.sh
# This script validates the generation of AppArmor profiles, runtime configurations,
# and systemd service units.

set -e

# Color codes for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Test counter
TESTS_TOTAL=0
TESTS_PASSED=0
TESTS_FAILED=0

# Get script directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GEN_CONFIG_SCRIPT="${SCRIPT_DIR}/gen_ssamd_conf.sh"
TEST_OUTPUT_DIR="${SCRIPT_DIR}/tmp/test_output"

# Function to print test header
print_test_header() {
    echo ""
    echo "=========================================="
    echo "TEST: $1"
    echo "=========================================="
}

# Function to print test result
print_result() {
    local test_name="$1"
    local result="$2"

    TESTS_TOTAL=$((TESTS_TOTAL + 1))

    if [[ "$result" == "PASS" ]]; then
        echo -e "${GREEN}✓ PASS${NC}: $test_name"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        echo -e "${RED}✗ FAIL${NC}: $test_name"
        TESTS_FAILED=$((TESTS_FAILED + 1))
    fi
}

# Function to verify file content
verify_file_content() {
    local file="$1"
    local expected_pattern="$2"
    local description="$3"

    if [[ ! -f "$file" ]]; then
        print_result "$description - File exists" "FAIL"
        echo "  Error: File not found: $file"
        return 1
    fi

    # print_result "$description - File exists" "PASS"

    if grep -q "$expected_pattern" "$file"; then
        print_result "$description - Contains expected pattern" "PASS"
        return 0
    else
        print_result "$description - Contains expected pattern" "FAIL"
        echo "  Error: Pattern not found: $expected_pattern"
        echo "  File content:"
        cat "$file"
        return 1
    fi
}

verify_file_content_not() {
    local file="$1"
    local expected_pattern="$2"
    local description="$3"

    if [[ ! -f "$file" ]]; then
        print_result "$description - File exists" "FAIL"
        echo "  Error: File not found: $file"
        return 1
    fi

    if ! grep -q "$expected_pattern" "$file"; then
        print_result "$description - Does not contain pattern" "PASS"
        return 0
    else
        print_result "$description - Does not contain pattern" "FAIL"
        echo "  Error: Pattern should not be found but was: $expected_pattern"
        echo "  File content:"
        cat "$file"
        return 1
    fi
}

# Function to verify placeholder replacement
verify_no_placeholders() {
    local file="$1"
    local description="$2"

    if grep -q "@[A-Z_]*@" "$file"; then
        print_result "$description - No unreplaced placeholders" "FAIL"
        echo "  Error: Found unreplaced placeholders in $file:"
        grep "@[A-Z_]*@" "$file"
        return 1
    else
        print_result "$description - No unreplaced placeholders" "PASS"
        return 0
    fi
}

# Cleanup function
cleanup() {
    echo ""
    echo "Cleaning up test output directory..."
    rm -rf "$TEST_OUTPUT_DIR"
}

# Setup function
setup() {
    echo "Setting up test environment..."
    rm -rf "$TEST_OUTPUT_DIR"
    mkdir -p "$TEST_OUTPUT_DIR"
    echo "Test output directory: $TEST_OUTPUT_DIR"
}

# Test 1: Generate AppArmor profile with ssamd config
test_apparmor_profile_only() {
    print_test_header "Test 1: Generate AppArmor Profile with ssamd config"

    local test_dir="${TEST_OUTPUT_DIR}/test1"
    mkdir -p "$test_dir"
    local apparmor_output="${test_dir}/usr.bin.ssamd"
    local ssamd_config_output="${test_dir}/ssamd.toml"

    "$GEN_CONFIG_SCRIPT" \
        --output-path "$test_dir" \
        --gen-apparmor-profile \
        --pkgs-mnt-root /var/lib/mcontainer/mnt \
        --bundled-pkgs-dir /opt/mcontainer/pkgs \
        --downloaded-pkgs-dir /var/lib/mcontainer/packages \
        --pkgs-data-root /var/lib/mcontainer/data \
        --public-key-file /usr/share/ssamd/prod.pub.key \
        --packages-ext pkg

    verify_file_content "$apparmor_output" "abi <abi/3.0>" "AppArmor profile"
    verify_file_content "$apparmor_output" "/usr/bin/ssamd" "AppArmor profile - executable path"
    verify_file_content "$apparmor_output" "/var/{lib/,lib/mcontainer/,lib/mcontainer/mnt/} w," "AppArmor profile - mount root hierarchy"
    verify_file_content "$apparmor_output" "/var/lib/mcontainer/mnt/\*/ w," "AppArmor profile - mount root children"
    verify_file_content "$apparmor_output" "/var/lib/mcontainer/packages/\*.pkg rw," "AppArmor profile - downloaded packages dir"
    verify_file_content "$apparmor_output" "/opt/mcontainer/pkgs/\*.pkg r," "AppArmor profile - bundled packages dir"
    verify_file_content "$apparmor_output" "/usr/share/ssamd/prod.pub.key r" "AppArmor profile - public key"
    verify_no_placeholders "$apparmor_output" "AppArmor profile"
}

# Test 2: Generate runtime config only
test_runtime_config_only() {
    print_test_header "Test 2: Generate Runtime Config Only"

    local test_dir="${TEST_OUTPUT_DIR}/test2"
    mkdir -p "$test_dir"
    local output_file="${test_dir}/ssamd.toml"

    "$GEN_CONFIG_SCRIPT" \
        --output-path "$test_dir" \
        --pkgs-mnt-root /var/lib/mcontainer/mnt \
        --bundled-pkgs-dir /opt/mcontainer/base-pkgs \
        --downloaded-pkgs-dir /var/lib/mcontainer/packages \
        --pkgs-data-root /var/lib/mcontainer/data \
        --pkgs-overlayfs-root /var/run/mcontainer/overlayfs \
        --pkgs-cgroup mcontainer.slice \
        --public-key-file /usr/share/ssamd/prod.pub.key \
        --packages-ext pkg

    verify_file_content "$output_file" "\[common\]" "Runtime config"
    verify_file_content "$output_file" "bundled_packages_dir = \"/opt/mcontainer/base-pkgs\"" "Runtime config - bundled packages dir"
    verify_file_content "$output_file" "downloaded_packages_dir = \"/var/lib/mcontainer/packages\"" "Runtime config - downloaded packages dir"
    verify_file_content "$output_file" "packages_data_root = \"/var/lib/mcontainer/data\"" "Runtime config - data root"
    verify_file_content "$output_file" "packages_overlayfs_root = \"/var/run/mcontainer/overlayfs\"" "Runtime config - overlayfs root"
    verify_file_content "$output_file" "packages_mnt_root = \"/var/lib/mcontainer/mnt\"" "Runtime config - mount root"
    verify_file_content "$output_file" "public_key_file_path = \"/usr/share/ssamd/prod.pub.key\"" "Runtime config - public key"
    verify_file_content "$output_file" "packages_cgroup = \"mcontainer.slice\"" "Runtime config - cgroup"
    verify_file_content "$output_file" "packages_ext = \"pkg\"" "Runtime config - extension"
    verify_file_content "$output_file" "\[network\.bridge\]" "Runtime config - network section"
    verify_file_content "$output_file" "enabled = false" "Runtime config - default bridge enabled"
    verify_file_content "$output_file" "\[network\.bridge\.addr_pool\]" "Runtime config - addr_pool section"
    verify_file_content "$output_file" 'base = "172.20.0.0/16"' "Runtime config - default base"
    verify_file_content "$output_file" "size = 29" "Runtime config - default size"
    verify_no_placeholders "$output_file" "Runtime config"
}

# Test 3: Generate both AppArmor profile and runtime config simultaneously
test_both_simultaneously() {
    print_test_header "Test 3: Generate Both AppArmor Profile and Runtime Config Simultaneously"

    local test_dir="${TEST_OUTPUT_DIR}/test3"
    mkdir -p "$test_dir"
    local apparmor_output="${test_dir}/opt.ssamd.bin.ssamd"
    local runtime_output="${test_dir}/ssamd.toml"

    "$GEN_CONFIG_SCRIPT" \
        --output-path "$test_dir" \
        --gen-apparmor-profile \
        --ssamd-install-path /opt/ssamd/bin/ssamd \
        --pkgs-mnt-root /var/lib/mcontainer/mnt \
        --bundled-pkgs-dir /opt/mcontainer/pkgs \
        --downloaded-pkgs-dir /var/lib/mcontainer/packages \
        --pkgs-data-root /var/lib/mcontainer/data \
        --pkgs-overlayfs-root /var/run/mcontainer/overlayfs \
        --pkgs-cgroup mcontainer.slice \
        --public-key-file /usr/share/ssamd/prod.pub.key \
        --packages-ext pkg

    # Verify AppArmor profile
    verify_file_content "$apparmor_output" "abi <abi/3.0>" "AppArmor profile (simultaneous)"
    verify_file_content "$apparmor_output" "/opt/ssamd/bin/ssamd" "AppArmor profile (simultaneous) - executable path"
    verify_file_content "$apparmor_output" "/var/{lib/,lib/mcontainer/,lib/mcontainer/mnt/} w," "AppArmor profile (simultaneous) - mount root hierarchy"
    verify_file_content "$apparmor_output" "/var/lib/mcontainer/mnt/\*/ w," "AppArmor profile (simultaneous) - mount root children"
    verify_file_content "$apparmor_output" "/var/lib/mcontainer/packages/\*.pkg rw," "AppArmor profile (simultaneous) - downloaded packages dir"
    verify_file_content "$apparmor_output" "/opt/mcontainer/pkgs/\*.pkg r," "AppArmor profile (simultaneous) - bundled packages dir"
    verify_no_placeholders "$apparmor_output" "AppArmor profile (simultaneous)"

    # Verify runtime config
    verify_file_content "$runtime_output" "\[common\]" "Runtime config (simultaneous)"
    verify_file_content "$runtime_output" "bundled_packages_dir = \"/opt/mcontainer/pkgs\"" "Runtime config (simultaneous) - bundled packages dir"
    verify_file_content "$runtime_output" "downloaded_packages_dir = \"/var/lib/mcontainer/packages\"" "Runtime config (simultaneous) - downloaded packages dir"
    verify_file_content "$runtime_output" "packages_overlayfs_root = \"/var/run/mcontainer/overlayfs\"" "Runtime config (simultaneous) - overlayfs root"
    verify_no_placeholders "$runtime_output" "Runtime config (simultaneous)"
}

# Test 4: Runtime config uses default bundled directory when not specified
test_default_bundled_dir() {
    print_test_header "Test 4: Runtime Config uses default bundled directory"

    local test_dir="${TEST_OUTPUT_DIR}/test4"
    mkdir -p "$test_dir"
    local output_file="${test_dir}/ssamd.toml"

    "$GEN_CONFIG_SCRIPT" \
        --output-path "$test_dir" \
        --downloaded-pkgs-dir /var/lib/ssamd/downloaded \
        --pkgs-mnt-root /var/lib/mcontainer/mnt \
        --pkgs-data-root /var/lib/mcontainer/data \
        --pkgs-overlayfs-root /var/run/mcontainer/overlayfs \
        --pkgs-cgroup mcontainer.slice \
        --public-key-file /usr/share/ssamd/test.pub.key \
        --packages-ext spkg

    verify_file_content "$output_file" 'bundled_packages_dir = "/var/lib/ssamd/bundled"' "Runtime config - default bundled packages dir"
    verify_file_content "$output_file" 'downloaded_packages_dir = "/var/lib/ssamd/downloaded"' "Runtime config - downloaded packages dir"
    verify_no_placeholders "$output_file" "Runtime config without bundled"
}

# Test 5: Error handling - Invalid install path
test_error_invalid_install_path() {
    print_test_header "Test 5: Error Handling - Invalid install path"

    if "$GEN_CONFIG_SCRIPT" --ssamd-install-path "relative/path" 2>&1 | grep -q "Argument --ssamd-install-path requires an absolute path"; then
        print_result "Error handling - invalid install path" "PASS"
    else
        print_result "Error handling - invalid install path" "FAIL"
    fi
}

# Test 5b: Error handling - Invalid bundled packages dir (relative path)
test_error_invalid_bundled_pkgs_dir() {
    print_test_header "Test 5b: Error Handling - Invalid bundled-pkgs-dir"

    local output rc
    set +e
    output=$("$GEN_CONFIG_SCRIPT" --bundled-pkgs-dir "relative/bundled" 2>&1)
    rc=$?
    set -e
    if [[ $rc -ne 0 ]] && echo "$output" | grep -q "Argument --bundled-pkgs-dir requires an absolute path"; then
        print_result "Error handling - invalid bundled-pkgs-dir" "PASS"
    else
        print_result "Error handling - invalid bundled-pkgs-dir" "FAIL"
    fi
}

# Test 5c: Error handling - Invalid downloaded packages dir (relative path)
test_error_invalid_downloaded_pkgs_dir() {
    print_test_header "Test 5c: Error Handling - Invalid downloaded-pkgs-dir"

    local output rc
    set +e
    output=$("$GEN_CONFIG_SCRIPT" --downloaded-pkgs-dir "relative/downloaded" 2>&1)
    rc=$?
    set -e
    if [[ $rc -ne 0 ]] && echo "$output" | grep -q "Argument --downloaded-pkgs-dir requires an absolute path"; then
        print_result "Error handling - invalid downloaded-pkgs-dir" "PASS"
    else
        print_result "Error handling - invalid downloaded-pkgs-dir" "FAIL"
    fi
}

# Test 5d: Error handling - Same bundled and downloaded dirs
test_error_same_pkg_dirs() {
    print_test_header "Test 5d: Error Handling - Same package directories"

    local output rc

    set +e
    output=$("$GEN_CONFIG_SCRIPT" --bundled-pkgs-dir "/var/lib/ssamd/pkgs" --downloaded-pkgs-dir "/var/lib/ssamd/pkgs" 2>&1)
    rc=$?
    set -e
    if [[ $rc -ne 0 ]] && echo "$output" | grep -q "must not be the same directory"; then
        print_result "Error handling - identical dirs" "PASS"
    else
        print_result "Error handling - identical dirs" "FAIL"
    fi

    set +e
    output=$("$GEN_CONFIG_SCRIPT" --bundled-pkgs-dir "/var/lib/ssamd/pkgs/" --downloaded-pkgs-dir "/var/lib/ssamd/pkgs" 2>&1)
    rc=$?
    set -e
    if [[ $rc -ne 0 ]] && echo "$output" | grep -q "must not be the same directory"; then
        print_result "Error handling - identical dirs (trailing slash)" "PASS"
    else
        print_result "Error handling - identical dirs (trailing slash)" "FAIL"
    fi

    set +e
    output=$("$GEN_CONFIG_SCRIPT" --output-path "${TEST_OUTPUT_DIR}/test5d" --bundled-pkgs-dir "/var/lib/ssamd" --downloaded-pkgs-dir "/var/lib/ssamd/pkgs" 2>&1)
    rc=$?
    set -e
    if [[ $rc -eq 0 ]]; then
        print_result "Nested dirs are accepted (parent-child)" "PASS"
    else
        print_result "Nested dirs are accepted (parent-child)" "FAIL"
    fi
}

# Test 6: Default handling - missing configuration fields trigger defaults
test_defaults() {
    print_test_header "Test 6: Default Handling - Missing Configuration Fields Trigger Defaults"

    local test_dir="${TEST_OUTPUT_DIR}/test6"
    mkdir -p "$test_dir"
    local output_file="${test_dir}/ssamd.toml"

    # Test defaults
    "$GEN_CONFIG_SCRIPT" \
        --output-path "$test_dir"

    verify_file_content "$output_file" "/run/ssamd/mnt" "Ssamd default configuration - pkgs-mnt-root"
    verify_file_content "$output_file" "packages_ext = \"ssam\"" "Ssamd default configuration - packages_ext"
    verify_file_content "$output_file" "rpc_bind_ip = \"127.0.0.1\"" "Ssamd default configuration - rpc_bind_ip"
    verify_file_content "$output_file" 'bundled_packages_dir = "/var/lib/ssamd/bundled"' "Ssamd default configuration - bundled dir"
    verify_file_content "$output_file" 'downloaded_packages_dir = "/var/lib/ssamd/downloaded"' "Ssamd default configuration - downloaded dir"
}

# Test 10: Generate systemd service unit
test_service_unit_only() {
    print_test_header "Test 10: Generate Systemd Service Unit"

    local test_dir="${TEST_OUTPUT_DIR}/test10"
    mkdir -p "$test_dir"
    local service_output="${test_dir}/ssamd.service"

    "$GEN_CONFIG_SCRIPT" \
        --output-path "$test_dir" \
        --gen-service-unit \
        --ssamd-install-path /opt/ssamd/bin/ssamd \
        --pkgs-mnt-root /var/lib/mcontainer/mnt \
        --downloaded-pkgs-dir /var/lib/mcontainer/packages \
        --pkgs-data-root /var/lib/mcontainer/data \
        --public-key-file /usr/share/ssamd/prod.pub.key \
        --packages-ext pkg

    verify_file_content "$service_output" "\[Unit\]" "Service unit"
    verify_file_content "$service_output" "Description=SSAM Daemon" "Service unit - description"
    verify_file_content "$service_output" "ExecStart=/opt/ssamd/bin/ssamd" "Service unit - ExecStart"
    verify_file_content "$service_output" "RequiresMountsFor=/tmp /var/lib/ssamd/bundled /var/lib/mcontainer/packages /var/lib/mcontainer/data /var/lib/mcontainer/mnt" "Service unit - RequiresMountsFor"
    verify_file_content "$service_output" "Type=notify" "Service unit - Type"
    verify_no_placeholders "$service_output" "Service unit"
}

# Test 11: Generate service unit with bundled and downloaded directories
test_service_unit_with_base_dir() {
    print_test_header "Test 11: Generate Service Unit with Bundled and Downloaded Dirs"

    local test_dir="${TEST_OUTPUT_DIR}/test11"
    mkdir -p "$test_dir"
    local service_output="${test_dir}/ssamd.service"

    "$GEN_CONFIG_SCRIPT" \
        --output-path "$test_dir" \
        --gen-service-unit \
        --ssamd-install-path /usr/bin/ssamd \
        --pkgs-mnt-root /var/lib/mcontainer/mnt \
        --bundled-pkgs-dir /opt/mcontainer/base-pkgs \
        --downloaded-pkgs-dir /var/lib/mcontainer/packages \
        --pkgs-data-root /var/lib/mcontainer/data \
        --public-key-file /usr/share/ssamd/test.pub.key \
        --packages-ext spkg

    # Verify that RequiresMountsFor contains both package directories separated by spaces
    verify_file_content "$service_output" "RequiresMountsFor=/tmp /opt/mcontainer/base-pkgs /var/lib/mcontainer/packages /var/lib/mcontainer/data /var/lib/mcontainer/mnt" "Service unit - bundled + downloaded dirs"
    verify_no_placeholders "$service_output" "Service unit with bundled dir"
}

# Test 12: Generate all configuration files simultaneously
test_all_configs_simultaneously() {
    print_test_header "Test 12: Generate All Configuration Files Simultaneously"

    local test_dir="${TEST_OUTPUT_DIR}/test12"
    mkdir -p "$test_dir"
    local apparmor_output="${test_dir}/opt.ssamd.bin.ssamd"
    local service_output="${test_dir}/ssamd.service"
    local runtime_output="${test_dir}/ssamd.toml"

    "$GEN_CONFIG_SCRIPT" \
        --output-path "$test_dir" \
        --gen-apparmor-profile \
        --gen-service-unit \
        --ssamd-install-path /opt/ssamd/bin/ssamd \
        --pkgs-mnt-root /var/lib/mcontainer/mnt \
        --bundled-pkgs-dir /opt/mcontainer/pkgs \
        --downloaded-pkgs-dir /var/lib/mcontainer/packages \
        --pkgs-data-root /var/lib/mcontainer/data \
        --pkgs-overlayfs-root /var/run/mcontainer/overlayfs \
        --pkgs-cgroup mcontainer.slice \
        --public-key-file /usr/share/ssamd/prod.pub.key \
        --packages-ext pkg

    # Verify AppArmor profile
    verify_file_content "$apparmor_output" "abi <abi/3.0>" "All configs - AppArmor profile"
    verify_file_content "$apparmor_output" "/opt/ssamd/bin/ssamd" "All configs - AppArmor executable path"
    verify_file_content "$apparmor_output" "/var/{lib/,lib/mcontainer/,lib/mcontainer/mnt/} w," "All configs - AppArmor mount root hierarchy"
    verify_file_content "$apparmor_output" "/var/lib/mcontainer/mnt/\*/ w," "All configs - AppArmor mount root children"
    verify_no_placeholders "$apparmor_output" "All configs - AppArmor profile"

    # Verify service unit
    verify_file_content "$service_output" "\[Unit\]" "All configs - Service unit"
    verify_file_content "$service_output" "ExecStart=/opt/ssamd/bin/ssamd" "All configs - Service unit ExecStart"
    verify_file_content "$service_output" "RequiresMountsFor=/tmp /opt/mcontainer/pkgs /var/lib/mcontainer/packages /var/lib/mcontainer/data /var/lib/mcontainer/mnt" "All configs - Service unit RequiresMountsFor"
    verify_no_placeholders "$service_output" "All configs - Service unit"

    # Verify runtime config
    verify_file_content "$runtime_output" "\[common\]" "All configs - Runtime config"
    verify_file_content "$runtime_output" "bundled_packages_dir = \"/opt/mcontainer/pkgs\"" "All configs - Runtime config bundled dir"
    verify_file_content "$runtime_output" "downloaded_packages_dir = \"/var/lib/mcontainer/packages\"" "All configs - Runtime config downloaded dir"
    verify_file_content "$runtime_output" "packages_overlayfs_root = \"/var/run/mcontainer/overlayfs\"" "All configs - Runtime config overlayfs root"
    verify_no_placeholders "$runtime_output" "All configs - Runtime config"
}

# Test 13: Service unit with default values
test_service_unit_defaults() {
    print_test_header "Test 13: Service Unit with Default Values"

    local test_dir="${TEST_OUTPUT_DIR}/test13"
    mkdir -p "$test_dir"
    local service_output="${test_dir}/ssamd.service"

    "$GEN_CONFIG_SCRIPT" \
        --output-path "$test_dir" \
        --gen-service-unit \
        --downloaded-pkgs-dir /var/lib/mcontainer/packages \
        --pkgs-data-root /var/lib/mcontainer/data \
        --public-key-file /usr/share/ssamd/test.key \
        --packages-ext pkg

    verify_file_content "$service_output" "ExecStart=/usr/bin/ssamd" "Service unit defaults - ExecStart"
    verify_file_content "$service_output" "RequiresMountsFor=/tmp /var/lib/ssamd/bundled /var/lib/mcontainer/packages /var/lib/mcontainer/data /var/run/ssamd/mnt" "Service unit defaults - RequiresMountsFor with default mnt root"
    verify_no_placeholders "$service_output" "Service unit with defaults"
}

# Test 14: RPC Bind IP
test_rpc_bind_ip() {
    print_test_header "Test 14: RPC Bind IP"

    local test_dir="${TEST_OUTPUT_DIR}/test14"
    mkdir -p "$test_dir"
    local output_file="${test_dir}/ssamd.toml"

    "$GEN_CONFIG_SCRIPT" \
        --output-path "$test_dir" \
        --rpc-bind-ip 192.168.1.100

    verify_file_content "$output_file" "rpc_bind_ip = \"192.168.1.100\"" "Runtime config - rpc_bind_ip"
}

# Test 19: Network section CLI overrides
test_network_overrides() {
    print_test_header "Test 19: Network Section CLI Overrides"

    local test_dir="${TEST_OUTPUT_DIR}/test19"
    mkdir -p "$test_dir"
    local output_file="${test_dir}/ssamd.toml"

    "$GEN_CONFIG_SCRIPT" \
        --output-path "$test_dir" \
        --bridge-enabled true \
        --bridge-pool-base 10.10.0.0/16 \
        --bridge-pool-size 28

    verify_file_content "$output_file" "\[network\.bridge\]" "Runtime config - network section (override)"
    verify_file_content "$output_file" "enabled = true" "Runtime config - bridge enabled override"
    verify_file_content "$output_file" "\[network\.bridge\.addr_pool\]" "Runtime config - addr_pool section (override)"
    verify_file_content "$output_file" 'base = "10.10.0.0/16"' "Runtime config - base override"
    verify_file_content "$output_file" "size = 28" "Runtime config - size override"
    verify_no_placeholders "$output_file" "Runtime config (network override)"
}

# Test 20: Error handling - network arg validation
test_network_arg_validation() {
    print_test_header "Test 20: Error Handling - Network Arg Validation"

    local output rc

    set +e
    output=$("$GEN_CONFIG_SCRIPT" --bridge-enabled notabool 2>&1)
    rc=$?
    set -e
    if [[ $rc -ne 0 ]] && echo "$output" | grep -q "bridge-enabled must be 'true' or 'false'"; then
        print_result "Error handling - invalid bridge-enabled" "PASS"
    else
        print_result "Error handling - invalid bridge-enabled" "FAIL"
    fi

    set +e
    output=$("$GEN_CONFIG_SCRIPT" --bridge-pool-size 99 2>&1)
    rc=$?
    set -e
    if [[ $rc -ne 0 ]] && echo "$output" | grep -q "bridge-pool-size must be an integer in \[1, 30\]"; then
        print_result "Error handling - bridge-pool-size out of range" "PASS"
    else
        print_result "Error handling - bridge-pool-size out of range" "FAIL"
    fi

    set +e
    output=$("$GEN_CONFIG_SCRIPT" --bridge-pool-size abc 2>&1)
    rc=$?
    set -e
    if [[ $rc -ne 0 ]] && echo "$output" | grep -q "bridge-pool-size must be an integer in \[1, 30\]"; then
        print_result "Error handling - bridge-pool-size non-integer" "PASS"
    else
        print_result "Error handling - bridge-pool-size non-integer" "FAIL"
    fi

    set +e
    output=$("$GEN_CONFIG_SCRIPT" --bridge-pool-base not-a-cidr 2>&1)
    rc=$?
    set -e
    if [[ $rc -ne 0 ]] && echo "$output" | grep -q "bridge-pool-base must be an IPv4 CIDR"; then
        print_result "Error handling - bridge-pool-base malformed shape" "PASS"
    else
        print_result "Error handling - bridge-pool-base malformed shape" "FAIL"
    fi

    set +e
    output=$("$GEN_CONFIG_SCRIPT" --bridge-pool-base '172.20.0.0/16"; extra = 1 #' 2>&1)
    rc=$?
    set -e
    if [[ $rc -ne 0 ]] && echo "$output" | grep -q "bridge-pool-base must be an IPv4 CIDR"; then
        print_result "Error handling - bridge-pool-base rejects TOML injection" "PASS"
    else
        print_result "Error handling - bridge-pool-base rejects TOML injection" "FAIL"
    fi

    set +e
    output=$("$GEN_CONFIG_SCRIPT" --bridge-pool-base 172.20.0.999/16 2>&1)
    rc=$?
    set -e
    if [[ $rc -ne 0 ]] && echo "$output" | grep -q "octet > 255"; then
        print_result "Error handling - bridge-pool-base octet out of range" "PASS"
    else
        print_result "Error handling - bridge-pool-base octet out of range" "FAIL"
    fi

    set +e
    output=$("$GEN_CONFIG_SCRIPT" --bridge-pool-base 172.20.0.0/40 2>&1)
    rc=$?
    set -e
    if [[ $rc -ne 0 ]] && echo "$output" | grep -q "prefix must be <= 32"; then
        print_result "Error handling - bridge-pool-base prefix out of range" "PASS"
    else
        print_result "Error handling - bridge-pool-base prefix out of range" "FAIL"
    fi

    local test_dir="${TEST_OUTPUT_DIR}/test20"
    mkdir -p "$test_dir"
    local output_file="${test_dir}/ssamd.toml"
    "$GEN_CONFIG_SCRIPT" \
        --output-path "$test_dir" \
        --bridge-pool-size 29 \
        --bridge-pool-base 10.0.0.0/8 \
        --bridge-enabled true
    verify_file_content "$output_file" "size = 29" "Valid bridge-pool-size still succeeds"
    verify_file_content "$output_file" 'base = "10.0.0.0/8"' "Valid bridge-pool-base still succeeds"
}

# Test 15: AppArmor profile with /var/run special case
test_apparmor_var_run_special_case() {
    print_test_header "Test 15: AppArmor Profile with /var/run Special Case"

    local test_dir="${TEST_OUTPUT_DIR}/test15"
    mkdir -p "$test_dir"
    local apparmor_output="${test_dir}/usr.bin.ssamd"

    "$GEN_CONFIG_SCRIPT" \
        --output-path "$test_dir" \
        --gen-apparmor-profile \
        --pkgs-mnt-root /var/run/ssamd/mnt \
        --downloaded-pkgs-dir /var/lib/mcontainer/packages \
        --pkgs-data-root /var/lib/mcontainer/data \
        --public-key-file /usr/share/ssamd/prod.pub.key \
        --packages-ext pkg

    verify_file_content "$apparmor_output" "@{run}{ssamd/,ssamd/mnt/} w," "AppArmor profile - /var/run special case hierarchy"
    verify_file_content "$apparmor_output" "@{run}ssamd/mnt/\*/ w," "AppArmor profile - /var/run special case wildcard"
}

# Test 16: AppArmor profile with /run special case
test_apparmor_run_special_case() {
    print_test_header "Test 16: AppArmor Profile with /run Special Case"

    local test_dir="${TEST_OUTPUT_DIR}/test16"
    mkdir -p "$test_dir"
    local apparmor_output="${test_dir}/usr.bin.ssamd"

    "$GEN_CONFIG_SCRIPT" \
        --output-path "$test_dir" \
        --gen-apparmor-profile \
        --pkgs-mnt-root /run/ssamd/mnt \
        --downloaded-pkgs-dir /var/lib/mcontainer/packages \
        --pkgs-data-root /var/lib/mcontainer/data \
        --public-key-file /usr/share/ssamd/prod.pub.key \
        --packages-ext pkg

    verify_file_content "$apparmor_output" "@{run}{ssamd/,ssamd/mnt/} w," "AppArmor profile - /run special case hierarchy"
    verify_file_content "$apparmor_output" "@{run}ssamd/mnt/\*/ w," "AppArmor profile - /run special case wildcard"
}

# Test 17: AppArmor profile with /var/run prefix collision (e.g. /var/runner)
test_apparmor_var_run_prefix_collision() {
    print_test_header "Test 17: AppArmor Profile with /var/run Prefix Collision"

    local test_dir="${TEST_OUTPUT_DIR}/test17"
    mkdir -p "$test_dir"
    local apparmor_output="${test_dir}/usr.bin.ssamd"

    "$GEN_CONFIG_SCRIPT" \
        --output-path "$test_dir" \
        --gen-apparmor-profile \
        --pkgs-mnt-root /var/runner/ssamd/mnt \
        --downloaded-pkgs-dir /var/lib/mcontainer/packages \
        --pkgs-data-root /var/lib/mcontainer/data \
        --public-key-file /usr/share/ssamd/prod.pub.key \
        --packages-ext pkg

    # Should NOT be normalized to /runner or use @{run}
    # Should start with /var
    verify_file_content "$apparmor_output" "/var/{runner/,runner/ssamd/,runner/ssamd/mnt/} w," "AppArmor profile - prefix collision hierarchy"
    verify_file_content "$apparmor_output" "/var/runner/ssamd/mnt/\*/ w," "AppArmor profile - prefix collision wildcard"
}

# Test 18: AppArmor profile for --pkgs-mnt-root
test_apparmor_pkgs_mnt_root() {
    print_test_header "Test 18: AppArmor Profile for --pkgs-mnt-root"

    local test_dir="${TEST_OUTPUT_DIR}/test18"
    mkdir -p "$test_dir"
    local apparmor_output="${test_dir}/usr.bin.ssamd"
    local title="AppArmor profile with --pkgs-mnt-root"

    "$GEN_CONFIG_SCRIPT" \
        --output-path "$test_dir" \
        --gen-apparmor-profile \
        --pkgs-mnt-root /var/run/ssamd/mnt

    verify_file_content "$apparmor_output" "@{run}{ssamd/,ssamd/mnt/} w," "$title /var/run/ssamd/mnt"
    verify_file_content "$apparmor_output" "@{run}ssamd/mnt/\*/ w," "$title /var/run/ssamd/mnt"

    "$GEN_CONFIG_SCRIPT" \
        --output-path "$test_dir" \
        --gen-apparmor-profile \
        --pkgs-mnt-root /run/ssamd/mnt

    verify_file_content "$apparmor_output" "@{run}{ssamd/,ssamd/mnt/} w," "$title /run/ssamd/mnt"
    verify_file_content "$apparmor_output" "@{run}ssamd/mnt/\*/ w," "$title /run/ssamd/mnt"

    "$GEN_CONFIG_SCRIPT" \
        --output-path "$test_dir" \
        --gen-apparmor-profile \
        --pkgs-mnt-root /lib/ssamd/mnt

    verify_file_content "$apparmor_output" "/lib/{ssamd/,ssamd/mnt/} w," "$title /lib/ssamd/mnt"
    verify_file_content "$apparmor_output" "/lib/ssamd/mnt/\*/ w," "$title /lib/ssamd/mnt"

    "$GEN_CONFIG_SCRIPT" \
        --output-path "$test_dir" \
        --gen-apparmor-profile \
        --pkgs-mnt-root /ssamd

    verify_file_content_not "$apparmor_output" "/ssamd/ w," "$title /ssamd"
    verify_file_content_not "$apparmor_output" "/ssamd w," "$title /ssamd"
    verify_file_content "$apparmor_output" "/ssamd/\*/ w," "$title /ssamd"
}

# Main test execution
main() {
    echo "=================================="
    echo "gen_ssamd_conf.sh Test Suite"
    echo "=================================="
    echo ""

    setup

    # Run all tests
    test_apparmor_profile_only
    test_runtime_config_only
    test_both_simultaneously
    test_default_bundled_dir
    test_error_invalid_install_path
    test_error_invalid_bundled_pkgs_dir
    test_error_invalid_downloaded_pkgs_dir
    test_error_same_pkg_dirs
    test_defaults
    test_service_unit_only
    test_service_unit_with_base_dir
    test_all_configs_simultaneously
    test_service_unit_defaults
    test_rpc_bind_ip
    test_network_overrides
    test_network_arg_validation
    test_apparmor_var_run_special_case
    test_apparmor_run_special_case
    test_apparmor_var_run_prefix_collision
    test_apparmor_pkgs_mnt_root

    # Print summary
    echo ""
    echo "=========================================="
    echo "Test Summary"
    echo "=========================================="
    echo "Total Tests: $TESTS_TOTAL"
    echo -e "${GREEN}Passed: $TESTS_PASSED${NC}"
    if [[ $TESTS_FAILED -gt 0 ]]; then
        echo -e "${RED}Failed: $TESTS_FAILED${NC}"
    else
        echo "Failed: $TESTS_FAILED"
    fi
    echo ""

    # Cleanup
    cleanup

    # Exit with appropriate code
    if [[ $TESTS_FAILED -gt 0 ]]; then
        echo -e "${RED}Some tests failed!${NC}"
        exit 1
    else
        echo -e "${GREEN}All tests passed!${NC}"
        exit 0
    fi
}

# Run main function
main
