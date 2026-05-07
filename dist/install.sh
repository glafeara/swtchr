#!/usr/bin/env bash
# swtchr installer for Arch Linux + Hyprland.
# Builds release binary, drops into ~/.local/bin, installs the udev rule
# (requires sudo once), enables the systemd user service.

set -euo pipefail

PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${HOME}/.local/bin"
UNIT_DIR="${HOME}/.config/systemd/user"
CONF_DIR="${HOME}/.config/swtchr"
UDEV_RULE_SRC="${PROJECT_ROOT}/dist/udev/70-swtchr.rules"
UDEV_RULE_DST="/etc/udev/rules.d/70-swtchr.rules"
UNIT_SRC="${PROJECT_ROOT}/dist/systemd/swtchr.service"
UNIT_DST="${UNIT_DIR}/swtchr.service"
CONF_EXAMPLE="${PROJECT_ROOT}/dist/config.example.toml"
FETCH_DICTS="${PROJECT_ROOT}/dist/fetch-dicts.sh"

step() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m!!\033[0m  %s\n' "$*" >&2; }
fatal() {
    printf '\033[1;31mxx\033[0m  %s\n' "$*" >&2
    exit 1
}

# Sanity checks ---------------------------------------------------------------
[[ -f "${PROJECT_ROOT}/Cargo.toml" ]] || fatal "run install.sh from inside the swtchr repo"
command -v cargo >/dev/null || fatal "cargo not in PATH (pacman -S rust)"
command -v hyprctl >/dev/null || warn "hyprctl not found — service will fail at runtime outside a Hyprland session"

# English Hunspell dictionary (large variant — ~125k lemmas)
if [[ ! -f /usr/share/hunspell/en_US-large.dic ]]; then
    warn "missing /usr/share/hunspell/en_US-large.dic — auto-detection will be weak"
    warn "install with: sudo pacman -S hunspell-en_us"
fi

# Group membership
if ! id -nG "$USER" | grep -qw input; then
    warn "user '$USER' is not in the 'input' group"
    echo "    fix with: sudo usermod -aG input $USER  (then log out + back in)"
fi

# Build -----------------------------------------------------------------------
step "building release binary"
( cd "${PROJECT_ROOT}" && cargo build --release )

step "installing binary into ${BIN_DIR}"
mkdir -p "${BIN_DIR}"
install -m 0755 "${PROJECT_ROOT}/target/release/swtchr" "${BIN_DIR}/swtchr"

# udev rule (needs sudo) ------------------------------------------------------
if [[ ! -f "${UDEV_RULE_DST}" ]] || ! cmp -s "${UDEV_RULE_SRC}" "${UDEV_RULE_DST}"; then
    step "installing udev rule (sudo)"
    sudo install -m 0644 "${UDEV_RULE_SRC}" "${UDEV_RULE_DST}"
    sudo udevadm control --reload
    sudo udevadm trigger --subsystem-match=input
    sudo udevadm trigger --name-match=uinput || true
else
    step "udev rule already up to date"
fi

# Systemd user unit -----------------------------------------------------------
step "installing systemd user unit"
mkdir -p "${UNIT_DIR}"
install -m 0644 "${UNIT_SRC}" "${UNIT_DST}"
systemctl --user daemon-reload

# Russian frequency word list ------------------------------------------------
step "fetching Russian frequency word list"
"${FETCH_DICTS}"

# Default config --------------------------------------------------------------
if [[ ! -f "${CONF_DIR}/config.toml" ]]; then
    step "seeding default config at ${CONF_DIR}/config.toml"
    mkdir -p "${CONF_DIR}"
    sed "s|\$HOME|${HOME}|g" "${CONF_EXAMPLE}" >"${CONF_DIR}/config.toml"
    chmod 0644 "${CONF_DIR}/config.toml"
fi

# Enable + start --------------------------------------------------------------
step "enabling + starting swtchr.service"
systemctl --user enable --now swtchr.service

step "done."
echo
systemctl --user --no-pager --full status swtchr.service || true
echo
echo "  logs:    journalctl --user -u swtchr -f"
echo "  config:  ${CONF_DIR}/config.toml"
echo "  toggle:  press <Pause> to swap auto/manual mode"
echo "  manual:  double-tap <Right Shift> within 300ms to convert the current word"
