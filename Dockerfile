# syntax=docker/dockerfile:1.7
#
# All-in-one image for Vibecut + MCP server + Web UI (Rust workspace
# + SvelteKit).
#
#   Stage 1 (web-builder):    build the SvelteKit static site
#                             (adapter-static; Node only exists in
#                             this stage, never in the runtime image).
#   Stage 2 (rust-builder):   compile all three binaries (vibecut +
#                             vibecut-mcp + vibecut-web).
#   Stage 3 (runtime):        CUDA runtime + ffmpeg-with-nvenc + the
#                             three Rust binaries + the built web UI.
#                             No Python, no Node, no npm.
#
# GPU passthrough at run time uses NVIDIA Container Toolkit (`--gpus all`):
# WSL2 + the Windows NVIDIA driver on Docker Desktop, native on Linux.
# Without `--gpus all` the pipeline falls back to libx264 — slower, but
# functionally identical output.

ARG CUDA_VERSION=12.6.0
ARG UBUNTU_VERSION=24.04
ARG NODE_VERSION=22

# ---------- Stage 1: SvelteKit static build ----------
#
# Separate stage (not folded into rust-builder) so a Rust-only source
# change doesn't invalidate `npm ci`'s layer cache and vice versa —
# the two build graphs have nothing in common. `npm ci` (not `install`)
# for a reproducible build straight from the committed lockfile.
FROM node:${NODE_VERSION}-slim AS web-builder
WORKDIR /src/vibecut-ui
COPY vibecut-ui/package.json vibecut-ui/package-lock.json ./
RUN npm ci
COPY vibecut-ui/ ./
RUN npm run build
# adapter-static (fallback: 'index.html', see vibecut-ui/svelte.config.js)
# writes the whole static site to ./build.

# ---------- Stage 2: Rust workspace build ----------
#
# We build on Ubuntu 24.04 to MATCH the runtime stage's glibc (2.39). The
# ort crate's `download-binaries` feature fetches ONNX Runtime prebuilts
# compiled against glibc 2.38+, which reference C23 symbols like
# `__isoc23_strtol`. Building on rust:slim-bookworm (Debian 12, glibc 2.36)
# fails the link with `undefined symbol: __isoc23_strtol` because those
# symbols don't exist there. Same-base builds eliminate the skew entirely.
FROM ubuntu:${UBUNTU_VERSION} AS rust-builder
ENV DEBIAN_FRONTEND=noninteractive \
    PATH="/root/.cargo/bin:${PATH}"
RUN apt-get update && apt-get install -y --no-install-recommends \
        curl \
        ca-certificates \
        pkg-config \
        libssl-dev \
        g++ \
    && rm -rf /var/lib/apt/lists/*

# Install rustup + stable toolchain.
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --default-toolchain stable --profile minimal

WORKDIR /src
# Copy the whole workspace. The dep graph is small enough that splitting
# into a "deps-only" pre-build layer isn't worth the maintenance cost.
# Every workspace member needs a COPY line here regardless of whether
# its binary ships in the runtime stage below — on a virtual-manifest
# workspace root, `cargo build --release` builds every member, so a
# missing COPY fails the build the moment the crate exists.
COPY Cargo.toml Cargo.lock ./
COPY vibecut ./vibecut
COPY vibecut-ops ./vibecut-ops
COPY vibecut-mcp ./vibecut-mcp
COPY vibecut-web ./vibecut-web

RUN cargo build --release \
    && mkdir -p /out \
    && cp target/release/vibecut /out/vibecut \
    && cp target/release/vibecut-mcp /out/vibecut-mcp \
    && cp target/release/vibecut-web /out/vibecut-web \
    # ort 2.x with `download-binaries` ships ONNX Runtime as shared libs
    # alongside the binary. Copy them so the runtime stage can find them.
    && (find target/release -maxdepth 1 -name "*.so*" -exec cp -L {} /out/ \; || true)

# ---------- Stage 3: runtime ----------
FROM nvidia/cuda:${CUDA_VERSION}-runtime-ubuntu${UBUNTU_VERSION} AS runtime
ENV DEBIAN_FRONTEND=noninteractive \
    VIBECUT_LOG=info \
    # Both servers' own crates AND vibecut-ops (SessionLogger and
    # every other op's tracing calls live there, not in either
    # transport crate) need to be named explicitly: an EnvFilter
    # directive list silently drops events from any target it
    # doesn't mention.
    RUST_LOG=vibecut_mcp=info,vibecut_web=info,vibecut_ops=info,rmcp=info \
    # NVIDIA Container Toolkit injects driver libraries at run time based
    # on this list. The CUDA base image defaults to "compute,utility" —
    # which is enough for inference but EXCLUDES libnvidia-encode (NVENC).
    # Adding "video" gives ffmpeg's h264_nvenc / hevc_nvenc encoders the
    # encoder library when --gpus all is passed.
    NVIDIA_DRIVER_CAPABILITIES=compute,utility,video \
    # Default bind addresses (override via env at run time).
    MCP_BIND_ADDR=0.0.0.0:8080 \
    WEB_BIND_ADDR=0.0.0.0:8090 \
    # Where vibecut-web finds the built SvelteKit site. Unset in
    # local (non-Docker) dev, where `vite dev` serves the UI
    # instead — see DOCKER.md's dev loop section.
    WEB_STATIC_DIR=/app/vibecut-ui

# ffmpeg from Ubuntu archive (currently 6.x) ships with NVENC enabled.
# tini reaps zombies from the vibecut subprocesses the MCP server
# spawns, and supervises the two-process entrypoint script below
# the same way.
RUN apt-get update && apt-get install -y --no-install-recommends \
        ffmpeg \
        ca-certificates \
        tini \
    && rm -rf /var/lib/apt/lists/*

# All three Rust binaries.
COPY --from=rust-builder /out/vibecut /usr/local/bin/vibecut
COPY --from=rust-builder /out/vibecut-mcp /usr/local/bin/vibecut-mcp
COPY --from=rust-builder /out/vibecut-web /usr/local/bin/vibecut-web
# ONNX Runtime shared libs (used by the vibecut binary).
COPY --from=rust-builder /out/ /tmp/ortlibs/
RUN find /tmp/ortlibs -name "*.so*" -exec cp -L {} /usr/local/lib/ \; \
    && rm -rf /tmp/ortlibs \
    && ldconfig

# The built web UI — vibecut-web serves this directly as a static
# fallback (see vibecut-web/src/lib.rs's `router`).
COPY --from=web-builder /src/vibecut-ui/build /app/vibecut-ui

# Mount points the MCP client must bind. We create them so an absent
# mount still produces a clear ENOENT instead of "is a directory" surprises.
RUN mkdir -p /footage /projects /projects/out /cache

COPY docker/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

EXPOSE 8080 8090

ENTRYPOINT ["tini", "--", "/usr/local/bin/entrypoint.sh"]
