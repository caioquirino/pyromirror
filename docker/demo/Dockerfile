# A throwaway Linux desktop that shares itself with PyroMirror, for trying a client against
# without a second computer. Everything is rendered in software (no GPU needed), so expect a
# modest frame rate; it is a test target, not a benchmark.
#
#   docker build -t pyromirror-demo docker/demo
#   docker run --rm --network host pyromirror-demo
#
# then connect a PyroMirror client to this machine's address.
FROM archlinux:latest

RUN pacman -Syu --noconfirm --needed \
        labwc foot waybar libdbusmenu-gtk3 mako swaybg wlr-randr ttf-dejavu \
        xdg-desktop-portal xdg-desktop-portal-wlr \
        pipewire wireplumber pipewire-pulse \
        vulkan-swrast vulkan-icd-loader mesa libglvnd libxkbcommon \
        dbus \
    && pacman -Scc --noconfirm

ARG PYROMIRROR_VERSION=0.1.0-beta.7
ADD https://github.com/caioquirino/pyromirror/releases/download/v${PYROMIRROR_VERSION}/pyromirror-${PYROMIRROR_VERSION}-linux-x86_64.tar.gz /tmp/pyromirror.tar.gz
RUN mkdir -p /opt/pyromirror \
    && tar -xzf /tmp/pyromirror.tar.gz -C /opt/pyromirror --strip-components=1 \
    && rm /tmp/pyromirror.tar.gz \
    # The launcher starts the server that sits next to it. Put a wrapper there, so that only the
    # codec gets the Mesa setting it needs (see the wrapper) and the desktop's own programs do not.
    && mkdir /opt/pyromirror/libexec \
    && mv /opt/pyromirror/pyromirror-server /opt/pyromirror/pyromirror-client /opt/pyromirror/libpyrowave-shared.so.0 /opt/pyromirror/libexec/
COPY codec-wrapper.sh /opt/pyromirror/pyromirror-server
COPY codec-wrapper.sh /opt/pyromirror/pyromirror-client
RUN chmod +x /opt/pyromirror/pyromirror-server /opt/pyromirror/pyromirror-client
ENV PATH=/opt/pyromirror:$PATH

RUN useradd -m demo
COPY --chown=demo:demo home/ /home/demo/
COPY session.sh /usr/local/bin/pyromirror-demo-session
RUN chmod +x /usr/local/bin/pyromirror-demo-session

USER demo
WORKDIR /home/demo
# Size of the virtual screen, and anything extra for pyromirror-server (for example
# "--bitrate-mbps 100"). Pairing is off unless PYROMIRROR_PAIRING=1.
ENV DEMO_RESOLUTION=1920x1080 \
    PYROMIRROR_ARGS="" \
    PYROMIRROR_PAIRING=0
EXPOSE 9000/tcp 9000/udp
ENTRYPOINT ["/usr/local/bin/pyromirror-demo-session"]
