# logcat

A high-performance Rust reimplementation and modern evolution of [JakeWharton/pidcat](https://github.com/JakeWharton/pidcat) to filter, format, and highlight Android `adb logcat` output.

[![Crates.io](https://img.shields.io/crates/v/logcat-cli.svg)](https://crates.io/crates/logcat-cli)
[![docs.rs](https://docs.rs/logcat-cli/badge.svg)](https://docs.rs/logcat-cli)
![Rust](https://img.shields.io/badge/rust-1.80%2B-orange.svg)
![Tests](https://img.shields.io/badge/tests-71%20passing-brightgreen.svg)
![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)

## Features

- **Package Filtering**: Filter logs by application package (`com.example.app`), multiple packages, or specific process names (`com.example.app:remote`).
- **Dynamic Process Tracking**: Detects new processes on app launch and untracks PIDs on termination, rendering colored lifecycle banners.
- **Color-Coded Tags & Levels**:
  - Tags right-aligned with unique colors from an LRU allocation palette.
  - Distinct sticky colors for known system tags (`ActivityManager`, `dalvikvm`, `AndroidRuntime`, `DEBUG`, etc.).
  - Bold, high-contrast log level badges (`V`, `D`, `I`, `W`, `E`, `F`).
  - Consecutive duplicate tags suppressed for readability (toggle with `--always-display-tags`).
- **Unicode Display Width Alignment**: Multi-column CJK glyphs and emojis align down to the exact terminal column.
- **Smart Terminal Wrapping**: Wraps neatly to terminal width and indents to align under the message column, preserving ANSI escape codes without truncation.
- **Timestamp Support (`-T`, `--timestamp`)**: Optional fixed-width timestamp column from `time` or `threadtime` streams.
- **PID Display (`-p`, `--show-pid`)**: Fixed-width PID column aligned next to the log tag.
- **Structured NDJSON Output (`--json`)**: Machine-readable JSON lines for piping into `jq` or log collectors.
- **Shell Completions (`--generate-completions <zsh|bash>`)**: Autocompletion for zsh and bash, including **dynamic Android package completion** queried live via `adb`.
- **Native Crash Backtraces**: Automatically attributes `DEBUG`/`debuggerd` native backtraces (`#00 pc ...`) to the crashed app PID.
- **Current Foreground App (`--current`)**: Automatically inspects `dumpsys activity activities` to monitor whichever app is currently in the foreground.
- **Piped Input Support**: Works both as an active streamer spawning `adb logcat` and as a Unix filter receiving piped input (e.g. `cat dump.log | logcat com.example.app`).

For detailed internal design, state machine flow, and stream parsing mechanics, see [ARCHITECTURE.md](ARCHITECTURE.md).

---

## Installation

### Via Homebrew

```bash
brew install GBouerat/tap/logcat
```

### From crates.io

```bash
cargo install logcat-cli
```

### From Source

Ensure you have Rust and Cargo installed:

```bash
cargo install --path .
```

---

## Shell Completions

`logcat` includes dynamic package autocompletion for `zsh` and `bash`.

### Zsh
Add to `~/.zshrc`:
```zsh
source <(logcat --generate-completions zsh)
```

### Bash
Add to `~/.bashrc`:
```bash
source <(logcat --generate-completions bash)
```

Pressing `<TAB>` after `logcat ` will dynamically autocomplete installed package names from your connected device or emulator!

---

## Usage Examples

```bash
# Filter logs for a specific application package
logcat com.example.app

# Filter for the app currently running in the foreground
logcat --current

# Display timestamps and process IDs
logcat -T -p com.example.app

# Minimum log level (V, D, I, W, E, F or verbose, debug, info, warn, error, fatal)
logcat -l W com.example.app

# Filter by tags (regex)
logcat -t "Retrofit|OkHttp" com.example.app

# Ignore specific tags (regex)
logcat -i "Glide" com.example.app

# Stream structured JSON lines into jq
logcat --json com.example.app | jq 'select(.level == "E")'

# Target specific device or emulator
logcat -s <device-serial> com.example.app
logcat -d com.example.app
logcat -e com.example.app

# Clear logcat before streaming
logcat -c com.example.app

# Print all log messages across the device
logcat -a
```

---

## Command-Line Options

```
Usage: logcat [OPTIONS] [package]...

Arguments:
  [package]...  Application package name(s)

Options:
  -w, --tag-width <TAG_WIDTH>           Width of log tag [default: 23]
  -l, --min-level <MIN_LEVEL>           Minimum level to be displayed (V, D, I, W, E, F) [default: V]
      --color-gc                        Color garbage collection
      --always-display-tags             Always display the tag name
      --current                         Filter logcat by current running app
  -s, --serial <DEVICE_SERIAL>          Device serial number (adb -s option)
  -d, --device                          Use first device for log input (adb -d option)
  -e, --emulator                        Use first emulator for log input (adb -e option)
  -c, --clear                           Clear the entire log before running
  -t, --tag <TAG>                       Filter output by specified tag(s) (regex supported)
  -i, --ignore-tag <IGNORED_TAG>        Filter output by ignoring specified tag(s) (regex supported)
  -a, --all                             Print all log messages
  -T, --timestamp                       Display timestamps (from time or threadtime format)
  -p, --show-pid                        Display process ID
      --json                            Output structured JSON lines
      --color <COLOR>                   When to colorize output [default: auto] [possible values: auto, always, never]
      --generate-completions <SHELL>    Generate shell completions [possible values: zsh, bash]
  -h, --help                            Print help
  -V, --version                         Print version
```

---

*Generated entirely by Gemini through Antigravity.*
