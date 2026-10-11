mod color;
mod completion;
mod filter;
mod parser;
mod processor;

use anyhow::Context;
use clap::{Parser, ValueEnum};
use std::collections::HashSet;
use std::io::{self, BufRead, IsTerminal, Write};
use std::process::{Command, Stdio};
use terminal_size::{terminal_size, Width};

use filter::{LogLevel, PackageFilter, TagFilter};
use parser::{parse_current_app, parse_ps_line};
use processor::{Config, Processor};

/// Kills the spawned `adb logcat` when dropped.
struct ChildGuard(Option<std::process::Child>);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Shell {
    Zsh,
    Bash,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ColorChoice {
    Auto,
    Always,
    Never,
}

#[derive(Parser, Debug)]
#[command(
    name = "logcat",
    version,
    about = "Filter and highlight adb logcat output"
)]
struct Args {
    /// Application package name(s)
    #[arg(value_name = "package")]
    packages: Vec<String>,

    /// Width of log tag
    #[arg(short = 'w', long = "tag-width", default_value_t = 23)]
    tag_width: usize,

    /// Minimum level to be displayed (V, D, I, W, E, F)
    #[arg(short = 'l', long = "min-level", value_enum, default_value_t = LogLevel::Verbose, ignore_case = true)]
    min_level: LogLevel,

    /// Color garbage collection
    #[arg(long = "color-gc")]
    color_gc: bool,

    /// Always display the tag name
    #[arg(long = "always-display-tags")]
    always_tags: bool,

    /// Filter logcat by current running app
    #[arg(long = "current")]
    current_app: bool,

    /// Device serial number (adb -s option)
    #[arg(short = 's', long = "serial", conflicts_with_all = ["use_device", "use_emulator"])]
    device_serial: Option<String>,

    /// Use first device for log input (adb -d option)
    #[arg(short = 'd', long = "device", conflicts_with = "use_emulator")]
    use_device: bool,

    /// Use first emulator for log input (adb -e option)
    #[arg(short = 'e', long = "emulator")]
    use_emulator: bool,

    /// Clear the entire log before running
    #[arg(short = 'c', long = "clear")]
    clear_logcat: bool,

    /// Filter output by specified tag(s) (regex supported)
    #[arg(short = 't', long = "tag")]
    tag: Vec<String>,

    /// Filter output by ignoring specified tag(s) (regex supported)
    #[arg(short = 'i', long = "ignore-tag")]
    ignored_tag: Vec<String>,

    /// Print all log messages
    #[arg(short = 'a', long = "all")]
    all: bool,

    /// Display timestamps (from time or threadtime format)
    #[arg(short = 'T', long = "timestamp")]
    timestamp: bool,

    /// Display process ID
    #[arg(short = 'p', long = "show-pid")]
    show_pid: bool,

    /// Output structured JSON lines
    #[arg(long = "json")]
    json: bool,

    /// When to colorize output
    #[arg(long = "color", value_enum, default_value_t = ColorChoice::Auto)]
    color: ColorChoice,

    /// Generate shell completions (zsh, bash)
    #[arg(long = "generate-completions", value_enum)]
    generate_completions: Option<Shell>,

    /// Hidden helper for shell dynamic package completion
    #[arg(long = "complete-packages", hide = true)]
    complete_packages: bool,
}

fn build_adb_base(args: &Args) -> Vec<String> {
    let mut cmd = vec!["adb".to_string()];
    if let Some(ref serial) = args.device_serial {
        cmd.push("-s".to_string());
        cmd.push(serial.clone());
    }
    if args.use_device {
        cmd.push("-d".to_string());
    }
    if args.use_emulator {
        cmd.push("-e".to_string());
    }
    cmd
}

fn fetch_current_package(base_cmd: &[String]) -> Option<String> {
    let mut cmd = Command::new(&base_cmd[0]);
    cmd.args(&base_cmd[1..])
        .args(["shell", "dumpsys", "activity", "activities"]);

    let output = cmd.output().ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_current_app(&stdout)
}

fn fetch_initial_pids(base_cmd: &[String], filter: &PackageFilter) -> HashSet<String> {
    let mut pids = HashSet::new();

    // Try `ps -A` first (for modern Android 8.0+), fallback to `ps`
    for ps_arg in [&["shell", "ps", "-A"][..], &["shell", "ps"][..]] {
        let mut cmd = Command::new(&base_cmd[0]);
        cmd.args(&base_cmd[1..]).args(ps_arg);

        if let Ok(output) = cmd.output() {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let mut found_any = false;
                for line in stdout.lines() {
                    if let Some((pid, proc_name)) = parse_ps_line(line) {
                        found_any = true;
                        if filter.matches(&proc_name) {
                            pids.insert(pid);
                        }
                    }
                }
                if found_any {
                    break;
                }
            }
        }
    }

    pids
}

/// Clears the device's ring buffer before streaming (`adb logcat -c`).
fn clear_logcat(base_cmd: &[String]) {
    let mut cmd = Command::new(&base_cmd[0]);
    cmd.args(&base_cmd[1..]).args(["logcat", "-c"]);
    let _ = cmd.status();
}

/// Parses the output of `adb shell pm list packages`, stripping the `package:` prefix
/// and returning a sorted, deduplicated list of package identifiers.
fn parse_pm_list_packages(stdout: &str) -> Vec<String> {
    let mut pkgs = Vec::new();
    for line in stdout.lines() {
        let trimmed = line.trim();
        if let Some(pkg) = trimmed.strip_prefix("package:") {
            let pkg = pkg.trim();
            if !pkg.is_empty() {
                pkgs.push(pkg.to_string());
            }
        }
    }
    pkgs.sort();
    pkgs.dedup();
    pkgs
}

/// Invokes `adb shell pm list packages` on the target device and returns all installed packages.
/// Fails silently and returns an empty list if no device is connected or adb fails.
fn fetch_installed_packages(base_cmd: &[String]) -> Vec<String> {
    let mut cmd = Command::new(&base_cmd[0]);
    cmd.args(&base_cmd[1..])
        .args(["shell", "pm", "list", "packages"]);

    if let Ok(output) = cmd.output() {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            return parse_pm_list_packages(&stdout);
        }
    }
    Vec::new()
}

/// Reads one line, replacing invalid UTF-8 with U+FFFD instead of failing
/// (logcat output regularly contains binary junk). Equivalent to pidcat's
/// `decode('utf-8', 'replace')`. Returns `Ok(None)` at EOF.
fn read_line_lossy<R: BufRead>(reader: &mut R, buf: &mut Vec<u8>) -> io::Result<Option<String>> {
    buf.clear();
    if reader.read_until(b'\n', buf)? == 0 {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(buf).into_owned()))
}

fn main() -> anyhow::Result<()> {
    let mut args = Args::parse();

    if let Some(shell) = args.generate_completions {
        match shell {
            Shell::Zsh => print!("{}", completion::generate_zsh()),
            Shell::Bash => print!("{}", completion::generate_bash()),
        }
        return Ok(());
    }

    if args.complete_packages {
        let base_adb = build_adb_base(&args);
        for pkg in fetch_installed_packages(&base_adb) {
            println!("{}", pkg);
        }
        return Ok(());
    }

    let stdout_is_tty = io::stdout().is_terminal();
    let use_color = match args.color {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => stdout_is_tty && std::env::var_os("NO_COLOR").is_none(),
    };

    let get_term_width = || -> Option<usize> {
        if stdout_is_tty {
            terminal_size().map(|(Width(w), _)| w as usize)
        } else {
            None
        }
    };

    let stdin_is_tty = io::stdin().is_terminal();
    let base_adb = build_adb_base(&args);

    if stdin_is_tty && args.current_app {
        if let Some(pkg) = fetch_current_package(&base_adb) {
            eprintln!("Filtering by current app: {}", pkg);
            args.packages.push(pkg);
        } else {
            eprintln!("Warning: Could not determine current running app.");
        }
    }

    let package_filter = PackageFilter::new(&args.packages);
    let tag_filter = TagFilter::new(&args.tag, &args.ignored_tag)?;
    let initial_pids = if stdin_is_tty {
        fetch_initial_pids(&base_adb, &package_filter)
    } else {
        HashSet::new()
    };
    let config = Config {
        tag_width: args.tag_width,
        min_level: args.min_level,
        color_gc: args.color_gc,
        always_tags: args.always_tags,
        // `-a` (or no package at all) shows every log line, but banners still
        // only appear for processes matching the package list.
        show_all: args.all || args.packages.is_empty(),
        use_color,
        show_timestamp: args.timestamp,
        show_pid: args.show_pid,
        json: args.json,
    };
    let mut processor = Processor::new(config, package_filter, tag_filter, initial_pids);

    let mut _child_guard = ChildGuard(None);
    let mut reader: Box<dyn BufRead> = if !stdin_is_tty {
        Box::new(io::BufReader::new(io::stdin()))
    } else {
        if args.clear_logcat {
            clear_logcat(&base_adb);
        }

        let log_format = if args.timestamp || args.json {
            "time"
        } else {
            "brief"
        };
        let mut cmd = Command::new(&base_adb[0]);
        cmd.args(&base_adb[1..])
            .args(["logcat", "-v", log_format])
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());

        let mut child = cmd.spawn().with_context(|| {
            format!(
                "Failed to execute '{}'. Ensure Android SDK platform-tools is installed and '{}' is in PATH.",
                base_adb[0], base_adb[0]
            )
        })?;
        let stdout = child.stdout.take().expect("Failed to open child stdout");
        _child_guard = ChildGuard(Some(child));
        Box::new(io::BufReader::new(stdout))
    };

    let mut stdout = io::stdout().lock();
    let mut byte_buf = Vec::new();

    while let Some(raw_line) = read_line_lossy(&mut reader, &mut byte_buf)? {
        let line = raw_line.trim_end_matches(['\r', '\n']);
        let output = processor.process_line(line, get_term_width());
        if output.is_empty() {
            continue;
        }
        if let Err(e) = stdout.write_all(output.as_bytes()) {
            if e.kind() == io::ErrorKind::BrokenPipe {
                break;
            }
            return Err(e.into());
        }
    }

    if let Some(mut child) = _child_guard.0.take() {
        let status = child.wait()?;
        if !status.success() {
            let code = status.code().unwrap_or(1);
            std::process::exit(code);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression (#1): invalid UTF-8 mid-stream must not abort reading.
    #[test]
    fn test_read_line_lossy_survives_invalid_utf8() {
        let input: &[u8] = b"D/A( 1): before\nD/A( 1): bad \xff\xfe bytes\nD/A( 1): after\n";
        let mut reader = io::BufReader::new(input);
        let mut buf = Vec::new();

        let mut lines = Vec::new();
        while let Some(line) = read_line_lossy(&mut reader, &mut buf).unwrap() {
            lines.push(line);
        }

        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "D/A( 1): before\n");
        assert_eq!(lines[1], "D/A( 1): bad \u{FFFD}\u{FFFD} bytes\n");
        assert_eq!(lines[2], "D/A( 1): after\n");
    }

    #[test]
    fn test_read_line_lossy_last_line_without_newline() {
        let mut reader = io::BufReader::new(&b"no newline"[..]);
        let mut buf = Vec::new();
        assert_eq!(
            read_line_lossy(&mut reader, &mut buf).unwrap().as_deref(),
            Some("no newline")
        );
        assert_eq!(read_line_lossy(&mut reader, &mut buf).unwrap(), None);
    }

    #[test]
    fn test_parse_pm_list_packages_handles_crlf_and_dedups() {
        let raw = "package:com.example.b\r\npackage:com.example.a\nnot_a_package_line\npackage:com.example.b\n";
        let pkgs = parse_pm_list_packages(raw);
        assert_eq!(pkgs, vec!["com.example.a", "com.example.b"]);
    }

    #[test]
    fn test_args_defaults_and_options() {
        let args = Args::try_parse_from(["logcat", "com.example.app"]).unwrap();
        assert_eq!(args.packages, vec!["com.example.app"]);
        assert_eq!(args.tag_width, 23);
        assert_eq!(args.min_level, LogLevel::Verbose);
        assert!(!args.all);
        assert!(!args.timestamp);
        assert!(!args.show_pid);
        assert!(!args.json);
    }

    #[test]
    fn test_args_device_conflicts() {
        // -s and -d cannot be used together
        assert!(Args::try_parse_from(["logcat", "-s", "emulator-5554", "-d"]).is_err());
        // -s and -e cannot be used together
        assert!(Args::try_parse_from(["logcat", "-s", "emulator-5554", "-e"]).is_err());
        // -d and -e cannot be used together
        assert!(Args::try_parse_from(["logcat", "-d", "-e"]).is_err());
    }

    #[test]
    fn test_args_case_insensitive_level() {
        let args = Args::try_parse_from(["logcat", "-l", "warning"]).unwrap();
        assert_eq!(args.min_level, LogLevel::Warn);

        let args2 = Args::try_parse_from(["logcat", "-l", "e"]).unwrap();
        assert_eq!(args2.min_level, LogLevel::Error);
    }

    #[test]
    fn test_args_completion_and_json_flags() {
        let args = Args::try_parse_from(["logcat", "--generate-completions", "zsh"]).unwrap();
        assert!(matches!(args.generate_completions, Some(Shell::Zsh)));

        let args_json = Args::try_parse_from(["logcat", "--json", "-T", "-p"]).unwrap();
        assert!(args_json.json);
        assert!(args_json.timestamp);
        assert!(args_json.show_pid);
    }
}
