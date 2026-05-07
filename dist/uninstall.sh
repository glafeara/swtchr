#!/usr/bin/env bash
# Reverse of install.sh: stop service, remove unit, binary, udev rule.
# Leaves ~/.config/swtchr/config.toml in place.

set -euo pipefail

UNIT_DST="${HOME}/.config/systemd/user/swtchr.service"
BIN_DST="${HOME}/.local/bin/swtchr"
UDEV_RULE_DST="/etc/udev/rules.d/70-swtchr.rules"

step() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }

if systemctl --user is-active --quiet swtchr.service 2>/dev/null; then
    step "stopping swtchr.service"
    systemctl --user disable --now swtchr.service || true
fi

if [[ -f "${UNIT_DST}" ]]; then
    step "removing systemd unit"
    rm -f "${UNIT_DST}"
    systemctl --user daemon-reload
fi

if [[ -f "${BIN_DST}" ]]; then
    step "removing binary"
    rm -f "${BIN_DST}"
fi

if [[ -f "${UDEV_RULE_DST}" ]]; then
    step "removing udev rule (sudo)"
    sudo rm -f "${UDEV_RULE_DST}"
    sudo udevadm control --reload
fi

step "done. config preserved at ~/.config/swtchr/"
