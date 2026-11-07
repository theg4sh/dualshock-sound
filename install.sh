#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
SERVICE_NAME=ds4-sound-link.service
BINARY_NAME=ds4-sound-link

usage() {
    echo "Usage: $0 [--install|--uninstall] [--node-latency FRAMES/RATE]"
}

if (( EUID == 0 )); then
    echo "Run this installer as your regular desktop user, not as root." >&2
    exit 1
fi

action=install
node_latency=
while (( $# > 0 )); do
    case $1 in
        --help|-h)
            usage
            exit 0
            ;;
        --install)
            action=install
            shift
            ;;
        --uninstall)
            action=uninstall
            shift
            ;;
        --node-latency)
            if (( $# < 2 )); then
                echo "--node-latency requires FRAMES/RATE" >&2
                exit 2
            fi
            node_latency=$2
            shift 2
            ;;
        --node-latency=*)
            node_latency=${1#*=}
            shift
            ;;
        *)
            usage >&2
            exit 2
            ;;
    esac
done

if [[ -n $node_latency && ! $node_latency =~ ^[1-9][0-9]*/[1-9][0-9]*$ ]]; then
    echo "--node-latency must use positive FRAMES/RATE values" >&2
    exit 2
fi

if [[ $action == uninstall ]]; then
    if [[ -n $node_latency ]]; then
        echo "--node-latency cannot be combined with --uninstall" >&2
        exit 2
    fi
    systemctl --user stop "$SERVICE_NAME" 2>/dev/null || true
    sudo -v
    sudo rm -f \
        "/usr/local/bin/$BINARY_NAME" \
        "/etc/systemd/user/$SERVICE_NAME" \
        /etc/udev/rules.d/50-playstation-hid-modules.rules \
        /etc/udev/rules.d/70-ds4-cuh-zct2e.rules
    sudo udevadm control --reload-rules
    sudo udevadm trigger --action=change --subsystem-match=hidraw
    systemctl --user daemon-reload
    systemctl --user reset-failed "$SERVICE_NAME" 2>/dev/null || true
    echo "DS4 Sound Link removed. Packages, Bluetooth, and kernel modules were left unchanged."
    exit 0
fi

missing=()
for command in cc cargo clang pkg-config bluetoothctl modinfo modprobe udevadm; do
    if ! command -v "$command" >/dev/null 2>&1; then
        missing+=("$command")
    fi
done
if ! pkg-config --exists libpipewire-0.3; then
    missing+=("PipeWire development files")
fi
if ! pkg-config --exists libpulse-simple; then
    missing+=("PulseAudio client development files")
fi
if (( ${#missing[@]} > 0 )); then
    printf 'Missing dependencies: %s\n' "${missing[*]}" >&2
    echo "Install the packages listed in $ROOT_DIR/README.md#dependencies and try again." >&2
    exit 1
fi

cargo build --release --locked --manifest-path "$ROOT_DIR/Cargo.toml"
sudo -v

sudo systemctl enable --now bluetooth.service
sudo modprobe hidp hid_playstation

sudo install -Dm755 \
    "$ROOT_DIR/target/release/$BINARY_NAME" \
    "/usr/local/bin/$BINARY_NAME"
sudo rm -f /etc/udev/rules.d/70-ds4-hidraw.rules
for rule in "$ROOT_DIR"/udev/*.rules; do
    sudo install -Dm644 "$rule" "/etc/udev/rules.d/$(basename -- "$rule")"
done
sudo install -Dm644 \
    "$ROOT_DIR/systemd/$SERVICE_NAME" \
    "/etc/systemd/user/$SERVICE_NAME"
if [[ -n $node_latency ]]; then
    sudo sed -i \
        "s|^ExecStart=.*|ExecStart=/usr/local/bin/$BINARY_NAME --node-latency $node_latency|" \
        "/etc/systemd/user/$SERVICE_NAME"
fi

sudo udevadm control --reload-rules
sudo udevadm trigger --action=change --subsystem-match=hidraw
systemctl --user daemon-reload

if systemctl --user is-active --quiet "$SERVICE_NAME"; then
    systemctl --user restart "$SERVICE_NAME"
elif "/usr/local/bin/$BINARY_NAME" --list-controllers | grep '^/dev/hidraw' >/dev/null; then
    systemctl --user start "$SERVICE_NAME"
else
    echo "No DS4 hidraw device detected. Pair or reconnect the controller to start the service."
fi

echo "DS4 Sound Link installed."
echo "Check status with: systemctl --user status $SERVICE_NAME"
echo "Audio outputs: DS4 Headphones and DS4 Speaker"
