#!/bin/sh
# Starts a headless Wayland desktop and shares it with pyromirror-server.
set -eu

export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/tmp/runtime-$(id -u)}"
mkdir -p "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"

if [ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ]; then
    exec dbus-run-session -- "$0" "$@"
fi

# No GPU and no input devices: draw with the CPU into a virtual screen.
export WLR_BACKENDS=headless
export WLR_RENDERER=pixman
export WLR_LIBINPUT_NO_DEVICES=1
export XDG_CURRENT_DESKTOP=wlroots
export XDG_SESSION_TYPE=wayland
export WAYLAND_DISPLAY=wayland-0
export LIBGL_ALWAYS_SOFTWARE=1

log="$XDG_RUNTIME_DIR/logs"
mkdir -p "$log"

pipewire >"$log/pipewire.log" 2>&1 &
wireplumber >"$log/wireplumber.log" 2>&1 &
pipewire-pulse >"$log/pipewire-pulse.log" 2>&1 &

labwc >"$log/labwc.log" 2>&1 &
i=0
until [ -S "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" ]; do
    i=$((i + 1))
    [ "$i" -le 100 ] || { echo "The desktop did not start:" >&2; cat "$log/labwc.log" >&2; exit 1; }
    sleep 0.1
done
wlr-randr --output HEADLESS-1 --custom-mode "${DEMO_RESOLUTION}" >/dev/null 2>&1 || true

dbus-update-activation-environment WAYLAND_DISPLAY XDG_CURRENT_DESKTOP XDG_RUNTIME_DIR XDG_SESSION_TYPE
/usr/lib/xdg-desktop-portal-wlr >"$log/portal-wlr.log" 2>&1 &
/usr/lib/xdg-desktop-portal >"$log/portal.log" 2>&1 &
sleep 1

# PyroMirror runs the way it does after logging in to a real desktop: the tray icon in the
# background, which starts sharing by itself. Settings as the launcher would have saved them.
config="${XDG_CONFIG_HOME:-$HOME/.config}/pyromirror"
mkdir -p "$config"
pairing=false
[ "${PYROMIRROR_PAIRING}" = "1" ] && pairing=true
cat >"$config/settings.json" <<JSON
{ "tab": "Host", "auto_share": true, "require_pairing": $pairing }
JSON

echo "Desktop is up (${DEMO_RESOLUTION}). PyroMirror is sharing it; connect a client to this machine on port 9000."
# The server's log is what is worth watching from outside.
touch "$config/server.log"
tail -F "$config/server.log" 2>/dev/null &
exec pyromirror --background
