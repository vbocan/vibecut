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
#   Stage 3 (models):         fetch the Silero VAD model (speech-driven
#                             ducking) from a pinned upstream commit and
#                             verify it.
#   Stage 4 (runtime):        CUDA runtime + ffmpeg-with-nvenc + title-card
#                             fonts + the three Rust binaries + the built
#                             web UI + the model. No Python, no Node, no npm.
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
COPY src/vibecut-ui/package.json src/vibecut-ui/package-lock.json ./
RUN npm ci
COPY src/vibecut-ui/ ./
RUN npm run build
# adapter-static (fallback: 'index.html', see src/vibecut-ui/svelte.config.js)
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

WORKDIR /build
# Copy the whole workspace. The dep graph is small enough that splitting
# into a "deps-only" pre-build layer isn't worth the maintenance cost.
# Every workspace member needs a COPY line here regardless of whether
# its binary ships in the runtime stage below — on a virtual-manifest
# workspace root, `cargo build --release` builds every member, so a
# missing COPY fails the build the moment the crate exists. Destination
# paths mirror the host layout (src/<crate>) since the workspace
# Cargo.toml's member paths are relative to it.
COPY Cargo.toml Cargo.lock ./
COPY src/vibecut ./src/vibecut
COPY src/vibecut-ops ./src/vibecut-ops
COPY src/vibecut-mcp ./src/vibecut-mcp
COPY src/vibecut-web ./src/vibecut-web

RUN cargo build --release \
    && mkdir -p /out \
    && cp target/release/vibecut /out/vibecut \
    && cp target/release/vibecut-mcp /out/vibecut-mcp \
    && cp target/release/vibecut-web /out/vibecut-web \
    # ort 2.x with `download-binaries` ships ONNX Runtime as shared libs
    # alongside the binary. Copy them so the runtime stage can find them.
    && (find target/release -maxdepth 1 -name "*.so*" -exec cp -L {} /out/ \; || true)

# ---------- Stage 3: Silero VAD model ----------
#
# Speech-driven soundtrack ducking runs Silero VAD (MIT licence,
# https://github.com/snakers4/silero-vad). The model is fetched from the
# upstream repo pinned to the commit of release v6.2.3 (model v6.2) and
# checked against the size and git blob id GitHub's contents API reports
# for that commit, so a changed or truncated download fails the build
# instead of shipping a different model. Bumping it means updating all four
# values from `gh api "repos/snakers4/silero-vad/contents/src/silero_vad/data?ref=<sha>"`
# (and `.../contents/LICENSE?ref=<sha>`), and checking that the model's
# inputs still match src/vibecut/src/ml_engine.rs (`input`, `state`, `sr`).
# Its own stage so the runtime image doesn't need curl.
FROM ubuntu:${UBUNTU_VERSION} AS models
ARG SILERO_VAD_COMMIT=5cd7945676eb32225748052e2e6a0580e4686a08
ARG SILERO_VAD_SIZE=2327524
ARG SILERO_VAD_BLOB=80c5592ef1f4c9ede3e357bbd02eb863358a6a9d
ARG SILERO_VAD_LICENSE_BLOB=0bf5e90cac691b999d4a35044f97167d7bbbf0b9
SHELL ["/bin/bash", "-o", "pipefail", "-c"]
RUN apt-get update && apt-get install -y --no-install-recommends \
        curl \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /models
RUN base="https://raw.githubusercontent.com/snakers4/silero-vad/${SILERO_VAD_COMMIT}" \
    && curl -fsSL --retry 3 -o silero_vad.onnx "${base}/src/silero_vad/data/silero_vad.onnx" \
    && curl -fsSL --retry 3 -o LICENSE.silero-vad "${base}/LICENSE" \
    # git's blob id: sha1 of "blob <size>\0" + the file's bytes.
    && blob() { { printf 'blob %d\0' "$(stat -c %s "$1")"; cat "$1"; } | sha1sum | cut -d' ' -f1; } \
    && check() { \
        if [ "$2" != "$3" ]; then echo "$1: got $2, expected $3" >&2; exit 1; fi; \
    } \
    && check "silero_vad.onnx size" "$(stat -c %s silero_vad.onnx)" "${SILERO_VAD_SIZE}" \
    && check "silero_vad.onnx blob" "$(blob silero_vad.onnx)" "${SILERO_VAD_BLOB}" \
    && check "LICENSE blob" "$(blob LICENSE.silero-vad)" "${SILERO_VAD_LICENSE_BLOB}" \
    && printf '%s\n' \
        "silero_vad.onnx: Silero VAD v6.2 (voice activity detection), used by" \
        "vibecut for speech-driven soundtrack ducking." \
        "Source:  https://github.com/snakers4/silero-vad" \
        "File:    src/silero_vad/data/silero_vad.onnx at commit ${SILERO_VAD_COMMIT} (tag v6.2.3)" \
        "Size:    ${SILERO_VAD_SIZE} bytes, git blob ${SILERO_VAD_BLOB}" \
        "Licence: MIT, copyright Silero Team; full text in LICENSE.silero-vad" \
        > NOTICE.silero-vad

# ---------- Stage 4: runtime ----------
FROM nvidia/cuda:${CUDA_VERSION}-runtime-ubuntu${UBUNTU_VERSION} AS runtime
ENV DEBIAN_FRONTEND=noninteractive \
    VIBECUT_LOG=info \
    # Both servers' own crates AND vibecut-ops (SessionLogger and
    # every other op's tracing calls live there, not in either
    # transport crate) need to be named explicitly: an EnvFilter
    # directive list silently drops events from any target it
    # doesn't mention. `vibecut` is the engine itself, which renders
    # in-process: its info lines say which font each title card and
    # which Silero VAD model the ducking used.
    RUST_LOG=vibecut_mcp=info,vibecut_web=info,vibecut_ops=info,vibecut=info,rmcp=info \
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
    WEB_STATIC_DIR=/app/vibecut-ui \
    # Drop-in folders on the projects volume, searched before the fonts
    # and the model baked into the image (see DOCKER.md, "Fonts and
    # models"). Absent folders are skipped.
    VIBECUT_FONTS_DIR=/projects/fonts \
    VIBECUT_MODELS_DIR=/projects/models

# ffmpeg from Ubuntu archive (currently 6.x) ships with NVENC enabled.
# tini reaps zombies from the vibecut subprocesses the MCP server
# spawns, and supervises the two-process entrypoint script below
# the same way.
# Title-card fonts (drawtext): fontconfig for `fc-match` family lookup,
# DejaVu (the fallback default), Liberation (metric-compatible stand-ins
# for Arial / Times New Roman / Courier New, so Windows-authored configs
# naming arialbd.ttf keep their layout), Inter (the default card face) and
# Bebas Neue (a condensed display face). CONFIG.md lists the names.
RUN apt-get update && apt-get install -y --no-install-recommends \
        ffmpeg \
        ca-certificates \
        tini \
        fontconfig \
        fonts-dejavu-core \
        fonts-liberation \
        fonts-inter \
        fonts-bebas-neue \
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
# fallback (see src/vibecut-web/src/lib.rs's `router`).
COPY --from=web-builder /src/vibecut-ui/build /app/vibecut-ui

# Silero VAD + its MIT licence and provenance notice. The engine's model
# lookup ends here: [project].models_dir → $VIBECUT_MODELS_DIR → /app/models.
COPY --from=models /models/ /app/models/

# Mount points the MCP client must bind. We create them so an absent
# mount still produces a clear ENOENT instead of "is a directory" surprises.
RUN mkdir -p /footage /projects /projects/out /cache

COPY docker/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

EXPOSE 8080 8090

ENTRYPOINT ["tini", "--", "/usr/local/bin/entrypoint.sh"]
