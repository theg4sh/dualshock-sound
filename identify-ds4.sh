#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)

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

if ! modinfo hid_playstation | grep -i 'v0000054Cp000009CC' >/dev/null; then
    echo "hid_playstation does not advertise CUH-ZCT2E (054c:09cc) support." >&2
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

mapfile -t rows < <("$discovery" --list-controllers | awk '/^\/dev\/hidraw/')
if (( ${#rows[@]} == 0 )); then
    echo "No connected Bluetooth DS4 controllers found. Connect the controller and try again." >&2
    exit 1
fi

devices=()
pids=()
macs=()
models=()
for index in "${!rows[@]}"; do
    read -r device pid mac _ <<< "${rows[$index]}"
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
    devices+=("$device")
    pids+=("$pid")
    macs+=("$mac")
    models+=("$model")
done

if (( ${#devices[@]} == 0 )); then
    echo "No supported DS4 models found." >&2
    exit 1
fi

for index in "${!devices[@]}"; do
    printf '%d) %s | 054c:%s | %s | %s\n' \
        "$((index + 1))" "${models[$index]}" "${pids[$index]}" \
        "${macs[$index]}" "${devices[$index]}"
done

read -r -p "Select controller [1]: " selected
selected=${selected:-1}
if [[ ! $selected =~ ^[0-9]+$ ]] || (( selected < 1 || selected > ${#devices[@]} )); then
    echo "Invalid selection." >&2
    exit 1
fi
index=$((selected - 1))

output="$ROOT_DIR/udev/generated/70-ds4-${models[$index],,}.rules"
read -r -p "Rule output [$output]: " custom_output
output=${custom_output:-$output}

if [[ -e $output ]]; then
    read -r -p "Overwrite $output? [y/N] " overwrite
    if [[ ! ${overwrite:-} =~ ^[Yy]$ ]]; then
        echo "Cancelled."
        exit 0
    fi
fi

mkdir -p "$(dirname -- "$output")"
cat > "$output" <<EOF
# DualShock 4 ${models[$index]} (054c:${pids[$index]}), detected as ${macs[$index]}
SUBSYSTEM=="hidraw", KERNEL=="hidraw[0-9]*", ATTRS{modalias}=="hid:b0005g*v0000054Cp0000${pids[$index]^^}", MODE="0660", TAG+="uaccess", TAG+="systemd", ENV{SYSTEMD_USER_WANTS}+="ds4-sound-link.service"

# USB permission for the same model
SUBSYSTEM=="hidraw", KERNEL=="hidraw[0-9]*", ATTRS{idVendor}=="054c", ATTRS{idProduct}=="${pids[$index]}", MODE="0660", TAG+="uaccess"
EOF

udevadm verify "$output"
echo "Created $output"
echo "Install it with:"
echo "  sudo install -Dm644 '$output' '/etc/udev/rules.d/$(basename -- "$output")'"
echo "  sudo udevadm control --reload-rules"
