# Demo desktop in a container

A throwaway Linux desktop (labwc on Wayland, a terminal, a task bar with a tray and a clock) that
shares itself with PyroMirror, for trying a client without a second computer. PyroMirror runs in
it the way it does after logging in to a real desktop: as a tray icon that starts sharing by
itself, with the launcher window one click away.

```bash
docker build -t pyromirror-demo docker/demo
```

```bash
docker run --rm --network host pyromirror-demo
```

Then connect a client to this machine's address (port 9000). Under WSL 2, that is the address
`ip -4 addr show eth0` prints inside WSL, used from the Windows side.

Without `--network host`, publish both protocols: `-p 9000:9000/tcp -p 9000:9000/udp`.

## What to expect

- **Mouse and keyboard work**, through the compositor's virtual pointer and keyboard (needs
  PyroMirror 0.1.0-beta.7 or later). The keyboard layout is `XKB_DEFAULT_LAYOUT`, `us` by default.
- **Tray icon and notifications.** The icon in the task bar shows whether sharing is on and
  whether someone is connected; its menu opens the launcher and stops or starts sharing, and
  "Quit" ends the container.
  While a client is connected, the menu also has "*name* session" with that client's fullscreen,
  mouse and sound options. A connecting client raises a notification in the top right corner.
- **Software rendering.** No GPU is used; Mesa's CPU Vulkan driver runs the encoder. It is a test
  target, not a benchmark. PyroWave needs 16-wide Vulkan subgroups, which that driver only offers
  with 512-bit vectors, so this is only known to work on CPUs with AVX-512.
- **No pairing** by default, since there is nobody in the container to read out a code. Do not
  expose it beyond a network you trust.

## Settings

| Variable | Default | Meaning |
| :--- | :--- | :--- |
| `DEMO_RESOLUTION` | `1920x1080` | Size of the virtual screen |
| `PYROMIRROR_ARGS` | empty | Extra `pyromirror-server` options, for example `--bitrate-mbps 100` |
| `PYROMIRROR_PAIRING` | `0` | `1` requires pairing; the code appears in the launcher window and in `docker logs` |

Build another version with `--build-arg PYROMIRROR_VERSION=0.1.0-beta.7`.
