# hs — Local, context-aware command memory for developers & DevOps

[![Crates.io Version](https://img.shields.io/crates/v/hs-cli)](https://crates.io/crates/hs-cli)
[![Crates.io Downloads](https://img.shields.io/crates/d/hs-cli)](https://crates.io/crates/hs-cli)
[![License: MIT](https://img.shields.io/crates/l/hs-cli)](https://crates.io/crates/hs-cli)

> The installed command is **`hs`**. It ships on crates.io as **`hs-cli`** (the bare `hs` name is occupied by an unrelated placeholder), GitHub Releases ([v1.0.0](https://github.com/unknownman/hs/releases/tag/v1.0.0)), and as a source build.

![hs demo](demo.gif)

---

## Why hs?

Standard shell history can only answer one question: *"What did I type?"* — and even that is a wall of strings with no memory of whether the command worked, where you were, or how often it saved your day.

> **hs answers a smarter question: *"What worked here before?"***

| | `history` / `Ctrl+R` | **hs** |
|---|---|---|
| Knows the exit code? | ❌ | ✅ Success rate per command |
| Knows the project (Git repo)? | ❌ | ✅ `--project` scopes to the repo you're in |
| Knows *when*? | ✅ raw timestamps | ✅ recency-weighted ranking (`--last`, `2d`, `1w`…) |
| Full-text search? | basic substring | ✅ SQLite FTS5 with relevance ranking |
| Re-runs safely? | blind paste | ✅ risk classifier + confirmation guard |
| Keeps secrets? | everything, forever | ✅ redacted **before** it touches disk |
| Private by default? | locally | ✅ 100% local, zero telemetry |

### A context-aware memory, not a log

`history` remembers **strings**. `hs` remembers **outcomes**:

- Every captured command carries its shell **exit code**, its **duration**, the **working directory**, and a **project boundary** — so rank is not "most recent", it's "most likely to work *here*".
- Consecutive tabs write to the same database without missed captures.
- Pinned commands bypass the ranking lottery entirely and float to the top.

### Where it fits alongside Atuin & McFly

We have deep respect for the tools that made shell history a serious engineering topic. Atuin brings encrypted **cloud sync** and a social index; McFly experiments with **neural ranking**. Both are excellent — and both solve problems hs deliberately does not:

- **hs is offline-first.** Your history is a single SQLite file on your machine. No account, no sync, no telemetry — never.
- **hs is project-boundary-aware.** The unit of recall is "this repo", not "your whole machine".
- **hs is success-aware.** Exit codes drive the ranking — a command that broke twice and worked once ranks lower than the one that's never failed.

Choose Atuin or McFly for cloud sync or ML-search. Choose hs when you want a fast, private, *honest* memory of what actually worked where.

---

## Feature matrix

| | |
|---|---|
| **100% Local** | Everything lives in a SQLite database on your machine. No cloud, no telemetry, no account. |
| **SQLite + WAL** | Write-ahead logging for fast, crash-safe reads and writes across multiple terminals. |
| **Zero latency** | Capture runs in a backgrounded subprocess from `preexec`/`precmd` hooks — the prompt never blocks. |
| **Secret-redacted by default** | AWS keys, GitHub tokens, JWTs, Bearer tokens, and `KEY=value` secrets are masked before they ever touch disk. |
| **Shells** | v1.0 ships engineered hooks for **bash** and **zsh**. |
| **Search** | SQLite **FTS5** full-text search with `bm25` relevance ranking. |
| **Ranking** | Recency + frequency + success rate + project locality + pins. |

---

## Installation

```sh
cargo install hs-cli
```

Or build from source:

```sh
cargo install --path .
```

Requires Rust 2024 edition (rustc 1.85+). The resulting binary is self-contained; at runtime it needs no network access.

### Enable the shell hook

**zsh** (`~/.zshrc`):

```zsh
eval "$(hs init zsh)"
```

**bash** (`~/.bashrc`):

```bash
eval "$(hs init bash)"
```

The hook runs in the background for every interactive command you type — capturing the command text, the current directory, its exit code, and its duration in milliseconds. It never captures itself, and it silences all of its own output (your prompt and `$?` stay exactly as they were).

### Import your legacy history

Already carrying years of shell history? Bring it in:

```sh
hs import
```

`hs import` finds `~/.zsh_history` first (zsh extended format `: <timestamp>:<elapsed>;<command>`, including multi-line continuations and the per-command elapsed duration), then `~/.bash_history` (including `#<epoch>` `HISTTIMEFORMAT` markers). Every line is:

1. **Sanitized** through the same secret-redaction engine used for live capture,
2. **Deduplicated** against commands already in the database (idempotent), and
3. **Stored** with its original timestamp and duration.

```console
$ hs import
[hs] Imported 3 commands (0 duplicates skipped, 1 secrets redacted)
```

Rerunning is always safe:

```console
$ hs import
[hs] Imported 0 commands (3 duplicates skipped, 1 secrets redacted)
```

Point it at a specific file anytime: `hs import ~/.local/share/history`.

---

## Usage

### Interactive recall

Run `hs` — **with no arguments** for a full browse, or with a **query** (e.g. `hs deploy`) to open the same browser already narrowed to matching commands. Results are grouped into two sections — **`Current project`** and **`Global / other`** — with a preview panel showing the full command, its run statistics, and a live risk classification badge.

```text
┌ hs — What worked here before? ─────────────┐
│ ── Current project ─────────────────────── │
│ ▶ migrate --database=prod                 │
│ ── Global / other ──────────────────────── │
│   make migrate                            │
├─────────────────────────────── Preview ──  ┤
│  ID: 1  ·  Score 1.17  ·  1 run(s)  ·  100%│
│  migrate --database=prod                  │
│  [SAFE]                                    │
└───────────────────────────────────────────┘
↑/↓ navigate   Enter run   Esc / Ctrl-C exit
```

| Key | Action |
|---|---|
| `↑` / `↓` | move through results |
| `PageUp` / `PageDown` | jump ±10 rows |
| `Home` / `End` | jump to first / last |
| `Enter` | run the selected command (through the [execution guard](#execution-guard)) |
| `Esc` / `Ctrl-C` | exit without running |

### Targeted CLI queries

A query is a **pre-filter for the TUI**, not a read-only shortcut: `hs deploy` opens the interactive browser already narrowed to deploy commands — hit `Enter` to re-run one (safely), `Esc` to back out. Hyphen-prefixed words are part of the query (so `hs "rm -rf"` finds the literal command); flags typed after the query are restored correctly.

| Scenario | Command |
|---|---|
| **Dev** — working migrations for this repo | `hs --project --ok migrate` |
| **DevOps** — failed container runs, last 24 h | `hs --failed --last 1d docker` |
| **Scripting** — formatted table for pipes/logs | `hs --print build` |
| **Keepers** — pin a command you re-run constantly | `hs pin <ID>` |
| **Escape hatch** — permanently erase a leaked/typo command | `hs delete <ID>` |

### Read-only tables with `--print`

`--print` prints the ranked table and exits — perfect for piping into `less`, saving to a file, or feeding a script. (Any time stdout is not a terminal, the table is printed automatically.)

```text
╭──────┬────┬─────────────────────────────┬───────┬──────┬─────────┬──────────────────╮
│ Rank ┆ ID ┆ Command                     ┆ Score ┆ Runs ┆ Success ┆ Last Run         │
╞══════╪════╪═════════════════════════════╪═══════╪══════╪═════════╪══════════════════╡
│ →1   ┆ 42 ┆ echo hello-from-hs-project  ┆  9.20 ┆    1 ┆    100% ┆ 2026-09-11 14:55 │
├╌╌╌╌╌╌┼╌╌╌╌┼╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌┼╌╌╌╌╌╌╌┼╌╌╌╌╌╌┼╌╌╌╌╌╌╌╌╌┼╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌┤
│  2   ┆ 43 ┆ echo global-universal-hello ┆  5.07 ┆    1 ┆    100% ┆ 2026-09-11 14:55 │
╰──────┴────┴─────────────────────────────┴───────┴──────┴─────────┴──────────────────╯
→ commands from the current project
```

### Filters

| Flag | Meaning |
|---|---|
| `--project` | Restrict to the current git project (nearest `.git` ancestor). Mutually exclusive with `--global`. |
| `--global` | Search every project. Mutually exclusive with `--project`. |
| `--ok` | Only successful commands (exit code 0). Mutually exclusive with `--failed`. |
| `--failed` | Only failed commands (exit code > 0). For debugging "why did this break". |
| `--last <WINDOW>` | Time window: `30m`, `1h`, `2d`, `1w`. |
| `--print` | Print the read-only ranked table and exit instead of opening the TUI. |

### Pinned commands

Pinning keeps a critical command on a dedicated list, independent of ranking — it also grants a **score boost** in search results, so pinned commands surface to the top even when their run history is sparse:

```sh
hs pin <ID>      # the ID column of the row you care about
hs pins          # table of pinned commands (ID, Command, Project, Pinned At)
hs unpin <ID>    # forget it again
```

`pin` and `unpin` are idempotent, pinning a nonexistent ID fails cleanly, and deleting a command also deletes its pin. An empty list prints `No pinned commands.`

### Deleting — your privacy escape hatch

If a secret ever evades redaction, or a useless typo pollutes your history, remove it for good:

```sh
hs delete <ID>   # alias: hs rm <ID>
```

This permanently deletes the command, its executions, its stats, and its pins — and drops it from every `hs` search. It cannot be undone, so the ID comes from `hs` search results. Double-check before you run it.

### When does hs run a command?

**Never automatically.** A query merely *pre-filters* the interactive TUI; the only way a recorded command actually runs is an explicit `Enter` there, routed through the [execution guard](#execution-guard). A search can end in a re-run — but only because *you* pressed Enter and confirmed it.

### Exit codes

`hs` exits `0` on a successful search/browse (or `--print`), propagates the exit code of any command it runs from the TUI, returns `130` when a guarded execution is cancelled by the user, and `1` on any other error. Dismissing the TUI with `Esc`/`Ctrl-C` exits `0` (nothing was run, which is not an error).

---

## Under the hood

Engineers deserve to know *why* this is fast and safe.

### Zero-latency capture

The shell hooks are the entire trick. On execution the hook records the command text and a millisecond timestamp; when the prompt returns it forks a `hs capture ...` subprocess **fully detached and silenced** — `( hs capture … & )` in bash, `… &!` (disown) in zsh — and hands back your `$?` untouched. Your prompt is never blocked; `vim`, long builds, and prompt themes all keep their exact behavior.

### Concurrency without `SQLITE_LOCKED`

The capture from several terminal tabs can arrive at once, so the storage layer is built for it:

- **SQLite in WAL mode** — concurrent readers with a single serialized writer; readers never block the writer and vice-versa.
- **r2d2 connection pooling** (max 4), each connection with `PRAGMA busy_timeout = 5000` and `synchronous = NORMAL` — transient write locks are *waited* on instead of erroring out.
- **Dedup is `INSERT OR IGNORE`** keyed on the command hash — a command appearing twice in one instant (e.g. your own `hs` calls are skipped, but a race between tabs) can't double-write.

### Speed

- Written in **Rust** — zero-cost abstractions, no GC pauses.
- **FTS5** full-text virtual table keeps a synchronous index so searches are instant and `bm25` provides the relevance base for ranking.
- **xxhash-rust (`xxh3`)** hashes every command string for O(1) deduplication and project-directory identity — faster than cryptographic hashing, without needing collision resistance for a local history.

### Deterministic privacy

Secret detection is **regex-based and predictable** — no heuristics, no ML, no network calls. The pattern set is compiled **exactly once** into a `OnceLock`, and the sanitizer runs **in-memory before any disk I/O**: the command is scrubbed before it is hashed or persisted, so a secret never exists in the database, the WAL, or the FTS index.

---

## Use cases

### 🧭 The Context Switcher

You maintain five microservices, and `docker compose up` for each is subtly different. Plain history mixes them all together.

```sh
hs --project docker compose
```

Result: the exact `docker compose` command that worked *in this repo* — not one from the four other services.

### 🔥 The DevOps Debugger

You fixed a production server last month and don't remember how. `history` just shows the failing blast radius.

```sh
hs --failed --last 1w
```

`hs` surfaces the commands that broke — so you can retrace exactly what you ran after the failure that actually fixed it. No more scrolling past unrelated triage.

### 🛡️ The Security-Conscious

You just pasted a production AWS key into your terminal. With `history`, it now lives in plaintext forever.

With `hs`, what reaches the database is:

```text
export AWS_ACCESS_KEY_ID=[REDACTED]
```

The redaction happens with deterministic regexes, *before* anything is written — recorded, hashed, persisted, and searchable only as `[REDACTED]`.

---

## Safety & privacy

### Secret redaction

Every command passes through a conservative redaction engine **before** it enters the database. What it masks:

| Family | Shape |
|---|---|
| AWS access key IDs | `AKIA` + 16 uppercase alphanumerics |
| Stripe-style keys | `sk_` / `pk_` + `live`/`test` + 16+ chars |
| GitHub personal access tokens | `ghp_` + 30+ alphanumerics |
| JWTs | `eyJ…`.`eu…`.`…` (three base64url segments) |
| Bearer credentials | `Bearer <token>` → `Bearer [REDACTED]` |
| Inline secret assignments | `password=` `secret:` `token=` `api_key=` `private_key=` `access_key=` (+ quoted values) |

```sh
# what you type
curl -H "Authorization: Bearer super-secret-api-token-12345" https://api.example.com

# what is stored
curl -H "Authorization: Bearer [REDACTED]" https://api.example.com
```

Matching is deliberately conservative: a short, word-bounded fragment that only *looks like* a key prefix (not a real 24+ character token) is **not** redacted, so commit messages and prose survive intact. The same engine sanitizes everything `hs import` ingests.

### Execution guard

Before hs ever runs a stored command, it classifies it:

- **SAFE** — executed directly.
- **HIGH RISK** — `rm -rf`, `git push --force`, `dd`, and other destructive patterns are intercepted with an explicit confirmation prompt that defaults to **no**:

```text
[!] CAUTION: This command is flagged as High Risk
    rm -rf /tmp/hs-demo
? Are you sure you want to execute this? (y/n) › n
[hs] Aborted — nothing was executed.
```

If the prompt cannot be shown (no TTY), the command is declined. Guarding fails closed by design — it is astonishingly rare to re-run a destructive command from memory, and safe to demand a yes. (A literal `[REDACTED]` in a command also triggers the prompt, since it can't work as written.)

---

## Diagnostics

`hs doctor` inspects the database and environment and reports green or red per check:

```text
$ hs doctor

[✓] Database exists at ~/Library/Application Support/hs/hs.db
[✓] Parent directory writable
[✓] Journal mode: wal
[✓] Integrity check: ok
[✓] 5 unique commands · 5 executions · 56 KB on disk
[✗] not found: ~/.bashrc, ~/.bash_profile (soft — skipped)
[✗] not found: ~/.zshrc, ~/.zprofile (soft — skipped)

hs doctor: everything looks good.
```

Database health and volume checks are **hard** failures (`hs doctor` exits `1` if the database is missing, unwritable, not in WAL mode, or fails integrity). Hook-presence checks are **soft** — they inform, they don't fail the run.

The database lives at `~/Library/Application Support/hs/hs.db` on macOS (`$XDG_DATA_HOME/hs/hs.db` on Linux), in WAL mode.

---

## Honest limitations

- **Shells.** v1.0 officially supports **bash** and **zsh**. Fish, Nushell, and PowerShell are not supported yet (no hook implementations ship for them).
- **Subshell execution.** Re-running a command spawns `$SHELL -c <command>` (falling back to `/bin/sh` if `$SHELL` is unset). Commands that mutate your interactive shell — `cd`, `export`, virtualenv activation — do **not** affect the shell you ran `hs` from.
- **Single machine.** Purely local SQLite. There is no multi-machine or cloud sync — by design.
- **Single mode of working.** The execution guard is the only approval gate; there is no interactive "confirm every replay" mode beyond it.
- **FTS token boundaries.** Search treats punctuation as separators. A pure-punctuation query (e.g. `***`) is intentionally ignored by the FTS engine.

---

## Development

```sh
cargo build                     # dev build
cargo test                      # 147 tests
cargo clippy --all-targets -- -D warnings
cargo fmt
```

Repository layout:

- `src/capture.rs` — live capture event handling (exit code, duration, dedup)
- `src/redaction.rs` — the secret engine (regexes compiled once in a `OnceLock`, unit-tested for every family)
- `src/ranking.rs` — the context-aware ranking model (FTS5 `bm25` + success/recency/project signals)
- `src/context.rs` — git-project detection + the risk classifier
- `src/ui/tui.rs` — the ratatui interactive browser
- `src/import.rs` — legacy shell-history ingestion (zsh + bash)
- `src/doctor.rs` — the diagnostic report
- `src/db/` — SQLite WAL storage, `r2d2` pooling, migrations, FTS5 schema
- `hooks/` — the bash and zsh hook scripts `hs init` emits

---

**hs** is deliberately small: a local, fast, honest memory of what worked. Ship it, run it, let it learn.

Released under the [MIT License](https://crates.io/crates/hs-cli) (declared in the package metadata).