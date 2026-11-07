# DS4 Sound Link for Linux

Experimental Rust implementation of DualShock 4 Bluetooth audio streaming for Linux. It captures desktop audio, encodes it as SBC, and sends it through Sony's vendor-specific HID reports instead of A2DP.

The PipeWire integration exposes two separate output devices:

- **DS4 Headphones** (`ds4_sound_link_headphones`)
- **DS4 Speaker** (`ds4_sound_link_speaker`)

They are visible through `pactl`, desktop audio settings, and PipeWire patchbay applications. A single HID writer switches between them without allowing both outputs to conflict.

## Status and safety

**Provided as-is, without warranty of any kind. Use at your own risk.**

This is experimental software using reverse-engineered controller protocols. It may affect controller input, lighting, or audio behavior. Stop the service and disconnect the controller if it behaves unexpectedly.

Currently supported:

- DualShock 4 CUH-ZCT2E (`054c:09cc`) over Bluetooth.
- Linux with systemd, udev, PipeWire, WirePlumber, and `pipewire-pulse`.

DualSense, USB audio streaming, and other DS4 models are not enabled by the installed CUH-ZCT2E rule.

## Dependencies

The installer does not install distribution packages. Install the build and runtime dependencies before running it:

- A C compiler, Rust/Cargo, Clang/libclang, and `pkg-config`.
- PipeWire development files and PulseAudio client development files.
- PipeWire, WirePlumber, `pipewire-pulse`, BlueZ, systemd, and udev.
- A kernel providing the `hidp` and `hid_playstation` modules.

Package names for common distributions:

**Arch Linux**

```bash
sudo pacman -S --needed base-devel rust clang pkgconf \
  pipewire pipewire-pulse wireplumber libpulse bluez bluez-utils
```

**Debian / Ubuntu**

```bash
sudo apt install build-essential cargo rustc clang libclang-dev pkg-config \
  libpipewire-0.3-dev libpulse-dev pipewire pipewire-pulse wireplumber bluez
```

**Fedora**

```bash
sudo dnf install gcc make cargo rust clang clang-devel pkgconf-pkg-config \
  pipewire-devel pulseaudio-libs-devel pipewire-pulseaudio wireplumber bluez
```

Package names and PipeWire integration packages can differ between distribution releases. For other distributions, install the equivalent development and runtime packages listed above.

## Installation

Pair and connect the controller first, then run:

```bash
./install.sh
```

Run the installer as your regular desktop user, not as root. It uses `sudo` only for system files and services.

The installer:

1. Checks the required build dependencies.
2. Builds the Rust release binary.
3. Enables Bluetooth and loads `hidp` and `hid_playstation`.
4. Installs `/usr/local/bin/ds4-sound-link`.
5. Installs the CUH-ZCT2E udev rules and systemd user service.
6. Starts the service when a connected controller is available.

Reconnect the DS4 after installation so device activation can run. The service starts automatically when the controller's `hidraw` device appears.

Check the installation:

```bash
systemctl --user status ds4-sound-link.service
pactl list short sinks
```

## Audio routing

Select **DS4 Headphones** or **DS4 Speaker** in the desktop audio settings, or use:

```bash
pactl set-default-sink ds4_sound_link_headphones
pactl set-default-sink ds4_sound_link_speaker
```

Stop and inspect the service with:

```bash
systemctl --user stop ds4-sound-link.service
journalctl --user -u ds4-sound-link.service -f
```

## PipeWire latency

PipeWire selects the graph latency by default. Suggest a different value for both DS4 sinks with `--node-latency FRAMES/RATE`:

| Setting at 48 kHz | Buffer time |
| --- | ---: |
| `256/48000` | 5.33 ms |
| `512/48000` | 10.67 ms |
| `768/48000` | 16.00 ms |
| `1024/48000` | 21.33 ms |

For example:

```bash
ds4-sound-link --node-latency 512/48000
```

This is a PipeWire suggestion, not a guarantee; the graph can select a different quantum. Lower values can increase scheduling pressure and audio underruns.

### DS4 latency budget

Code inspection and runtime logs indicate the following additional delay beyond PipeWire scheduling:

- Initial priming is 20 SBC frames, or **80 ms**.
- One outgoing report adds up to **16 ms**.
- Typical steady-state buffering observed during hardware tests was approximately **48–80 ms**, including a report.
- The encoded queue ceiling is 96 frames, or **384 ms**, which can produce roughly **400 ms** of delay under sustained scheduling pressure.

Consequently, a normal DS4 output path is roughly **70–120 ms before Bluetooth transport**. This is an estimate from buffering, not an end-to-end hardware measurement. `--node-latency` does not change the SBC jitter buffer, Bluetooth delay, or optional fade duration.

### Tuning procedure

1. Disable fading for latency-sensitive testing and select a physical microphone in Steam. The `ds4_sound_link_*.monitor` sources are output loopbacks, not DS4 microphones.
2. Start with `512/48000`. If it is stable, try `256/48000`; if underruns or crackling appear, return to `512/48000` or try `768/48000`/`1024/48000`.
3. Compare the same microphone and Steam session with the built-in analog output and then the DS4 output. The difference isolates DS4 latency from Steam, network, and microphone latency.
4. Inspect scheduling and DS4 buffer health while audio is active:

   ```bash
   pw-top -b -n 5
   pactl list short sink-inputs
   pactl list short source-outputs
   journalctl --user -u ds4-sound-link.service -n 100 --no-pager
   ```

Avoid forcing a system-wide quantum for this feature. A global forced value also affects microphones and other applications and can make duplex performance worse.

To persist the setting in the installed service, pass it to the installer:

```bash
./install.sh --node-latency 512/48000
```

The installer writes the option into the service's `ExecStart` command. Reinstalling without `--node-latency` restores the server-selected default.

## Optional fading

Fading is disabled by default. Enable a 0.5-second start/stop envelope with:

```bash
ds4-sound-link --fade 0.5
```

Other nonnegative durations are supported. To add fading to the installed service, create a user-service override:

```bash
systemctl --user edit ds4-sound-link.service
```

Add:

```ini
[Service]
ExecStart=
ExecStart=/usr/local/bin/ds4-sound-link --fade 0.5
```

Then apply it:

```bash
systemctl --user daemon-reload
systemctl --user restart ds4-sound-link.service
```

## Controller identification and udev rules

Check kernel drivers and generate rules for the currently connected controller once:

```bash
./identify-ds4.sh --once
```

Watch for new controllers and update existing generated rules:

```bash
./identify-ds4.sh
```

Generated files are placed under `udev/generated/`. Existing rules are updated only when their contents change and remain in place when a controller disconnects.

The installed rules are split by responsibility:

- `udev/50-playstation-hid-modules.rules`: load `hidp` and `hid_playstation` when a Bluetooth adapter appears.
- `udev/70-ds4-cuh-zct2e.rules`: CUH-ZCT2E permissions and user-service activation.

## Direct and diagnostic modes

Running without arguments creates the two virtual PipeWire sinks. Other modes include:

```bash
# Capture the default desktop monitor directly instead of creating virtual sinks
ds4-sound-link --source @DEFAULT_MONITOR@ --speaker

# Play a short 440 Hz test tone through the internal speaker
ds4-sound-link --test-tone --speaker --volume 60 --duration 3

# List connected Bluetooth DS4 controllers
ds4-sound-link --list-controllers
```

See all options with:

```bash
ds4-sound-link --help
```

## Uninstallation

```bash
./install.sh --uninstall
```

This stops the user service and removes the installed binary, service, and udev rules. Packages, Bluetooth configuration, and kernel modules are left unchanged.

## Development

```bash
cargo build --release --locked
cargo test
cargo clippy --all-targets -- -D warnings
```

The `DS4-Sound-Link/` and `ProsperoLight/` directories are reference repositories and are not required by the installer.
