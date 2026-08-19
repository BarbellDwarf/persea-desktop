# Local UI audit environment: builds the app, provisions a test persea
# server, and runs the E2E suite exactly like the Linux leg of e2e.yml,
# with no host tooling beyond docker (persea-desktop#81).
#
# The repo is mounted at /workspace and the persea repo at /persea at
# runtime, so nothing is copied into the image and the audit always runs
# against the checked-out code.
FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive \
    RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH

# Same system deps as the Linux leg of e2e.yml, plus the build baseline
# (build-essential, pkg-config), curl/ca-certificates for rustup, and
# node for the WebDriver client.
RUN apt-get update && apt-get install -y \
      libwebkit2gtk-4.1-dev \
      libgtk-3-dev \
      libayatana-appindicator3-dev \
      librsvg2-dev \
      libxdo-dev \
      libssl-dev \
      libdbus-1-dev \
      xvfb \
      openssh-server \
      webkit2gtk-driver \
      curl \
      ca-certificates \
      build-essential \
      pkg-config \
      git \
      nodejs \
      npm \
    && rm -rf /var/lib/apt/lists/*

# Rust toolchain: stable with rustfmt and clippy, as the CI setup action
# installs. Not pinned; the suite needs whatever stable is current.
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y \
      --default-toolchain stable \
      --profile minimal \
    && rustup component add rustfmt clippy

# tauri-driver drives the app under WebDriver; tauri-cli builds the deb
# bundle for the deb-smoke spec.
RUN cargo install tauri-driver --locked \
    && cargo install tauri-cli --locked

# audit.sh runs the container with no command; the entrypoint lives in the
# mounted repo so the audit always runs the entrypoint of the checked-out
# code.
CMD ["/workspace/tests/e2e/audit-entrypoint.sh"]
