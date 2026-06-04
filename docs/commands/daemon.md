# Daemon Commands

Daemon commands manage the persistent cache daemon for faster repeated queries.

## daemon

**Purpose:** Daemon management commands.

**Implementation:** `crates/tldr-cli/src/commands/daemon/`

**Subcommands:**

### daemon start

Start the TLDR daemon for caching.

```bash
tldr daemon start

# With custom project
tldr daemon start --project /path/to/project

# TCP mode (Windows)
tldr daemon start --tcp --port 7890
```

**Lifetime (presence-based liveness, epic TLDR-cxa):** the daemon
self-terminates after 30 minutes with **no project presence** — no client
connection, no `tldr`/`tldr_mcp` invocation in the project (every invocation
sends a liveness poke; opt out with `TLDR_NO_POKE=1`), and no project file
writes — and **never** while an index build or re-index delta is in flight.
`tldr daemon status` shows what is keeping it alive and the idle deadline.

**How it works:**
1. Creates Unix socket at `~/.cache/tldr/<project_hash>.sock`
2. Starts HTTP server on socket
3. Background process caches analysis results

### daemon stop

Stop the running daemon.

```bash
tldr daemon stop
```

### daemon status

Check if daemon is running.

```bash
tldr daemon status
```

### daemon query

Send raw query to daemon.

```bash
tldr daemon query '{"cmd":"stats"}'
```

### daemon notify

Notify daemon of file changes (invalidates cache).

```bash
tldr daemon notify src/main.py
tldr daemon notify src/
```

---

## cache

**Purpose:** Cache management commands.

### cache stats

Show cache statistics.

```bash
tldr cache stats
```

### cache clear

Clear all cache files.

```bash
tldr cache clear
```

---

## warm

**Alias:** `w`

**Purpose:** Pre-warm call graph cache for faster subsequent queries.

**Implementation:** `crates/tldr-cli/src/commands/daemon/warm.rs`

```bash
tldr warm src/

# Background warming
tldr warm src/ -b
```

**How it works:**
1. Builds call graph in background
2. Caches results in daemon memory
3. Subsequent queries hit cache (~35x faster)

---

## stats

**Purpose:** Show TLDR usage statistics.

```bash
tldr stats
```

Shows:
- Total queries run
- Cache hit rate
- Average query time
- Most used commands
