.PHONY: build test lint fmt clean release install install-restart restart-daemons clean-stale-bins which-tldr

# Binary install locations. Both are commonly on PATH; keeping them in sync
# prevents a stale shadow copy from lingering (a real footgun: a long-lived
# launchd daemon or `which tldr` can otherwise run yesterday's binary).
LOCAL_BIN ?= $(HOME)/.local/bin
CARGO_BIN ?= $(HOME)/.cargo/bin

build:
	cargo build --release --features semantic

test:
	cargo test -p tldr-core --lib
	cargo test -p tldr-cli --lib

lint:
	cargo clippy --workspace -- -D warnings

fmt:
	cargo fmt --check

clean:
	cargo clean

# Install the release binary to BOTH common PATH locations so no stale copy
# can shadow the fresh one. NOTE: this updates the on-disk binary only — a
# already-running daemon keeps the OLD code in memory until restarted
# (see `install-restart`).
install: build
	@mkdir -p $(LOCAL_BIN)
	@# rm before cp, never overwrite in place: on macOS, if a running process still
	@# has this path mapped as its executable, an in-place overwrite corrupts the
	@# kernel's code-signature validation cache for that vnode, and every subsequent
	@# exec of the path gets SIGKILLed (exit 137) until the file gets a fresh inode.
	rm -f $(LOCAL_BIN)/tldr && cp target/release/tldr $(LOCAL_BIN)/tldr
	@# Keep the cargo-bin copy in sync too, if present, so it can't go stale.
	@if [ -e "$(CARGO_BIN)/tldr" ]; then rm -f "$(CARGO_BIN)/tldr" && cp target/release/tldr "$(CARGO_BIN)/tldr"; echo "synced $(CARGO_BIN)/tldr"; fi
	@echo "installed: $$($(LOCAL_BIN)/tldr --version) -> $(LOCAL_BIN)/tldr"

# Stop daemons BEFORE installing (not after): a live daemon keeps the binary's
# vnode mapped, so installing first and restarting after is exactly the
# ordering that triggers the corruption described in `install` above. Stopping
# first also means the real IPC shutdown (`tldr daemon stop --all`) is used
# instead of `pkill -f 'tldr daemon start'`, which only sends SIGTERM — these
# daemons only shut down via their own IPC protocol and ignore SIGTERM, so the
# old pkill-based target silently left the old binary running.
install-restart: restart-daemons install

restart-daemons:
	@echo "stopping running tldr daemons via IPC (SIGTERM alone does not stop them)..."
	@tldr daemon stop --all 2>/dev/null || true
	@sleep 1
	@echo "done — daemons respawn via launchd KeepAlive or lazily on next tldr invocation."

# Show every tldr on PATH (and flag drift) — quick staleness check.
which-tldr:
	@which -a tldr || true
	@for p in $(LOCAL_BIN)/tldr $(CARGO_BIN)/tldr; do \
		[ -e "$$p" ] && printf "%s -> %s\n" "$$p" "$$($$p --version 2>/dev/null)"; \
	done

# Remove a stale cargo-bin copy entirely (use if you want a single source of
# truth at $(LOCAL_BIN) and $(LOCAL_BIN) is first on PATH).
clean-stale-bins:
	@if [ -e "$(CARGO_BIN)/tldr" ]; then rm -f "$(CARGO_BIN)/tldr" && echo "removed $(CARGO_BIN)/tldr"; else echo "no $(CARGO_BIN)/tldr"; fi

# Run all checks (CI equivalent)
check: fmt lint test

# Quick dev build
dev:
	cargo build
