#!/bin/bash
# Copyright 2025-2026 Hyundai Mobis Co., Ltd.
# SPDX-License-Identifier: Apache-2.0

# Configuration Files Generator for SSAMD
# This script generates SSAMD configuration files including the AppArmor
# profile and the systemd service unit from templates by replacing placeholders
# in templates with defaults or user-provided values.

set -e

#=============================================================================
# Configuration Variables
#=============================================================================

# Default output path of generated files is the current working directory
CONF_OUTPUT_PATH="$PWD"

# Flags to enable generating additional configuration files
unset GEN_APPARMOR_PROFILE
unset GEN_SERVICE_UNIT

# Configuration values with defaults
declare -A SSAMD_CONFIG
SSAMD_CONFIG=(
    [CONF_SSAMD_INSTALL_PATH]="/usr/bin/ssamd"
    [CONF_PACKAGES_MNT_ROOT]="/var/run/ssamd/mnt"
    [CONF_BUNDLED_PACKAGES_DIR]="/var/lib/ssamd/bundled"
    [CONF_DOWNLOADED_PACKAGES_DIR]="/var/lib/ssamd/downloaded"
    [CONF_PACKAGES_DATA_ROOT]="/var/lib/ssamd/data"
    [CONF_PUBLIC_KEY_FILE_PATH]="/etc/ssamd/public.key"
    [CONF_PACKAGES_EXT]="ssam"
    [CONF_RPC_BIND_IP]="127.0.0.1"
    [CONF_PACKAGES_CGROUP]=""
    [CONF_PACKAGES_OVERLAYFS_ROOT]=""
    [CONF_BRIDGE_ENABLED]="false"
    [CONF_BRIDGE_POOL_BASE]="172.20.0.0/16"
    [CONF_BRIDGE_POOL_SIZE]="29"
)

error_exit() {
    echo "Error: $1" >&2
    echo "Refer to '$(basename "$0") --help' for usage information." >&2
    exit 1
}

usage() {
    cat << EOF
Usage: $(basename "$0") [OPTIONS]

* Generates the ssam daemon configuration file - ssamd.toml.
* Optionally, the script can generate other configuration files, such as the
  Systemd service unit file.
* Since all arguments are optional, simply executing the script will generate
  ssamd.toml with default configuration values.

Arguments:
    -h, --help                  Display this help message

Output Arguments:
    --output-path PATH      Path where the generated files will be located.
                            If not specified, current working directory is used.
    --gen-service-unit      Enable generating ssamd.service
    --gen-apparmor-profile  Enable generating AppArmor profile for SSAMD

Arguments for SSAMD configuration (All paths below are on the target device)
    --ssamd-install-path   PATH  SSAMD installation path
                                 (default: ${SSAMD_CONFIG[CONF_SSAMD_INSTALL_PATH]})
    --pkgs-mnt-root        PATH  Package mount root directory.
                                 (default: "${SSAMD_CONFIG[CONF_PACKAGES_MNT_ROOT]}")
    --bundled-pkgs-dir     PATH  Bundled (read-only) package directory path.
                                 (default: "${SSAMD_CONFIG[CONF_BUNDLED_PACKAGES_DIR]}")
    --downloaded-pkgs-dir  PATH  Downloaded (read-write) package directory path.
                                 (default: "${SSAMD_CONFIG[CONF_DOWNLOADED_PACKAGES_DIR]}")
    --pkgs-data-root       PATH  Package data root directory.
                                 (default: "${SSAMD_CONFIG[CONF_PACKAGES_DATA_ROOT]}")
    --pkgs-overlayfs-root  PATH  OverlayFS root directory.
                                 (default: "${SSAMD_CONFIG[CONF_PACKAGES_OVERLAYFS_ROOT]}")
    --pkgs-cgroup          NAME  Cgroup name for package isolation.
                                 (default: "${SSAMD_CONFIG[CONF_PACKAGES_CGROUP]}")
    --public-key-file      PATH  Public key file path for package verification.
                                 (default: "${SSAMD_CONFIG[CONF_PUBLIC_KEY_FILE_PATH]}")
    --packages-ext         EXT   Package file extension without a dot.
                                 (default: "${SSAMD_CONFIG[CONF_PACKAGES_EXT]}")
    --rpc-bind-ip          IP    IP address to bind the ssamd RPC server
                                 (default: "${SSAMD_CONFIG[CONF_RPC_BIND_IP]}")
    --bridge-enabled       BOOL  Enable per-container bridge networking.
                                 (default: ${SSAMD_CONFIG[CONF_BRIDGE_ENABLED]})
    --bridge-pool-base     CIDR  IP pool bridges carve container subnets from.
                                 (default: "${SSAMD_CONFIG[CONF_BRIDGE_POOL_BASE]}")
    --bridge-pool-size     BITS  Prefix size of each per-bridge subnet.
                                 (default: ${SSAMD_CONFIG[CONF_BRIDGE_POOL_SIZE]})

Example - Generate all available configuration files:
    $(basename "$0") \\
        --output-path . \\
        --gen-service-unit \\
        --gen-apparmor-profile \\
        --ssamd-install-path ${SSAMD_CONFIG[CONF_SSAMD_INSTALL_PATH]} \\
        --pkgs-mnt-root ${SSAMD_CONFIG[CONF_PACKAGES_MNT_ROOT]} \\
        --bundled-pkgs-dir /var/lib/ssamd/bundled \\
        --downloaded-pkgs-dir /var/lib/ssamd/downloaded \\
        --pkgs-data-root ${SSAMD_CONFIG[CONF_PACKAGES_DATA_ROOT]} \\
        --public-key-file ${SSAMD_CONFIG[CONF_PUBLIC_KEY_FILE_PATH]} \\
        --packages-ext ${SSAMD_CONFIG[CONF_PACKAGES_EXT]} \\
        --pkgs-overlayfs-root /var/lib/ssamd/data/overlayfs \\
        --pkgs-cgroup ssamd.slice \\
        --rpc-bind-ip 192.168.63.100

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
        -e "s|@CONF_SSAMD_INSTALL_PATH@|${SSAMD_CONFIG[CONF_SSAMD_INSTALL_PATH]}|g" \
        -e "s|@CONF_BUNDLED_PACKAGES_DIR@|${SSAMD_CONFIG[CONF_BUNDLED_PACKAGES_DIR]}|g" \
        -e "s|@CONF_DOWNLOADED_PACKAGES_DIR@|${SSAMD_CONFIG[CONF_DOWNLOADED_PACKAGES_DIR]}|g" \
        -e "s|@CONF_PACKAGES_DATA_ROOT@|${SSAMD_CONFIG[CONF_PACKAGES_DATA_ROOT]}|g" \
        -e "s|@CONF_PACKAGES_OVERLAYFS_ROOT@|${SSAMD_CONFIG[CONF_PACKAGES_OVERLAYFS_ROOT]}|g" \
        -e "s|@CONF_PACKAGES_MNT_ROOT@|${SSAMD_CONFIG[CONF_PACKAGES_MNT_ROOT]}|g" \
        -e "s|@CONF_PUBLIC_KEY_FILE_PATH@|${SSAMD_CONFIG[CONF_PUBLIC_KEY_FILE_PATH]}|g" \
        -e "s|@CONF_PACKAGES_CGROUP@|${SSAMD_CONFIG[CONF_PACKAGES_CGROUP]}|g" \
        -e "s|@CONF_PACKAGES_EXT@|${SSAMD_CONFIG[CONF_PACKAGES_EXT]}|g" \
        -e "s|@CONF_RPC_BIND_IP@|${SSAMD_CONFIG[CONF_RPC_BIND_IP]}|g" \
        -e "s|@CONF_BRIDGE_ENABLED@|${SSAMD_CONFIG[CONF_BRIDGE_ENABLED]}|g" \
        -e "s|@CONF_BRIDGE_POOL_BASE@|${SSAMD_CONFIG[CONF_BRIDGE_POOL_BASE]}|g" \
        -e "s|@CONF_BRIDGE_POOL_SIZE@|${SSAMD_CONFIG[CONF_BRIDGE_POOL_SIZE]}|g" \
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
#   - For input /run/ssamd/mnt or /var/run/ssamd/mnt:
#     @{run}{ssamd/,ssamd/mnt/} w,
#     @{run}ssamd/mnt/*/ w,
#   - For input /var/lib/ssamd:
#     /var/{lib/,lib/ssamd/} w,
#     /var/lib/ssamd/*/ w,
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

    # Put wildcard at the end of whole path only to allow creating random
    # subdirectories per container name
    result+=("  ${path}/*/ w,")

    # Put \n at the end of each policy
    printf "%s\n" "${result[@]}"
}

generate_apparmor_profile() {
    local profile_filename conf_output conf_ssamd_toml_path
    conf_ssamd_toml_path="$(dirname "${SSAMD_CONFIG[CONF_SSAMD_INSTALL_PATH]}")/ssamd.toml"
    # Get a valid filename for the AppArmor profile from the install path
    # Replace / with . to derive the expected filename for the AppArmor profile
    profile_filename="${SSAMD_CONFIG[CONF_SSAMD_INSTALL_PATH]//\//.}"
    # Remove leading dot since CONF_SSAMD_INSTALL_PATH always starts with '/'
    profile_filename="${profile_filename:1}"
    conf_output="${CONF_OUTPUT_PATH}/${profile_filename}"
    echo "Generating AppArmor profile - $conf_output"

    update_template_common "TEMPLATE_SSAMD_APPARMOR" "$conf_output"

    # Grant permissions per package directory: bundled=read-only, downloaded=read-write
    local formatted_pkg_dir=""
    # Bundled directory: read-only access (system-provided packages)
    if [[ -n "${SSAMD_CONFIG[CONF_BUNDLED_PACKAGES_DIR]}" ]]; then
        local bdir="${SSAMD_CONFIG[CONF_BUNDLED_PACKAGES_DIR]}"
        formatted_pkg_dir="${formatted_pkg_dir}  ${bdir}/ r,\n"
        formatted_pkg_dir="${formatted_pkg_dir}  ${bdir}/*.${SSAMD_CONFIG[CONF_PACKAGES_EXT]} r,\n"
    fi
    # Downloaded directory: read-write access (user-installed packages)
    local ddir="${SSAMD_CONFIG[CONF_DOWNLOADED_PACKAGES_DIR]}"
    if [[ -n "$ddir" ]]; then
        formatted_pkg_dir="${formatted_pkg_dir}  ${ddir}/ rw,\n"
        formatted_pkg_dir="${formatted_pkg_dir}  ${ddir}/*.${SSAMD_CONFIG[CONF_PACKAGES_EXT]} rw,\n"
    fi
    # Remove trailing newline
    formatted_pkg_dir="${formatted_pkg_dir%\\n}"

    # Special treatment to grant write access to let ssamd create
    # ${SSAMD_CONFIG[CONF_PACKAGES_MNT_ROOT]} with mkdir -p at runtime
    local mnt_root_permissions
    mnt_root_permissions=$(apparmor_gen_mkdir_policy "${SSAMD_CONFIG[CONF_PACKAGES_MNT_ROOT]}")
    if [[ -n "$mnt_root_permissions" ]]; then
        # Using gawk for in-place editing with multi-line replacement
        gawk -i inplace -v replacement="$mnt_root_permissions" \
            '{gsub(/@APPARMOR_PERMISSION_PACKAGES_MNT_ROOT@/, replacement); print}' \
            "$conf_output"
    else
        error_exit "Cannot generate permissions for package mount root '${SSAMD_CONFIG[CONF_PACKAGES_MNT_ROOT]}'."
        exit 1
    fi

    # Replace package directories (multi-line)
    # Using gawk for in-place editing with multi-line replacement
    gawk -i inplace -v replacement="$formatted_pkg_dir" \
        '{gsub(/@APPARMOR_PERMISSION_PACKAGES_DIR@/, replacement); print}' \
        "$conf_output"

    sed --in-place \
        -e "s|@CONF_SSAMD_TOML_PATH@|${conf_ssamd_toml_path}|g" \
        -e "s|@APPARMOR_NORMALIZED_PACKAGES_MNT_ROOT@|$(apparmor_normalize_path "${SSAMD_CONFIG[CONF_PACKAGES_MNT_ROOT]}")|g" \
        "$conf_output"
}

generate_service_unit() {
    local conf_output
    conf_output="${CONF_OUTPUT_PATH}/ssamd.service"

    echo "Generating Service unit - ${conf_output}..."
    update_template_common "TEMPLATE_SSAMD_SERVICE_UNIT" "$conf_output"
}

# Generate SSAMD configuration
generate_ssamd_config() {
    local conf_output="${CONF_OUTPUT_PATH}/ssamd.toml"
    echo "Generating ssamd configuration - ${conf_output}..."
    update_template_common "TEMPLATE_SSAMD_TOML" "$conf_output"
}

#=============================================================================
# Argument Parsing
#=============================================================================

# Parse command line arguments
parse_arguments() {
    temp_args=$(getopt -o h \
        --long help,output-path:,gen-apparmor-profile,gen-service-unit,ssamd-install-path:,pkgs-mnt-root:,bundled-pkgs-dir:,downloaded-pkgs-dir:,pkgs-data-root:,pkgs-overlayfs-root:,pkgs-cgroup:,public-key-file:,packages-ext:,rpc-bind-ip:,bridge-enabled:,bridge-pool-base:,bridge-pool-size: \
        -n "$(basename "$0")" -- "$@") || error_exit "Invalid argument"

    eval set -- "$temp_args"

    while true; do
        case "$1" in
            -h|--help)
                usage
                exit 0
                ;;
            --output-path)
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
                if [[ "${2:0:1}" != "/" ]]; then
                    error_exit "Argument --ssamd-install-path requires an absolute path. Got: '$2'"
                fi
                SSAMD_CONFIG[CONF_SSAMD_INSTALL_PATH]="$2"
                shift 2
                ;;
            --pkgs-mnt-root)
                SSAMD_CONFIG[CONF_PACKAGES_MNT_ROOT]="$2"
                shift 2
                ;;
            --bundled-pkgs-dir)
                if [[ "${2:0:1}" != "/" ]]; then
                    error_exit "Argument --bundled-pkgs-dir requires an absolute path. Got: '$2'"
                fi
                SSAMD_CONFIG[CONF_BUNDLED_PACKAGES_DIR]="$2"
                shift 2
                ;;
            --downloaded-pkgs-dir)
                if [[ "${2:0:1}" != "/" ]]; then
                    error_exit "Argument --downloaded-pkgs-dir requires an absolute path. Got: '$2'"
                fi
                SSAMD_CONFIG[CONF_DOWNLOADED_PACKAGES_DIR]="$2"
                shift 2
                ;;
            --pkgs-data-root)
                SSAMD_CONFIG[CONF_PACKAGES_DATA_ROOT]="$2"
                shift 2
                ;;
            --pkgs-overlayfs-root)
                SSAMD_CONFIG[CONF_PACKAGES_OVERLAYFS_ROOT]="$2"
                shift 2
                ;;
            --pkgs-cgroup)
                SSAMD_CONFIG[CONF_PACKAGES_CGROUP]="$2"
                shift 2
                ;;
            --public-key-file)
                SSAMD_CONFIG[CONF_PUBLIC_KEY_FILE_PATH]="$2"
                shift 2
                ;;
            --packages-ext)
                SSAMD_CONFIG[CONF_PACKAGES_EXT]="$2"
                shift 2
                ;;
            --rpc-bind-ip)
                SSAMD_CONFIG[CONF_RPC_BIND_IP]="$2"
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

#=============================================================================
# Main Execution
#=============================================================================

main() {
    # Parse command line arguments
    parse_arguments "$@"

    local bdir ddir
    bdir=$(realpath -m "${SSAMD_CONFIG[CONF_BUNDLED_PACKAGES_DIR]}")
    ddir=$(realpath -m "${SSAMD_CONFIG[CONF_DOWNLOADED_PACKAGES_DIR]}")
    SSAMD_CONFIG[CONF_BUNDLED_PACKAGES_DIR]="$bdir"
    SSAMD_CONFIG[CONF_DOWNLOADED_PACKAGES_DIR]="$ddir"
    if [[ -n "$bdir" && -n "$ddir" && "$bdir" == "$ddir" ]]; then
        error_exit "bundled-pkgs-dir ('$bdir') and downloaded-pkgs-dir ('$ddir') must not be the same directory"
    fi

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

TEMPLATE_SSAMD_TOML="
[common]
bundled_packages_dir = \"@CONF_BUNDLED_PACKAGES_DIR@\"
downloaded_packages_dir = \"@CONF_DOWNLOADED_PACKAGES_DIR@\"
packages_data_root = \"@CONF_PACKAGES_DATA_ROOT@\"
packages_overlayfs_root = \"@CONF_PACKAGES_OVERLAYFS_ROOT@\"
packages_mnt_root = \"@CONF_PACKAGES_MNT_ROOT@\"
public_key_file_path = \"@CONF_PUBLIC_KEY_FILE_PATH@\"
packages_cgroup = \"@CONF_PACKAGES_CGROUP@\"
packages_ext = \"@CONF_PACKAGES_EXT@\"
rpc_bind_ip = \"@CONF_RPC_BIND_IP@\"

[network.bridge]
enabled = @CONF_BRIDGE_ENABLED@

[network.bridge.addr_pool]
base = \"@CONF_BRIDGE_POOL_BASE@\"
size = @CONF_BRIDGE_POOL_SIZE@
"

TEMPLATE_SSAMD_SERVICE_UNIT="
[Unit]
Description=SSAM Daemon
DefaultDependencies=no
RequiresMountsFor=/tmp @CONF_BUNDLED_PACKAGES_DIR@ @CONF_DOWNLOADED_PACKAGES_DIR@ @CONF_PACKAGES_DATA_ROOT@ @CONF_PACKAGES_MNT_ROOT@
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
  network inet tcp,

  mount -> @APPARMOR_NORMALIZED_PACKAGES_MNT_ROOT@/*/,
  umount @APPARMOR_NORMALIZED_PACKAGES_MNT_ROOT@/*/,

  @CONF_SSAMD_INSTALL_PATH@ mr,
  /dev/loop* rw,
  /dev/mapper/control rw,
  @{PROC}*/cgroup r,
  @{PROC}*/mounts r,
  @{PROC}*/mountinfo r,
  /sys/fs/cgroup/** r,
  @CONF_SSAMD_TOML_PATH@ r,
  @CONF_PUBLIC_KEY_FILE_PATH@ r,
  @CONF_PACKAGES_DATA_ROOT@/ rw,
  @CONF_PACKAGES_DATA_ROOT@/** rw,
@APPARMOR_PERMISSION_PACKAGES_MNT_ROOT@
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
