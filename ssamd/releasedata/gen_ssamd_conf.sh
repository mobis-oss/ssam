#!/bin/bash
# Copyright 2025-2026 Hyundai Mobis Co., Ltd.
# SPDX-License-Identifier: Apache-2.0

# Configuration Files Generator for SSAMD
# This script generates SSAMD configuration files including the AppArmor
# profile and the systemd service unit from templates by replacing placeholders
# in templates with user-provided values.

set -e
export LC_ALL=C

#=============================================================================
# Configuration Variables
#=============================================================================

# Default output path of generated files is the current working directory
CONF_OUTPUT_PATH="$PWD"

# Flags to enable generating additional configuration files
unset GEN_APPARMOR_PROFILE
unset GEN_SERVICE_UNIT

# Configuration values are populated only when the corresponding argument is
# supplied. Defaults belong to ssamd's configuration implementation, not this
# generator, except for required feature fields whose empty value explicitly
# disables that feature.
declare -A SSAMD_CONFIG
SSAMD_CONFIG=()

config_is_set() {
    [[ -n "${SSAMD_CONFIG[$1]+set}" ]]
}

validate_path_argument() {
    local opt_name="$1" opt_value="$2" require_absolute="$3" allow_empty="$4"

    if [[ -z "$opt_value" ]]; then
        if [[ "$allow_empty" == "true" ]]; then
            return 0
        fi
        error_exit "Argument $opt_name requires a non-empty path."
    fi

    if [[ "$require_absolute" == "true" && "${opt_value:0:1}" != "/" ]]; then
        error_exit "Argument $opt_name requires an absolute path. Got: '$opt_value'"
    fi

    # Canonicalize missing components so validation does not require the path to exist.
    if ! realpath -m -- "$opt_value" >/dev/null 2>&1; then
        error_exit "Argument $opt_name is not a valid path. Got: '$opt_value'"
    fi
}

error_exit() {
    if [[ -n "$1" ]]; then
        echo "Error: $1" >&2
    fi
    echo >&2
    echo "Refer to --help for usage information." >&2
    exit 1
}

validate_rpc_allowed_gids() {
    local value="$1" gid normalized formatted=""
    local -a gids

    if [[ -z "$value" ]]; then
        RPC_ALLOWED_GIDS_NORMALIZED=""
        return 0
    fi

    if ! [[ "$value" =~ ^[0-9]+(,[0-9]+)*$ ]]; then
        error_exit "--rpc-allowed-gids must be a comma-separated ASCII decimal list, got: '$value'"
    fi

    IFS=',' read -r -a gids <<< "$value"
    for gid in "${gids[@]}"; do
        normalized="$gid"
        while [[ ${#normalized} -gt 1 && ${normalized:0:1} == "0" ]]; do
            normalized="${normalized:1}"
        done
        if [[ ${#normalized} -gt 10 ]] ||
            [[ ${#normalized} -eq 10 && "$normalized" > "4294967295" ]]; then
            error_exit "--rpc-allowed-gids values must be in [0, 4294967295], got: '$gid'"
        fi
        if [[ -n "$formatted" ]]; then
            formatted+=", "
        fi
        formatted+="$normalized"
    done
    RPC_ALLOWED_GIDS_NORMALIZED="$formatted"
}

usage() {
    cat << EOF
Usage: $(basename "$0") [OPTIONS]

* Generates the ssam daemon configuration file - ssamd.toml.
* Optionally, the script can generate other configuration files, such as the
  Systemd service unit file.

Arguments:
    -h, --help              Display this help message

Output Arguments:
    --output-path PATH      Path where the generated files will be located.
                            If not specified, current working directory is used.
    --gen-service-unit      Enable generating ssamd.service
    --gen-apparmor-profile  Enable generating AppArmor profile for SSAMD

Arguments for SSAMD configuration (All paths below are on the target device)
  Daemon configuration:
    --ssamd-install-path    PATH  Required: SSAMD installation path when
                                  --gen-service-unit or --gen-apparmor-profile
                                  is used.

  Package configuration:
    Required:
      --oci-bundle-dir       PATH  Absolute OCI bundle root directory.
      --pkgs-data-root       PATH  Package data root directory.

    Optional:
      --builtin-pkgs-dir     PATH  Builtin (read-only) package directory.
                                  Omit for no builtin packages.
      --downloaded-pkgs-dir  PATH  Downloaded package directory.
                                  Omit for no downloaded packages.
                                  At least one package directory is required.
      --pkgs-overlayfs-root  PATH  OverlayFS root directory.
                                  Omit for disabled OverlayFS.
      --pkgs-cgroup          NAME  Cgroup name.
                                  Omit for default cgroup (system.slice).
      --public-key-file      PATH  Public key file for package verification.
                                  Defaults to /etc/ssamd/public.key.
      --packages-ext         EXT   Package file extension without a dot.
                                  Omit for the default package extension: ssam.

  Network configuration (optional):
    --bridge-enabled        BOOL  Enable per-container bridge networking;
                                  Omit for disabled bridge networking.
    --bridge-pool-base      CIDR  IP pool bridges carve container subnets from;
                                  Omit for the default pool base;
                                  requires --bridge-enabled.
    --bridge-pool-size      BITS  Prefix size of each per-bridge subnet;
                                  Omit for the default pool size;
                                  requires --bridge-enabled.

  Integrity configuration (optional):
    --disable-runtime-integrity BOOL
                                 Disable runtime package filesystem integrity.
                                 Omit for enabled runtime package filesystem
                                 integrity.
    --disable-package-verify BOOL
                                 Disable package signature verification.

  RPC configuration (optional):
    --rpc-allowed-gids      LIST  Additional RPC peer effective GIDs to permit,
                                  comma-separated.
                                  Omit for no additional RPC GIDs.

Example - Generate all available configuration files:
    $(basename "$0") \\
        --output-path . \\
        --gen-service-unit \\
        --gen-apparmor-profile \\
        --ssamd-install-path /usr/bin/ssamd \\
        --oci-bundle-dir /var/run/ssamd/oci-bundle \\
        --builtin-pkgs-dir /var/lib/ssamd/builtin \\
        --downloaded-pkgs-dir /var/lib/ssamd/downloaded \\
        --pkgs-data-root /var/lib/ssamd/data \\
        --public-key-file /etc/ssamd/public.key \\
        --packages-ext ssam \\
        --pkgs-overlayfs-root /var/lib/ssamd/data/overlayfs

EOF
}



display_configuration() {
    echo "Using configurations:"
    for conf_key in "${!SSAMD_CONFIG[@]}"; do
        echo "  $conf_key: ${SSAMD_CONFIG[$conf_key]}"
    done
    echo
}

update_template_common () {
    local template_name output_filename
    template_name=$1
    output_filename=$2

    mkdir -p "$(dirname "$output_filename")"

    echo "${!template_name}" | sed \
        -e "s|@CONF_SSAMD_INSTALL_PATH@|${SSAMD_CONFIG[CONF_SSAMD_INSTALL_PATH]-}|g" \
        -e "s|@CONF_BUILTIN_PACKAGES_DIR@|${SSAMD_CONFIG[CONF_BUILTIN_PACKAGES_DIR]-}|g" \
        -e "s|@CONF_DOWNLOADED_PACKAGES_DIR@|${SSAMD_CONFIG[CONF_DOWNLOADED_PACKAGES_DIR]-}|g" \
        -e "s|@CONF_PACKAGES_DATA_ROOT@|${SSAMD_CONFIG[CONF_PACKAGES_DATA_ROOT]-}|g" \
        -e "s|@CONF_PKGFS_OVERLAY_DIR@|${SSAMD_CONFIG[CONF_PKGFS_OVERLAY_DIR]-}|g" \
        -e "s|@CONF_OCI_BUNDLE_DIR@|${SSAMD_CONFIG[CONF_OCI_BUNDLE_DIR]-}|g" \
        -e "s|@CONF_PUBLIC_KEY_FILE_PATH@|${SSAMD_CONFIG[CONF_PUBLIC_KEY_FILE_PATH]-}|g" \
        -e "s|@CONF_PACKAGES_CGROUP@|${SSAMD_CONFIG[CONF_PACKAGES_CGROUP]-}|g" \
        -e "s|@CONF_PACKAGES_EXT@|${SSAMD_CONFIG[CONF_PACKAGES_EXT]-}|g" \
        -e "s|@CONF_BRIDGE_ENABLED@|${SSAMD_CONFIG[CONF_BRIDGE_ENABLED]-}|g" \
        -e "s|@CONF_BRIDGE_POOL_BASE@|${SSAMD_CONFIG[CONF_BRIDGE_POOL_BASE]-}|g" \
        -e "s|@CONF_BRIDGE_POOL_SIZE@|${SSAMD_CONFIG[CONF_BRIDGE_POOL_SIZE]-}|g" \
        -e "s|@CONF_DISABLE_PKGFS_INTEGRITY@|${SSAMD_CONFIG[CONF_DISABLE_PKGFS_INTEGRITY]-}|g" \
        -e "s|@CONF_DISABLE_PACKAGE_VERIFY@|${SSAMD_CONFIG[CONF_DISABLE_PACKAGE_VERIFY]-}|g" \
        -e "s|@CONF_RPC_ALLOWED_GIDS@|${SSAMD_CONFIG[CONF_RPC_ALLOWED_GIDS]-}|g" \
        -e "s|@CONF_REQUIRES_MOUNTS@|${SSAMD_CONFIG[CONF_REQUIRES_MOUNTS]-}|g" \
        -e "s|@APPARMOR_PUBLIC_KEY_RULE@|${SSAMD_CONFIG[CONF_APPARMOR_PUBLIC_KEY_RULE]-}|g" \
        - > "$output_filename"
}

# Normalize path to use AppArmor variables
# e.g.
#   /var/run/foo -> @{run}foo
#   /run/foo     -> @{run}foo
#   /etc/foo     -> /etc/foo
apparmor_normalize_path() {
    local path="$1"
    path=${path%/}  # Remove trailing slash

    # Normalize /var/run to /run
    if [[ "$path" == "/var/run" ]] || [[ "$path" == "/var/run/"* ]]; then
        path="/run${path#/var/run}"
    fi

    if [[ "$path" == "/run" ]]; then
        echo "@{run}"
    elif [[ "$path" == "/run/"* ]]; then
        echo "@{run}${path#/run/}"
    else
        echo "$path"
    fi
}

# Generate apparmor permission policy to allow `mkdir -p` for given path
# * Generates brace expansion pattern to grant write permission for
#   intermediate directories exclusively
# * First entry of the path would not be included by brace expansion
#   * Assuming that it's under read-only so the system partition must have it
# * Generates wildcard pattern for whole path only to allow creating random
#   subdirectories per container name
# * Utilizes @{run} for paths start with /run and /var/run
#   * @{run} must not have a trailing slash when combinded with other path
# * e.g.
#   - For input /run/ssamd/oci-bundle or /var/run/ssamd/oci-bundle:
#     @{run}{ssamd/,ssamd/oci-bundle/} w,
#     @{run}ssamd/oci-bundle/** rwk,
#   - For input /var/lib/ssamd:
#     /var/{lib/,lib/ssamd/} w,
#     /var/lib/ssamd/** rwk,
apparmor_gen_mkdir_policy() {
    local path=$(apparmor_normalize_path "$1")
    local path_1st
    local path_remains
    local -a path_remains_array
    local path_brace_content
    local -a result

    if [[ "$path" == "@{run}"* ]]; then
        path_1st="@{run}"
        path_remains="${path#"@{run}"}"
    else
        path_remains="${path#/}"
    fi
    IFS='/' read -ra path_remains_array <<< "$path_remains"

    # Generate brace patterned policy
    local p accumulated_p
    for p in "${path_remains_array[@]}"; do
        if [ -z "$path_1st" ]; then
            path_1st="/${p}/"
        else
            accumulated_p="${accumulated_p}/${p}"
            path_brace_content="${path_brace_content}${accumulated_p#/}/,"
        fi
    done

    # Remove trailing comma
    path_brace_content="${path_brace_content%,}"
    if [ -n "$path_brace_content" ]; then
        result=("  ${path_1st}{${path_brace_content}} w,")
    fi

    # Permit bundle files and mount targets below each package directory.
    result+=("  ${path}/** rwk,")

    # Put \n at the end of each policy
    printf "%s\n" "${result[@]}"
}

generate_apparmor_profile() {
    local profile_filename conf_output conf_ssamd_toml_path public_key_path
    conf_ssamd_toml_path="$(dirname "${SSAMD_CONFIG[CONF_SSAMD_INSTALL_PATH]}")/ssamd.toml"
    # Get a valid filename for the AppArmor profile from the install path
    # Replace / with . to derive the expected filename for the AppArmor profile
    profile_filename="${SSAMD_CONFIG[CONF_SSAMD_INSTALL_PATH]//\//.}"
    # Remove leading dot since CONF_SSAMD_INSTALL_PATH always starts with '/'
    profile_filename="${profile_filename:1}"
    conf_output="${CONF_OUTPUT_PATH}/${profile_filename}"
    echo "Generating AppArmor profile - $conf_output"

    public_key_path="${SSAMD_CONFIG[CONF_PUBLIC_KEY_FILE_PATH]-/etc/ssamd/public.key}"
    if [[ "${SSAMD_CONFIG[CONF_DISABLE_PACKAGE_VERIFY]-false}" != "true" ]]; then
        SSAMD_CONFIG[CONF_APPARMOR_PUBLIC_KEY_RULE]="  ${public_key_path} r,"
    else
        SSAMD_CONFIG[CONF_APPARMOR_PUBLIC_KEY_RULE]=""
    fi

    update_template_common "TEMPLATE_SSAMD_APPARMOR" "$conf_output"

    # Grant permissions per package directory: builtin=read-only, downloaded=read-write
    local formatted_pkg_dir=""
    # Builtin directory: read-only access (system-provided, immutable packages)
    if [[ -n "${SSAMD_CONFIG[CONF_BUILTIN_PACKAGES_DIR]}" ]]; then
        local bdir="${SSAMD_CONFIG[CONF_BUILTIN_PACKAGES_DIR]}"
        formatted_pkg_dir="${formatted_pkg_dir}  ${bdir}/ r,\n"
        formatted_pkg_dir="${formatted_pkg_dir}  ${bdir}/* r,\n"
    fi
    # Downloaded directory: read-write access (user-installed packages)
    local ddir="${SSAMD_CONFIG[CONF_DOWNLOADED_PACKAGES_DIR]}"
    if [[ -n "$ddir" ]]; then
        formatted_pkg_dir="${formatted_pkg_dir}  ${ddir}/ rw,\n"
        formatted_pkg_dir="${formatted_pkg_dir}  ${ddir}/* rw,\n"
    fi
    # Remove trailing newline
    formatted_pkg_dir="${formatted_pkg_dir%\\n}"

    # Special treatment to grant write access to let ssamd create
    # ${SSAMD_CONFIG[CONF_OCI_BUNDLE_DIR]} with mkdir -p at runtime
    local oci_bundle_root_permissions
    oci_bundle_root_permissions=$(apparmor_gen_mkdir_policy "${SSAMD_CONFIG[CONF_OCI_BUNDLE_DIR]}")
    if [[ -n "$oci_bundle_root_permissions" ]]; then
        # Using gawk for in-place editing with multi-line replacement
        gawk -i inplace -v replacement="$oci_bundle_root_permissions" \
            '{gsub(/@APPARMOR_PERMISSION_OCI_BUNDLE_DIR@/, replacement); print}' \
            "$conf_output"
    else
        error_exit "Cannot generate permissions for OCI bundle root '${SSAMD_CONFIG[CONF_OCI_BUNDLE_DIR]}'."
        exit 1
    fi

    # Replace package directories (multi-line)
    # Using gawk for in-place editing with multi-line replacement
    gawk -i inplace -v replacement="$formatted_pkg_dir" \
        '{gsub(/@APPARMOR_PERMISSION_PACKAGES_DIR@/, replacement); print}' \
        "$conf_output"

    sed --in-place \
        -e "s|@CONF_SSAMD_TOML_PATH@|${conf_ssamd_toml_path}|g" \
        -e "s|@APPARMOR_NORMALIZED_OCI_BUNDLE_DIR@|$(apparmor_normalize_path "${SSAMD_CONFIG[CONF_OCI_BUNDLE_DIR]}")|g" \
        "$conf_output"
}

generate_service_unit() {
    local conf_output requires_mounts
    conf_output="${CONF_OUTPUT_PATH}/ssamd.service"

    requires_mounts="/tmp"
    if [[ -n "${SSAMD_CONFIG[CONF_BUILTIN_PACKAGES_DIR]}" ]]; then
        requires_mounts+=" ${SSAMD_CONFIG[CONF_BUILTIN_PACKAGES_DIR]}"
    fi
    if [[ -n "${SSAMD_CONFIG[CONF_DOWNLOADED_PACKAGES_DIR]}" ]]; then
        requires_mounts+=" ${SSAMD_CONFIG[CONF_DOWNLOADED_PACKAGES_DIR]}"
    fi
    requires_mounts+=" ${SSAMD_CONFIG[CONF_PACKAGES_DATA_ROOT]} ${SSAMD_CONFIG[CONF_OCI_BUNDLE_DIR]}"
    SSAMD_CONFIG[CONF_REQUIRES_MOUNTS]="$requires_mounts"

    echo "Generating Service unit - ${conf_output}..."
    update_template_common "TEMPLATE_SSAMD_SERVICE_UNIT" "$conf_output"
}

# Generate SSAMD configuration
generate_ssamd_config() {
    local conf_output="${CONF_OUTPUT_PATH}/ssamd.toml"
    echo "Generating ssamd configuration - ${conf_output}..."

    local SSAMD_TOML_CONTENT=$'[package]\n'
    if config_is_set CONF_PACKAGES_EXT; then
        SSAMD_TOML_CONTENT+="extension = \"@CONF_PACKAGES_EXT@\""$'\n'
    fi
    if config_is_set CONF_PACKAGES_CGROUP; then
        SSAMD_TOML_CONTENT+="cgroup = \"@CONF_PACKAGES_CGROUP@\""$'\n'
    fi
    if config_is_set CONF_PUBLIC_KEY_FILE_PATH; then
        SSAMD_TOML_CONTENT+="public_key = \"@CONF_PUBLIC_KEY_FILE_PATH@\""$'\n'
    fi

    SSAMD_TOML_CONTENT+=$'\n[package.dir]\n'
    if config_is_set CONF_BUILTIN_PACKAGES_DIR; then
        SSAMD_TOML_CONTENT+="builtin = \"@CONF_BUILTIN_PACKAGES_DIR@\""$'\n'
    fi
    if config_is_set CONF_DOWNLOADED_PACKAGES_DIR; then
        SSAMD_TOML_CONTENT+="downloaded = \"@CONF_DOWNLOADED_PACKAGES_DIR@\""$'\n'
    fi
    SSAMD_TOML_CONTENT+="oci_bundle = \"@CONF_OCI_BUNDLE_DIR@\""$'\n'
    SSAMD_TOML_CONTENT+="data = \"@CONF_PACKAGES_DATA_ROOT@\""$'\n'

    if config_is_set CONF_PKGFS_OVERLAY_DIR ||
        config_is_set CONF_DISABLE_PKGFS_INTEGRITY ||
        config_is_set CONF_DISABLE_PACKAGE_VERIFY; then
        SSAMD_TOML_CONTENT+=$'\n[debug]\n'
        if config_is_set CONF_PKGFS_OVERLAY_DIR; then
            SSAMD_TOML_CONTENT+="pkgfs_overlay_dir = \"@CONF_PKGFS_OVERLAY_DIR@\""$'\n'
        fi
        if config_is_set CONF_DISABLE_PKGFS_INTEGRITY; then
            SSAMD_TOML_CONTENT+="disable_pkgfs_integrity = @CONF_DISABLE_PKGFS_INTEGRITY@"$'\n'
        fi
        if config_is_set CONF_DISABLE_PACKAGE_VERIFY; then
            SSAMD_TOML_CONTENT+="disable_package_verify = @CONF_DISABLE_PACKAGE_VERIFY@"$'\n'
        fi
    fi

    if config_is_set CONF_BRIDGE_ENABLED; then
        SSAMD_TOML_CONTENT+=$'\n[network.bridge]\n'
        SSAMD_TOML_CONTENT+="enabled = @CONF_BRIDGE_ENABLED@"$'\n'
        if config_is_set CONF_BRIDGE_POOL_BASE || config_is_set CONF_BRIDGE_POOL_SIZE; then
            SSAMD_TOML_CONTENT+=$'\n[network.bridge.addr_pool]\n'
            if config_is_set CONF_BRIDGE_POOL_BASE; then
                SSAMD_TOML_CONTENT+="base = \"@CONF_BRIDGE_POOL_BASE@\""$'\n'
            fi
            if config_is_set CONF_BRIDGE_POOL_SIZE; then
                SSAMD_TOML_CONTENT+="size = @CONF_BRIDGE_POOL_SIZE@"$'\n'
            fi
        fi
    fi

    if config_is_set CONF_RPC_ALLOWED_GIDS; then
        SSAMD_TOML_CONTENT+=$'\n[rpc]\n'
        SSAMD_TOML_CONTENT+="allowed_gids = [@CONF_RPC_ALLOWED_GIDS@]"$'\n'
    fi

    update_template_common "SSAMD_TOML_CONTENT" "$conf_output"
}

#=============================================================================
# Argument Parsing
#=============================================================================

# Parse command line arguments
parse_arguments() {
    temp_args=$(getopt -o h \
        --long help,output-path:,gen-apparmor-profile,gen-service-unit,ssamd-install-path:,oci-bundle-dir:,builtin-pkgs-dir:,downloaded-pkgs-dir:,pkgs-data-root:,pkgs-overlayfs-root:,pkgs-cgroup:,public-key-file:,packages-ext:,bridge-enabled:,bridge-pool-base:,bridge-pool-size:,disable-runtime-integrity:,disable-package-verify:,rpc-allowed-gids: \
        -n "$(basename "$0")" -- "$@") || error_exit "Invalid argument"

    eval set -- "$temp_args"

    while true; do
        case "$1" in
            -h|--help)
                usage
                exit 0
                ;;
            --output-path)
                validate_path_argument "$1" "$2" false false
                CONF_OUTPUT_PATH="$2"
                shift 2
                ;;
            --gen-apparmor-profile)
                GEN_APPARMOR_PROFILE="1"
                shift
                ;;
            --gen-service-unit)
                GEN_SERVICE_UNIT="1"
                shift
                ;;
            --ssamd-install-path)
                validate_path_argument "$1" "$2" true false
                SSAMD_CONFIG[CONF_SSAMD_INSTALL_PATH]="$2"
                shift 2
                ;;
            --oci-bundle-dir)
                validate_path_argument "$1" "$2" true false
                SSAMD_CONFIG[CONF_OCI_BUNDLE_DIR]="$2"
                shift 2
                ;;
            --builtin-pkgs-dir)
                validate_path_argument "$1" "$2" true true
                SSAMD_CONFIG[CONF_BUILTIN_PACKAGES_DIR]="$2"
                shift 2
                ;;
            --downloaded-pkgs-dir)
                validate_path_argument "$1" "$2" true true
                SSAMD_CONFIG[CONF_DOWNLOADED_PACKAGES_DIR]="$2"
                shift 2
                ;;
            --pkgs-data-root)
                validate_path_argument "$1" "$2" true false
                SSAMD_CONFIG[CONF_PACKAGES_DATA_ROOT]="$2"
                shift 2
                ;;
            --pkgs-overlayfs-root)
                validate_path_argument "$1" "$2" true true
                SSAMD_CONFIG[CONF_PKGFS_OVERLAY_DIR]="$2"
                shift 2
                ;;
            --pkgs-cgroup)
                SSAMD_CONFIG[CONF_PACKAGES_CGROUP]="$2"
                shift 2
                ;;
            --public-key-file)
                validate_path_argument "$1" "$2" true true
                SSAMD_CONFIG[CONF_PUBLIC_KEY_FILE_PATH]="$2"
                shift 2
                ;;
            --packages-ext)
                SSAMD_CONFIG[CONF_PACKAGES_EXT]="$2"
                shift 2
                ;;
            --bridge-enabled)
                case "$2" in
                    true|false) ;;
                    *) error_exit "--bridge-enabled must be 'true' or 'false', got: '$2'" ;;
                esac
                SSAMD_CONFIG[CONF_BRIDGE_ENABLED]="$2"
                shift 2
                ;;
            --bridge-pool-base)
                # Shape-validate: gets sed-substituted into quoted TOML as-is.
                if ! [[ "$2" =~ ^[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}/[0-9]{1,2}$ ]]; then
                    error_exit "--bridge-pool-base must be an IPv4 CIDR (e.g. 172.20.0.0/16), got: '$2'"
                fi
                IFS='./' read -r o1 o2 o3 o4 prefix <<< "$2"
                for octet in "$o1" "$o2" "$o3" "$o4"; do
                    if [ "$octet" -gt 255 ]; then
                        error_exit "--bridge-pool-base has an octet > 255, got: '$2'"
                    fi
                done
                if [ "$prefix" -gt 32 ]; then
                    error_exit "--bridge-pool-base prefix must be <= 32, got: '$2'"
                fi
                SSAMD_CONFIG[CONF_BRIDGE_POOL_BASE]="$2"
                shift 2
                ;;
            --bridge-pool-size)
                # Bare TOML int: reject non-ints (unparsable TOML panics ssamd at
                # init); /31,/32 leave no usable host, so cap at 30.
                if ! [[ "$2" =~ ^[0-9]+$ ]] || [ "$2" -lt 1 ] || [ "$2" -gt 30 ]; then
                    error_exit "--bridge-pool-size must be an integer in [1, 30], got: '$2'"
                fi
                SSAMD_CONFIG[CONF_BRIDGE_POOL_SIZE]="$2"
                shift 2
                ;;
            --disable-runtime-integrity)
                case "$2" in
                    true|false) ;;
                    *) error_exit "--disable-runtime-integrity must be 'true' or 'false', got: '$2'" ;;
                esac
                SSAMD_CONFIG[CONF_DISABLE_PKGFS_INTEGRITY]="$2"
                shift 2
                ;;
            --disable-package-verify)
                case "$2" in
                    true|false) ;;
                    *) error_exit "--disable-package-verify must be 'true' or 'false', got: '$2'" ;;
                esac
                SSAMD_CONFIG[CONF_DISABLE_PACKAGE_VERIFY]="$2"
                shift 2
                ;;
            --rpc-allowed-gids)
                if [[ -n "${RPC_ALLOWED_GIDS_SET:-}" ]]; then
                    error_exit "--rpc-allowed-gids may be specified only once"
                fi
                validate_rpc_allowed_gids "$2"
                SSAMD_CONFIG[CONF_RPC_ALLOWED_GIDS]="$RPC_ALLOWED_GIDS_NORMALIZED"
                RPC_ALLOWED_GIDS_SET=1
                shift 2
                ;;
            --)
                shift
                break
                ;;
            *)
                error_exit "Internal error during argument parsing"
                ;;
        esac
    done

    # Check for unexpected positional arguments
    if [ $# -gt 0 ]; then
        error_exit "Unexpected positional argument: $1"
    fi
}

# Validate the arguments against the serde configuration structures in
# ssamd/src/configuration.rs.
validate_configuration() {
    if ! config_is_set CONF_OCI_BUNDLE_DIR; then
        error_exit "--oci-bundle-dir is required."
    fi
    if ! config_is_set CONF_PACKAGES_DATA_ROOT; then
        error_exit "--pkgs-data-root is required."
    fi

    if [[ -z "${SSAMD_CONFIG[CONF_BUILTIN_PACKAGES_DIR]-}" &&
          -z "${SSAMD_CONFIG[CONF_DOWNLOADED_PACKAGES_DIR]-}" ]]; then
        error_exit "At least one of --builtin-pkgs-dir and --downloaded-pkgs-dir must be configured"
    fi

    if config_is_set CONF_PUBLIC_KEY_FILE_PATH &&
        [[ -z "${SSAMD_CONFIG[CONF_PUBLIC_KEY_FILE_PATH]}" ]] &&
        [[ "${SSAMD_CONFIG[CONF_DISABLE_PACKAGE_VERIFY]-false}" != "true" ]]; then
        error_exit "--public-key-file must not be empty when package verification is enabled"
    fi

    if (config_is_set CONF_BRIDGE_POOL_BASE ||
        config_is_set CONF_BRIDGE_POOL_SIZE) &&
        ! config_is_set CONF_BRIDGE_ENABLED; then
        error_exit "--bridge-enabled is required when a bridge address pool is configured"
    fi

    if [[ -n "$GEN_APPARMOR_PROFILE" || -n "$GEN_SERVICE_UNIT" ]] &&
        ! config_is_set CONF_SSAMD_INSTALL_PATH; then
        error_exit "--ssamd-install-path is required when generating an AppArmor profile or service unit"
    fi
}

normalize_package_directories() {
    local bdir ddir

    if config_is_set CONF_BUILTIN_PACKAGES_DIR; then
        if [[ -n "${SSAMD_CONFIG[CONF_BUILTIN_PACKAGES_DIR]}" ]]; then
            bdir=$(realpath -m "${SSAMD_CONFIG[CONF_BUILTIN_PACKAGES_DIR]}")
        else
            bdir=""
        fi
        SSAMD_CONFIG[CONF_BUILTIN_PACKAGES_DIR]="$bdir"
    fi

    if config_is_set CONF_DOWNLOADED_PACKAGES_DIR; then
        if [[ -n "${SSAMD_CONFIG[CONF_DOWNLOADED_PACKAGES_DIR]}" ]]; then
            ddir=$(realpath -m "${SSAMD_CONFIG[CONF_DOWNLOADED_PACKAGES_DIR]}")
        else
            ddir=""
        fi
        SSAMD_CONFIG[CONF_DOWNLOADED_PACKAGES_DIR]="$ddir"
    fi

    if config_is_set CONF_BUILTIN_PACKAGES_DIR &&
        [[ -n "${SSAMD_CONFIG[CONF_BUILTIN_PACKAGES_DIR]}" ]] &&
        [[ "${SSAMD_CONFIG[CONF_BUILTIN_PACKAGES_DIR]}" == "${SSAMD_CONFIG[CONF_DOWNLOADED_PACKAGES_DIR]-}" ]]; then
        error_exit "builtin-pkgs-dir ('${SSAMD_CONFIG[CONF_BUILTIN_PACKAGES_DIR]}') and downloaded-pkgs-dir ('$ddir') must not be the same directory"
    fi
}

#=============================================================================
# Main Execution
#=============================================================================

main() {
    # Parse command line arguments
    parse_arguments "$@"

    normalize_package_directories
    validate_configuration

    display_configuration

    # Generate outputs
    generate_ssamd_config

    if [[ -n "$GEN_APPARMOR_PROFILE" ]]; then
        generate_apparmor_profile
    fi

    if [[ -n "$GEN_SERVICE_UNIT" ]]; then
        generate_service_unit
    fi
}

TEMPLATE_SSAMD_SERVICE_UNIT="
[Unit]
Description=SSAM Daemon
DefaultDependencies=no
RequiresMountsFor=@CONF_REQUIRES_MOUNTS@
After=dbus.socket
#Requires=

[Service]
Type=notify
ExecStart=@CONF_SSAMD_INSTALL_PATH@
#User=
#SupplementaryGroups=

[Install]
WantedBy=multi-user.target
"

TEMPLATE_SSAMD_APPARMOR="
abi <abi/3.0>,

include <tunables/global>

@CONF_SSAMD_INSTALL_PATH@ {
  include <abstractions/base>
  include <abstractions/user-tmp>
  include <abstractions/apparmor_api/is_enabled>
  include <abstractions/apparmor_api/find_mountpoint>

  capability sys_admin,
  network unix stream,
  network unix dgram,
  network netlink dgram,

  mount -> @APPARMOR_NORMALIZED_OCI_BUNDLE_DIR@/*/pkgfs/,
  umount @APPARMOR_NORMALIZED_OCI_BUNDLE_DIR@/*/pkgfs/,

  @CONF_SSAMD_INSTALL_PATH@ mr,
  /dev/loop* rw,
  /dev/mapper/control rw,
  @{PROC}*/cgroup r,
  @{PROC}*/mounts r,
  @{PROC}*/mountinfo r,
  /sys/fs/cgroup/** r,
  @CONF_SSAMD_TOML_PATH@ r,
@APPARMOR_PUBLIC_KEY_RULE@
  @CONF_PACKAGES_DATA_ROOT@/ rw,
  @CONF_PACKAGES_DATA_ROOT@/** rw,
@APPARMOR_PERMISSION_OCI_BUNDLE_DIR@
@APPARMOR_PERMISSION_PACKAGES_DIR@
}

profile container-default flags=(attach_disconnected,mediate_deleted) {
    include <abstractions/base>

    network,
    capability,
    file,
    umount,

    signal (receive) peer=unconfined,
    signal (receive) peer=runc,
    signal (receive) peer=crun,
    signal (send,receive) peer=container-default,

    deny @{PROC}/* w,
    deny @{PROC}/{[^1-9],[^1-9][^0-9],[^1-9s][^0-9y][^0-9s],[^1-9][^0-9][^0-9][^0-9/]*}/** w,
    deny @{PROC}/sys/[^k]** w,
    deny @{PROC}/sys/kernel/{?,??,[^s][^h][^m]**} w,
    deny @{PROC}/sysrq-trigger rwklx,
    deny @{PROC}/kcore rwklx,

    deny mount,
    deny /sys/[^f]*/** wklx,
    deny /sys/f[^s]*/** wklx,
    deny /sys/fs/[^c]*/** wklx,
    deny /sys/fs/c[^g]*/** wklx,
    deny /sys/fs/cg[^r]*/** wklx,
    deny /sys/firmware/** rwklx,
    deny /sys/devices/virtual/powercap/** rwklx,
    deny /sys/kernel/security/** rwklx,

    ptrace (trace,read,tracedby,readby) peer=container-default,
}
"

# Execute main function
main "$@"
