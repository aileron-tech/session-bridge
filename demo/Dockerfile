# Demo image: one simulated laptop running a real session-bridge daemon
# (Linux build) with scripted stand-in agents. Build context: the repository
# root (see compose.yml); demo/Dockerfile.dockerignore keeps it small.

FROM rust:1.95-slim-trixie AS build
# rust-toolchain.toml asks for rustfmt and clippy; the release build needs
# neither, so pin the image's own toolchain instead of downloading them.
ENV RUSTUP_TOOLCHAIN=1.95.0
WORKDIR /src
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY .cargo .cargo
COPY src src
RUN cargo build --locked --release \
    && cp target/release/session-bridge /session-bridge \
    && rm -rf target

FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends sqlite3 lsof cron python3 procps \
       openssh-client openssh-server \
    && mkdir -p /run/sshd \
    && printf 'PasswordAuthentication no\nKbdInteractiveAuthentication no\nPermitRootLogin no\n' > /etc/ssh/sshd_config.d/demo.conf \
    && rm -rf /var/lib/apt/lists/* \
    # cron's PAM login uid step fails in containers.
    && sed -i '/pam_loginuid.so/s/^/#/' /etc/pam.d/cron \
    && useradd --create-home --uid 1000 --shell /bin/bash dev
COPY --from=build /session-bridge /usr/local/bin/session-bridge
COPY demo/bin /demo/bin
COPY demo/agents /demo/agents
RUN chmod 755 /demo/bin/* /demo/agents/*.py
ENTRYPOINT ["/demo/bin/entrypoint.sh"]
