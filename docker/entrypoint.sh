#!/bin/sh
# Runs both server processes in one container: vibecut-mcp
# (MCP Streamable HTTP, :8080) and vibecut-web (REST + WebSocket +
# the built web UI, :8090). Plain POSIX sh — no bash-only `wait -n` —
# so this doesn't depend on which shell provides /bin/sh on the base
# image.
#
# vibecut-web starts first, backgrounded; vibecut-mcp is the
# foreground process tini (PID 1, see the Dockerfile's ENTRYPOINT)
# supervises directly, so existing "docker logs" / restart-policy
# expectations built around vibecut-mcp being the main process
# don't change.
#
# Known simplification: if vibecut-web crashes, nothing here
# notices or brings vibecut-mcp down with it — the container keeps
# running with only the MCP side alive. tini still reaps the crashed
# process cleanly (no zombie), you just don't get an automatic
# container restart out of that specific failure. Correlating the two
# processes' lifetimes is a reasonable follow-up if that gap ever
# actually bites; not worth the added shell-script complexity until
# then.
set -e

/usr/local/bin/vibecut-web &

exec /usr/local/bin/vibecut-mcp
