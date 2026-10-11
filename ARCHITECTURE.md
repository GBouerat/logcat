# Technical Architecture & Internal Design

This document details the internal design, stream processing pipeline, and architectural decisions behind `logcat`.

---

## 1. System Architecture Overview

```
                      ┌─────────────────────────────────────────┐
                      │               src/main.rs               │
                      │  - CLI parsing (clap)                   │
                      │  - Terminal width / color detection     │
                      │  - Subprocess execution & ChildGuard    │
                      │  - Lossy UTF-8 stream reader            │
                      └────────────┬──────────────┬─────────────┘
                                   │              │
                   ┌───────────────┴────┐     ┌───┴─────────────┐
                   │                    │     │                 │
                   ▼                    ▼     ▼                 ▼
            ┌──────────────┐     ┌──────────────┐         ┌──────────────┐
            │src/parser.rs │     │src/filter.rs │         │ src/color.rs │
            │- Regex specs │     │- Packages    │         │- 6-color LRU │
            │- brief/time/ │     │- Tag regexes │         │- Badges      │
            │  threadtime  │     │- Min levels  │         │- Sticky tags │
            └──────┬───────┘     └──────┬───────┘         └──────┬───────┘
                   │                    │                        │
                   └────────────────────┼────────────────────────┘
                                        ▼
                         ┌─────────────────────────────┐
                         │      src/processor.rs       │
                         │  - Stateful stream processor│
                         │  - Tag deduplication        │
                         │  - Unicode display width    │
                         │  - Multi-line word wrapping │
                         │  - NDJSON serialization     │
                         └─────────────────────────────┘
```

The system is deliberately designed around a **clean separation between I/O and state processing**:
- **`Processor`** in [`src/processor.rs`](src/processor.rs) is completely I/O-free: it accepts one raw log line at a time and returns the formatted text (or empty string). This enables deterministic, sub-millisecond unit tests for all complex scenarios (banners, Unicode wrapping, deduplication, native crashes, NDJSON output).
- **`main`** in [`src/main.rs`](src/main.rs) handles operating system interaction, terminal signal handling, and child process lifecycle.

---

## 2. Core Components Deep-Dive

### 2.1 The Stateful Engine (`src/processor.rs`)

`Processor` maintains runtime state across streamed log lines:

```rust
pub struct Processor {
    config: Config,
    package_filter: PackageFilter,
    tag_filter: TagFilter,
    pids: HashSet<String>,          // Actively tracked application PIDs
    last_tag: Option<String>,        // Tag of last printed line (for deduplication)
    app_pid: Option<String>,         // Most recently launched app PID (for crash backtraces)
    palette: Palette,                // LRU color rotation
}
```

#### Tag Deduplication
When consecutive lines share the same log tag, the tag name is omitted and replaced with spaces (`tag_width` columns) to reduce visual clutter. When a process lifecycle banner (start or death) is emitted, `last_tag` is cleared to ensure the next log message immediately re-identifies its tag.

#### Unicode Visual Column Alignment
Standard Rust string operations (`len()` or `chars().count()`) measure byte lengths or code points, which fail for:
- Double-width East Asian glyphs (CJK).
- Multi-byte emojis (e.g. 🚀, ⚠️).
- Zero-width combining characters.

`format_tag_unicode` uses `unicode-width` to measure exact terminal column cells:
- **Left-padding**: Pads with visual space deficit when `tag.width() < tag_width`.
- **Right-truncation**: Iterates backwards via `char_indices().rev()` to preserve the rightmost suffix of long tags while preserving character boundaries.

#### ANSI-Aware Word Wrapping
`indent_wrap` formats messages exceeding the terminal width:
1. Strips and bypasses ANSI color escape sequences when computing the visible column cursor.
2. Wraps precisely when `visible_col + ch_width > wrap_area`.
3. Indents wrapped lines by `header_size` spaces to maintain visual alignment under the message column.

---

### 2.2 Multi-Format Log Parser (`src/parser.rs`)

Android `adb logcat` supports multiple output formats depending on command-line flags and device configurations. `parse_log_line` dynamically handles three distinct formats:

| Format | Pattern | Example |
| :--- | :--- | :--- |
| **`brief`** | `^([A-Z])/(.+?)\( *(\d+)\): (.*?)$` | `D/dalvikvm( 1234): GC freed 2048K` |
| **`time`** | `^(\d{2}-\d{2}\s+\d{2}:\d{2}:\d{2}\.\d{3})\s+([A-Z])/(.+?)\( *(\d+)\): (.*?)$` | `10-11 17:14:17.248 W/linker( 232): Warning ...` |
| **`threadtime`** | `^(\d{2}-\d{2}\s+\d{2}:\d{2}:\d{2}\.\d{3})\s+(\d+)\s+\d+\s+([A-Z])\s+(.+?)\s*:\s?(.*)$` | `10-11 12:00:00.000 1234 5678 D MyTag: hello` |

#### Process Lifecycle Detection
- **Process Start**: Matches standard Android ActivityManager creation strings across Android 4.x (`PID_START`), modern Android 5.1–15 (`PID_START_5_1`), and Dalvik VM launch lines (`PID_START_DALVIK`).
- **Process Termination**: Matches modern death lines (`Process <pkg> (pid <pid>) has died...`) as well as legacy `Killing ...` and `No longer want ...` patterns.
- **Native Crash Attribution**: Native crashes logged by `DEBUG` (`#00 pc ...`) originate from a separate system PID. When detected, the event is re-attributed to `app_pid` so crashes are never filtered out.

---

### 2.3 Terminal Color & Palette Allocation (`src/color.rs`)

1. **LRU Palette Cache**: Dynamic tags cycle through a 6-color rotating palette (`Red`, `Green`, `Yellow`, `Blue`, `Magenta`, `Cyan`). Recently used tags are bumped to the back of the queue, ensuring concurrent tags have contrasting colors.
2. **Sticky System Colors**: High-frequency system tags are pinned to static colors:
   - `ActivityManager`, `ActivityThread`, `dalvikvm`, `jdwp`, `StrictMode` -> White
   - `AndroidRuntime` -> Cyan
   - `DEBUG` -> Yellow
3. **Log Level Badges**: Bold badges with colored background and contrasting foreground:
   - `V` (Verbose): Black on White
   - `D` (Debug): Black on Blue
   - `I` (Info): Black on Green
   - `W` (Warn): Black on Yellow
   - `E` / `F` (Error / Fatal): Black on Red
4. **`NO_COLOR` Compliance**: Automatically disables ANSI escape codes when stdout is not a TTY or when the `NO_COLOR` environment variable is set.

---

### 2.4 Structured JSON Streaming (`JsonEvent`)

When invoked with `--json`, `logcat` serializes events into **newline-delimited JSON (NDJSON)**:

```json
{"type":"proc_start","package":"com.example.app","target":"activity .Main","pid":"1234","uid":"10000","gids":"3003"}
{"type":"log","timestamp":"10-11 17:14:17.248","level":"I","tag":"ActivityManager","pid":"1234","message":"Started"}
{"type":"proc_death","package":"com.example.app","pid":"1234"}
```

In JSON mode:
- Raw message content is emitted without ANSI color codes or line wrapping.
- All filters (package, PID, tag whitelist/blacklist, log level) apply identically.

---

### 2.5 Shell Completions with Dynamic Package Resolution (`src/completion.rs`)

The `--generate-completions` flag emits shell completion scripts for **Zsh** and **Bash**:
- Standard flags and argument completions are statically specified.
- Positional package arguments hook into `logcat --complete-packages 2>/dev/null`.
- `logcat --complete-packages` queries the connected device via `adb shell pm list packages`, strips `package:`, and outputs sorted, deduplicated package names live in sub-50ms latency.

---

### 2.6 Robust Process & Stream Management (`src/main.rs`)

- **Subprocess Guard (`ChildGuard`)**: Implements `Drop` to send `SIGKILL` and call `.wait()` on the child `adb logcat` process upon exit or `SIGINT`, preventing orphaned background processes.
- **Lossy Line Reader (`read_line_lossy`)**: Logcat output occasionally contains raw memory dumps or binary garbage. Rather than panicking on invalid UTF-8, bytes are decoded with replacement character (`U+FFFD`), matching `pidcat`'s `decode('utf-8', 'replace')`.

---

## 3. Testing Strategy

The test suite consists of **71 automated tests** covering three tiers:

1. **Unit Tests (Internal Logic)**:
   - Parsing of brief, time, and threadtime logs with edge-case timestamps.
   - Exact CJK / emoji display width alignment and truncation.
   - State machine PID tracking, multiple processes per package, and death events.
   - Tag deduplication across process start/death banners.
   - Case-insensitive log level aliases.
2. **CLI Argument Validation**:
   - Conflicts between `-s`, `-d`, and `-e` device targets.
   - Option defaults and flag combinations.
3. **End-to-End Integration Tests (`tests/cli.rs`)**:
   - Executes the compiled `logcat` binary in separate subprocesses.
   - Validates stdout/stderr and exit codes against piped log streams.
