#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
ONCE=false

case ${1:-} in
    --once)
        ONCE=true
        ;;
    --help|-h)
        echo "Usage: $0 [--once]"
        echo "Watch for DS4 hidraw devices and create/update model rules."
        exit 0
        ;;
    '')
        ;;
    *)
        echo "Usage: $0 [--once]" >&2
        exit 2
        ;;
esac

if ! command -v modinfo >/dev/null 2>&1; then
    echo "modinfo is required (Arch package: kmod)." >&2
    exit 1
fi

missing=()
for module in hidp hid_playstation; do
    if modinfo "$module" >/dev/null 2>&1; then
        echo "Driver available: $module"
    else
        missing+=("$module")
    fi
done

if (( ${#missing[@]} > 0 )); then
    echo "Missing kernel drivers: ${missing[*]}" >&2
    echo "Install a kernel package that provides Bluetooth HID and PlayStation HID support." >&2
    exit 1
fi

if ! modinfo hid_playstation | grep -i 'v0000054C' >/dev/null; then
    echo "hid_playstation does not advertise Sony PlayStation controller support." >&2
    exit 1
fi

if ! lsmod | grep '^hidp ' >/dev/null || ! lsmod | grep '^hid_playstation ' >/dev/null; then
    read -r -p "Load hidp and hid_playstation now? [Y/n] " answer
    if [[ ! ${answer:-} =~ ^[Nn]$ ]]; then
        sudo modprobe hidp hid_playstation
    fi
fi

if [[ -x "$ROOT_DIR/target/release/ds4-sound-link" ]]; then
    discovery="$ROOT_DIR/target/release/ds4-sound-link"
elif command -v ds4-sound-link >/dev/null 2>&1; then
    discovery=$(command -v ds4-sound-link)
else
    echo "Build the project first: cargo build --release --locked" >&2
    exit 1
fi

output_dir="$ROOT_DIR/udev/generated"
if [[ $ONCE == false ]]; then
    read -r -p "Rule output directory [$output_dir]: " custom_output
    output_dir=${custom_output:-$output_dir}
fi
mkdir -p "$output_dir"

update_rule() {
    local model=$1
    local pid=$2
    local mac_list=$3
    local output="$output_dir/70-ds4-${model,,}.rules"
    local temporary="$output.tmp.$$"

    cat > "$temporary" <<EOF
# DualShock 4 $model (054c:$pid), controllers: $mac_list
SUBSYSTEM=="hidraw", KERNEL=="hidraw[0-9]*", ATTRS{modalias}=="hid:b0005g*v0000054Cp0000${pid^^}", MODE="0660", TAG+="uaccess", TAG+="systemd", ENV{SYSTEMD_USER_WANTS}+="ds4-sound-link.service"

# USB permission for the same model
SUBSYSTEM=="hidraw", KERNEL=="hidraw[0-9]*", ATTRS{idVendor}=="054c", ATTRS{idProduct}=="$pid", MODE="0660", TAG+="uaccess"
EOF

    if [[ -f $output ]] && cmp -s "$temporary" "$output"; then
        rm -f "$temporary"
        echo "Unchanged: $output"
    else
        udevadm verify "$temporary"
        mv "$temporary" "$output"
        echo "Updated: $output"
    fi
}

refresh_controllers() {
    local rows=()
    local device pid mac model
    local found=0
    declare -A model_pids=()
    declare -A model_macs=()

    mapfile -t rows < <("$discovery" --list-controllers | awk '/^\/dev\/hidraw/')
    for row in "${rows[@]}"; do
        read -r device pid mac _ <<< "$row"
        pid=${pid,,}
        case "$pid" in
            09cc) model=CUH-ZCT2E ;;
            05c4) model=CUH-ZCT1 ;;
            05c5) model=DS4-VARIANT ;;
            *)
                echo "Skipping unsupported product ID $pid on $device" >&2
                continue
                ;;
        esac
        found=$((found + 1))
        printf 'Detected: %s | 054c:%s | %s | %s\n' "$model" "$pid" "$mac" "$device"
        model_pids[$model]=$pid
        if [[ -v model_macs[$model] ]]; then
            if [[ ",${model_macs[$model]}," != *",$mac,"* ]]; then
                model_macs[$model]="${model_macs[$model]}, $mac"
            fi
        else
            model_macs[$model]=$mac
        fi
    done

    for model in CUH-ZCT2E CUH-ZCT1 DS4-VARIANT; do
        if [[ -v model_pids[$model] ]]; then
            update_rule "$model" "${model_pids[$model]}" "${model_macs[$model]}"
        fi
    done

    if (( found == 0 )); then
        echo "No connected DS4 controllers; existing rules were left in place."
    fi
    return 0
}

refresh_controllers

if [[ $ONCE == true ]]; then
    echo "Install generated rules with:"
    echo "  sudo install -Dm644 '$output_dir/'*.rules /etc/udev/rules.d/"
    echo "  sudo udevadm control --reload-rules"
    exit 0
fi

echo "Watching for new DS4 devices. Existing rules will be updated; press Ctrl+C to stop."
udevadm monitor --udev --subsystem-match=hidraw | while IFS= read -r event; do
    case "$event" in
        *' add '*|*' remove '*)
            echo "$event"
            sleep 0.2
            refresh_controllers
            ;;
    esac
done
