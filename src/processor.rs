//! Stateful, I/O-free processing of logcat lines.
//!
//! [`Processor::process_line`] takes one raw Android logcat line and returns
//! exactly the formatted text that should be printed for it (or empty string).
//! Keeping this completely free of adb or terminal access enables fast, deterministic
//! testing of all lifecycle, formatting, and filtering scenarios.

use serde::Serialize;
use std::collections::HashSet;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::color::{colorize, format_level, format_pid, format_timestamp, Color, Palette};
use crate::filter::{LogLevel, PackageFilter, TagFilter};
use crate::parser::{apply_rules, parse_death, parse_log_line, parse_start_proc, BACKTRACE_LINE};

/// Visual width reserved for the formatted timestamp column (`MM-DD HH:MM:SS.mmm`).
pub const TIMESTAMP_WIDTH: usize = 18;

/// Visual width reserved for the right-aligned PID column.
pub const PID_WIDTH: usize = 5;

/// Machine-readable NDJSON event emitted when `--json` is enabled.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum JsonEvent<'a> {
    /// Process creation event detected from ActivityManager.
    #[serde(rename = "proc_start")]
    ProcStart {
        package: &'a str,
        target: &'a str,
        pid: &'a str,
        uid: &'a str,
        gids: &'a str,
    },
    /// Process termination event detected from ActivityManager.
    #[serde(rename = "proc_death")]
    ProcDeath { package: &'a str, pid: &'a str },
    /// Standard logcat message entry.
    #[serde(rename = "log")]
    Log {
        timestamp: Option<&'a str>,
        level: char,
        tag: &'a str,
        pid: &'a str,
        message: &'a str,
    },
}

/// Static display configuration passed into [`Processor`].
#[derive(Debug, Clone)]
pub struct Config {
    /// Maximum width in terminal columns for log tags (0 hides the tag column).
    pub tag_width: usize,
    /// Minimum log level threshold to display.
    pub min_level: LogLevel,
    /// Whether to highlight GC statistics in green/yellow.
    pub color_gc: bool,
    /// When true, re-prints tag name on every line rather than blanking out duplicates.
    pub always_tags: bool,
    /// When true, show log lines from every PID, not just tracked application processes (`-a`).
    pub show_all: bool,
    /// Whether ANSI terminal colors are enabled.
    pub use_color: bool,
    /// Whether to display the timestamp column.
    pub show_timestamp: bool,
    /// Whether to display the PID column.
    pub show_pid: bool,
    /// Whether to emit structured NDJSON lines instead of human-formatted text.
    pub json: bool,
}

/// Stateful stream processor maintaining tracked PIDs and visual deduplication state.
pub struct Processor {
    config: Config,
    package_filter: PackageFilter,
    tag_filter: TagFilter,
    /// PIDs of the application processes whose logs are currently tracked.
    pids: HashSet<String>,
    /// Tag of the last printed line, used to avoid repeating it on consecutive lines.
    last_tag: Option<String>,
    /// PID of the most recently started matching process; native crash
    /// backtraces (logged by `DEBUG` under another PID) are attributed to it.
    app_pid: Option<String>,
    /// Palette managing LRU color rotation for tags.
    palette: Palette,
}

impl Processor {
    /// Creates a new `Processor` initialized with a set of pre-discovered PIDs.
    pub fn new(
        config: Config,
        package_filter: PackageFilter,
        tag_filter: TagFilter,
        initial_pids: HashSet<String>,
    ) -> Self {
        Self {
            config,
            package_filter,
            tag_filter,
            pids: initial_pids,
            last_tag: None,
            app_pid: None,
            palette: Palette::new(),
        }
    }

    /// Computes the exact total visual column width preceding the message body.
    ///
    /// Sums:
    /// - Optional timestamp: `TIMESTAMP_WIDTH + 1` (19 cols)
    /// - Tag column: `tag_width + 1` (if `tag_width > 0`)
    /// - Optional PID column: `PID_WIDTH + 1` (6 cols)
    /// - Level badge: 3 badge chars + 1 space (4 cols)
    ///
    /// This header size is used to indent continuation lines during word-wrapping
    /// and to span lifecycle banner backgrounds across the entire header region.
    fn header_size(&self) -> usize {
        let mut size = if self.config.tag_width > 0 {
            self.config.tag_width + 1 + 3 + 1
        } else {
            3 + 1
        };
        if self.config.show_timestamp {
            size += TIMESTAMP_WIDTH + 1;
        }
        if self.config.show_pid {
            size += PID_WIDTH + 1;
        }
        size
    }

    /// Processes one input line (without trailing newline) and returns the text to print.
    ///
    /// Returns an empty string if the line is filtered out or ignored.
    /// `term_width` specifies the available terminal width for wrapping, if known.
    pub fn process_line(&mut self, line: &str, term_width: Option<usize>) -> String {
        let mut out = String::new();
        let header_size = self.header_size();
        let use_color = self.config.use_color;

        // Process start
        if let Some(start) = parse_start_proc(line) {
            if self.package_filter.matches(start.package) {
                self.pids.insert(start.pid.to_string());
                self.app_pid = Some(start.pid.to_string());

                if self.config.json {
                    let event = JsonEvent::ProcStart {
                        package: start.package,
                        target: start.target,
                        pid: start.pid,
                        uid: start.uid,
                        gids: start.gids,
                    };
                    if let Ok(json) = serde_json::to_string(&event) {
                        out.push_str(&json);
                        out.push('\n');
                    }
                } else {
                    let banner_bg = banner_pad(header_size, Color::White, use_color);
                    let first_msg =
                        format!(" Process {} created for {}", start.package, start.target);
                    let second_msg = format!(
                        " PID: {}   UID: {}   GIDs: {}",
                        start.pid, start.uid, start.gids
                    );
                    out.push_str(&format!(
                        "\n{}{}\n{}{}\n\n",
                        banner_bg,
                        indent_wrap(&first_msg, term_width, header_size),
                        banner_bg,
                        indent_wrap(&second_msg, term_width, header_size)
                    ));
                    self.last_tag = None; // Ensure next log gets a tag printed
                }
            }
        }

        let Some(parsed) = parse_log_line(line) else {
            return out;
        };
        let tag = parsed.tag;
        let level_char = parsed.level;
        let mut owner = parsed.pid;
        let mut message = parsed.message;

        // Process death
        if let Some(death) = parse_death(tag, message) {
            if self.package_filter.matches(death.package) && self.pids.remove(death.pid) {
                if self.config.json {
                    let event = JsonEvent::ProcDeath {
                        package: death.package,
                        pid: death.pid,
                    };
                    if let Ok(json) = serde_json::to_string(&event) {
                        out.push_str(&json);
                        out.push('\n');
                    }
                } else {
                    let banner_bg = banner_pad(header_size, Color::Red, use_color);
                    let end_msg = format!(" Process {} (PID: {}) ended", death.package, death.pid);
                    out.push_str(&format!(
                        "\n{}{}\n\n",
                        banner_bg,
                        indent_wrap(&end_msg, term_width, header_size)
                    ));
                    self.last_tag = None; // Ensure next log gets a tag printed
                }
            }
        }

        // Make sure the backtrace is printed after a native crash
        if tag == "DEBUG" {
            let trimmed = message.trim_start();
            if BACKTRACE_LINE.is_match(trimmed) {
                message = trimmed;
                if let Some(app_pid) = self.app_pid.as_deref() {
                    owner = app_pid;
                }
            }
        }

        // Filtering
        if !self.config.show_all && !self.pids.contains(owner) {
            return out;
        }
        if let Some(log_level) = LogLevel::from_char(level_char) {
            if log_level < self.config.min_level {
                return out;
            }
        }
        if !self.tag_filter.allows(tag) {
            return out;
        }

        if self.config.json {
            let event = JsonEvent::Log {
                timestamp: parsed.timestamp,
                level: level_char,
                tag,
                pid: owner,
                message,
            };
            if let Ok(json) = serde_json::to_string(&event) {
                out.push_str(&json);
                out.push('\n');
            }
            return out;
        }

        // Timestamp column: fixed-width (18 chars)
        if self.config.show_timestamp {
            if let Some(ts) = parsed.timestamp {
                let formatted_ts = if ts.len() == TIMESTAMP_WIDTH {
                    ts.to_string()
                } else if ts.len() < TIMESTAMP_WIDTH {
                    format!("{}{}", ts, " ".repeat(TIMESTAMP_WIDTH - ts.len()))
                } else {
                    ts[..TIMESTAMP_WIDTH].to_string()
                };
                out.push_str(&format_timestamp(&formatted_ts, use_color));
            } else {
                out.push_str(&" ".repeat(TIMESTAMP_WIDTH));
            }
            out.push(' ');
        }

        // Tag column: right-aligned, colored, omitted when repeated
        let tag_width = self.config.tag_width;
        if tag_width > 0 {
            if self.last_tag.as_deref() != Some(tag) || self.config.always_tags {
                self.last_tag = Some(tag.to_string());
                let color = self.palette.allocate_color(tag);
                let formatted_tag = format_tag_unicode(tag, tag_width);
                out.push_str(&colorize(&formatted_tag, Some(color), None, use_color));
            } else {
                out.push_str(&" ".repeat(tag_width));
            }
            out.push(' ');
        }

        // PID column: right-aligned in PID_WIDTH columns
        if self.config.show_pid {
            out.push_str(&format_pid(owner, PID_WIDTH, use_color));
            out.push(' ');
        }

        // Level badge
        out.push_str(&format_level(level_char, use_color));
        out.push(' ');

        // Message, with highlight rules applied
        let formatted_message = apply_rules(message, self.config.color_gc, use_color);
        out.push_str(&indent_wrap(&formatted_message, term_width, header_size));
        out.push('\n');

        out
    }
}

/// Formats a tag to fit exactly `tag_width` columns using Unicode display width.
/// If the tag exceeds `tag_width`, keeps the suffix of the tag (like pidcat's `tag[-width:]`).
/// If the tag is shorter, pads on the left with spaces.
fn format_tag_unicode(tag: &str, tag_width: usize) -> String {
    let total_width = tag.width();
    if total_width <= tag_width {
        let pad = " ".repeat(tag_width - total_width);
        format!("{}{}", pad, tag)
    } else {
        // Tag exceeds width: keep the suffix that fits tag_width columns
        let mut width_accum = 0;
        let mut start_byte = 0;
        for (i, ch) in tag.char_indices().rev() {
            let ch_w = ch.width().unwrap_or(0);
            if width_accum + ch_w > tag_width {
                break;
            }
            width_accum += ch_w;
            start_byte = i;
        }
        let suffix = &tag[start_byte..];
        let pad = " ".repeat(tag_width.saturating_sub(width_accum));
        format!("{}{}", pad, suffix)
    }
}

/// Colored block filling the header column of a start/death banner.
fn banner_pad(header_size: usize, bg: Color, use_color: bool) -> String {
    let pad = " ".repeat(header_size.saturating_sub(1));
    colorize(&pad, None, Some(bg), use_color)
}

/// Wraps `message` to the terminal width, indenting continuation lines by
/// `header_size` so they align under the message column. ANSI escape
/// sequences are passed through without counting towards the width.
/// Character widths are calculated using Unicode display width.
pub fn indent_wrap(message: &str, term_width: Option<usize>, header_size: usize) -> String {
    let Some(width) = term_width else {
        return message.to_string();
    };

    if width <= header_size {
        return message.to_string();
    }

    let wrap_area = width - header_size;
    let expanded = message.replace('\t', "    ");
    let mut output = String::new();
    let mut visible_col = 0;
    let mut chars = expanded.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            output.push(ch);
            if chars.peek() == Some(&'[') {
                output.push(chars.next().unwrap());
                for c in chars.by_ref() {
                    output.push(c);
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }

        if ch == '\n' {
            output.push('\n');
            for _ in 0..header_size {
                output.push(' ');
            }
            visible_col = 0;
            continue;
        }

        let ch_width = ch.width().unwrap_or(0);
        if visible_col + ch_width > wrap_area && visible_col > 0 {
            output.push('\n');
            for _ in 0..header_size {
                output.push(' ');
            }
            visible_col = 0;
        }

        output.push(ch);
        visible_col += ch_width;
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- helpers -------------------------------------------------------

    const W: usize = 23;

    fn config() -> Config {
        Config {
            tag_width: W,
            min_level: LogLevel::Verbose,
            color_gc: false,
            always_tags: false,
            show_all: false,
            use_color: false,
            show_timestamp: false,
            show_pid: false,
            json: false,
        }
    }

    fn processor_with(config: Config, packages: &[&str], initial_pids: &[&str]) -> Processor {
        let packages: Vec<String> = packages.iter().map(|s| s.to_string()).collect();
        let show_all = config.show_all || packages.is_empty();
        let config = Config { show_all, ..config };
        Processor::new(
            config,
            PackageFilter::new(&packages),
            TagFilter::new(&[], &[]).unwrap(),
            initial_pids.iter().map(|s| s.to_string()).collect(),
        )
    }

    fn processor(packages: &[&str]) -> Processor {
        processor_with(config(), packages, &[])
    }

    /// Feeds every line of `input` and concatenates the output.
    fn run(p: &mut Processor, input: &str) -> String {
        input.lines().map(|l| p.process_line(l, None)).collect()
    }

    /// Expected output for a log line whose tag is printed.
    fn shown(tag: &str, level: char, msg: &str) -> String {
        format!("{:>W$}  {}  {}\n", tag, level, msg)
    }

    /// Expected output for a log line whose (repeated) tag is omitted.
    fn repeated(level: char, msg: &str) -> String {
        format!("{:W$}  {}  {}\n", "", level, msg)
    }

    const START_MINE_42: &str =
        "I/ActivityManager( 500): Start proc 42:com.mine/u0a1 for activity com.mine/.Main";

    // ---- package / PID tracking ------------------------------------------

    #[test]
    fn shows_only_logs_of_started_package() {
        let mut p = processor(&["com.mine"]);
        let out = run(
            &mut p,
            &format!("{START_MINE_42}\nD/T( 42): mine\nD/T( 7): foreign\n"),
        );
        assert!(out.contains("Process com.mine created for activity com.mine/.Main"));
        assert!(out.contains(" PID: 42 "));
        assert!(out.ends_with(&shown("T", 'D', "mine")));
        assert!(!out.contains("foreign"));
    }

    #[test]
    fn shows_logs_of_pids_found_at_startup() {
        let mut p = processor_with(config(), &["com.mine"], &["42"]);
        let out = run(&mut p, "D/T( 42): mine\nD/T( 7): foreign\n");
        assert_eq!(out, shown("T", 'D', "mine"));
    }

    #[test]
    fn catchall_package_includes_its_named_processes() {
        let mut p = processor(&["com.mine"]);
        let out = run(
            &mut p,
            "I/ActivityManager( 500): Start proc 43:com.mine:remote/u0a1 for service x\n\
             D/R( 43): remote\n",
        );
        assert!(out.contains("Process com.mine:remote created"));
        assert!(out.ends_with(&shown("R", 'D', "remote")));
    }

    #[test]
    fn named_process_excludes_main_process() {
        let mut p = processor(&["com.mine:remote"]);
        let out = run(
            &mut p,
            &format!(
                "{START_MINE_42}\n\
                 I/ActivityManager( 500): Start proc 43:com.mine:remote/u0a1 for service x\n\
                 D/T( 42): main\nD/T( 43): remote\n"
            ),
        );
        assert!(!out.contains("Process com.mine created"));
        assert!(!out.contains("main"));
        assert!(out.ends_with(&shown("T", 'D', "remote")));
    }

    #[test]
    fn legacy_start_format_records_uid_and_gids() {
        let mut p = processor(&["com.mine"]);
        let out = run(
            &mut p,
            "I/ActivityManager( 500): Start proc com.mine for activity com.mine/.Main: pid=42 uid=10001 gids=3003\n\
             D/T( 42): mine\n",
        );
        assert!(out.contains(" PID: 42   UID: 10001   GIDs: 3003"));
        assert!(out.ends_with(&shown("T", 'D', "mine")));
    }

    #[test]
    fn process_death_prints_banner_and_stops_tracking() {
        let mut p = processor(&["com.mine"]);
        let out = run(
            &mut p,
            &format!(
                "{START_MINE_42}\n\
                 I/ActivityManager( 500): Process com.mine (pid 42) has died.\n\
                 D/T( 42): after death\n"
            ),
        );
        assert!(out.contains("Process com.mine (PID: 42) ended"));
        assert!(!out.contains("after death"));
    }

    #[test]
    fn kill_message_stops_tracking() {
        let mut p = processor(&["com.mine"]);
        let out = run(
            &mut p,
            &format!(
                "{START_MINE_42}\n\
                 I/ActivityManager( 500): Killing 42:com.mine/u0a1: remove task\n\
                 D/T( 42): after kill\n"
            ),
        );
        assert!(out.contains("Process com.mine (PID: 42) ended"));
        assert!(!out.contains("after kill"));
    }

    #[test]
    fn death_of_untracked_pid_is_ignored() {
        let mut p = processor(&["com.mine"]);
        let out = run(
            &mut p,
            "I/ActivityManager( 500): Process com.mine (pid 42) has died.\n",
        );
        assert_eq!(out, "");
    }

    /// `-a com.mine`: every line is shown, but only com.mine gets banners.
    #[test]
    fn show_all_still_limits_banners_to_package() {
        let mut p = processor_with(
            Config {
                show_all: true,
                ..config()
            },
            &["com.mine"],
            &[],
        );
        let out = run(
            &mut p,
            "I/ActivityManager( 500): Start proc 99:com.other/u0a2 for activity x\n\
             D/T( 7): foreign\n",
        );
        assert!(!out.contains("Process com.other created"));
        assert!(out.contains("foreign"));
    }

    // ---- native crash ------------------------------------------------------

    #[test]
    fn native_backtrace_is_attributed_to_app() {
        let mut p = processor(&["com.mine"]);
        let out = run(
            &mut p,
            &format!(
                "{START_MINE_42}\n\
                 I/DEBUG( 77): backtrace:\n\
                 I/DEBUG( 77):     #00 pc 00012345  /system/lib/libc.so (abort+10)\n"
            ),
        );
        assert!(!out.contains("backtrace:"));
        assert!(out.ends_with(&shown(
            "DEBUG",
            'I',
            "#00 pc 00012345  /system/lib/libc.so (abort+10)"
        )));
    }

    // ---- filtering ---------------------------------------------------------

    #[test]
    fn min_level_hides_lower_levels() {
        let mut p = processor_with(
            Config {
                min_level: LogLevel::Warn,
                ..config()
            },
            &["com.mine"],
            &["42"],
        );
        let out = run(
            &mut p,
            "V/T( 42): v\nD/T( 42): d\nI/T( 42): i\nW/T( 42): w\nE/T( 42): e\nF/T( 42): f\n",
        );
        assert_eq!(
            out,
            shown("T", 'W', "w") + &repeated('E', "e") + &repeated('F', "f")
        );
    }

    #[test]
    fn tag_filter_is_applied() {
        let mut p = Processor::new(
            Config {
                show_all: true,
                ..config()
            },
            PackageFilter::new(&[]),
            TagFilter::new(&["Keep|Also".to_string()], &["Also".to_string()]).unwrap(),
            HashSet::new(),
        );
        let out = run(&mut p, "D/Keep( 1): a\nD/Also( 1): b\nD/KeepMore( 1): c\n");
        assert_eq!(out, shown("Keep", 'D', "a"));
    }

    #[test]
    fn ignores_non_log_and_bug_lines() {
        let mut p = processor(&[]);
        let out = run(
            &mut p,
            "not a log line\n--------- beginning of main\nD/T( 1): nativeGetEnabledTags x\n",
        );
        assert_eq!(out, "");
    }

    // ---- rendering ---------------------------------------------------------

    #[test]
    fn repeated_tag_is_omitted_until_it_changes() {
        let mut p = processor(&[]);
        let out = run(
            &mut p,
            "D/A( 1): one\nD/A( 1): two\nD/B( 1): three\nD/A( 1): four\n",
        );
        assert_eq!(
            out,
            shown("A", 'D', "one")
                + &repeated('D', "two")
                + &shown("B", 'D', "three")
                + &shown("A", 'D', "four")
        );
    }

    #[test]
    fn always_display_tags_repeats_tag() {
        let mut p = processor_with(
            Config {
                always_tags: true,
                ..config()
            },
            &[],
            &[],
        );
        let out = run(&mut p, "D/A( 1): one\nD/A( 1): two\n");
        assert_eq!(out, shown("A", 'D', "one") + &shown("A", 'D', "two"));
    }

    #[test]
    fn tag_is_reprinted_after_banner() {
        let mut p = processor(&["com.mine"]);
        let out = run(
            &mut p,
            &format!("D/T( 42): before\n{START_MINE_42}\nD/T( 42): after\n"),
        );
        assert!(out.ends_with(&shown("T", 'D', "after")));
    }

    #[test]
    fn long_tag_keeps_its_end() {
        let mut p = processor_with(
            Config {
                tag_width: 5,
                ..config()
            },
            &[],
            &[],
        );
        let out = run(&mut p, "D/ABCDEFGH( 1): msg\n");
        assert_eq!(out, "DEFGH  D  msg\n");
    }

    #[test]
    fn wraps_to_terminal_width() {
        let mut p = processor(&[]);
        // header is 28 columns, so 5 columns are left for the message
        let out = p.process_line("D/T( 1): 1234567890", Some(33));
        assert_eq!(
            out,
            format!("{:>W$}  D  12345\n{}67890\n", "T", " ".repeat(28))
        );
    }

    #[test]
    fn colors_are_emitted_only_when_enabled() {
        let mut plain = processor(&[]);
        assert!(!plain.process_line("D/T( 1): m", None).contains('\x1b'));

        let mut colored = processor_with(
            Config {
                use_color: true,
                ..config()
            },
            &[],
            &[],
        );
        assert!(colored.process_line("D/T( 1): m", None).contains('\x1b'));
    }

    // ---- indent_wrap ---------------------------------------------------------

    #[test]
    fn indent_wrap_no_terminal_width() {
        assert_eq!(indent_wrap("Hello world", None, 28), "Hello world");
    }

    #[test]
    fn indent_wrap_with_wrapping() {
        // term_width = 33, header_size = 28 -> wrap_area = 5
        let wrapped = indent_wrap("1234567890", Some(33), 28);
        assert_eq!(wrapped, format!("12345\n{}67890", " ".repeat(28)));
    }

    #[test]
    fn indent_wrap_with_ansi_sequences() {
        let wrapped = indent_wrap("\x1b[31m12345\x1b[0m67890", Some(33), 28);
        assert_eq!(
            wrapped,
            format!("\x1b[31m12345\x1b[0m\n{}67890", " ".repeat(28))
        );
    }

    #[test]
    fn test_zero_tag_width_wrapping() {
        let mut p = processor_with(
            Config {
                tag_width: 0,
                ..config()
            },
            &[],
            &[],
        );
        // header_size for tag_width 0 is 4 (" D  ")
        // term_width 9 => message wrapped after 5 chars
        let out = p.process_line("D/T( 1): 1234567890", Some(9));
        assert_eq!(out, " D  12345\n    67890\n");
    }

    #[test]
    fn test_unknown_log_level_not_filtered() {
        let mut p = processor_with(
            Config {
                min_level: LogLevel::Warn,
                ..config()
            },
            &[],
            &[],
        );
        let out = p.process_line("X/T( 1): special custom level", None);
        assert_eq!(out, "                      T  X  special custom level\n");
    }

    #[test]
    fn test_modern_process_death_stops_tracking() {
        let mut p = processor(&["com.mine"]);
        let out = run(
            &mut p,
            &format!(
                "{START_MINE_42}\n\
                 I/ActivityManager( 500): Process com.mine (pid 42) has died: fg  TOP\n\
                 D/T( 42): after death\n"
            ),
        );
        assert!(out.contains("Process com.mine (PID: 42) ended"));
        assert!(!out.contains("after death"));
    }

    #[test]
    fn test_unicode_tag_alignment() {
        use unicode_width::UnicodeWidthStr;

        // "测试" is 2 CJK characters, each of width 2 => total display width 4
        let formatted = format_tag_unicode("测试", 10);
        // Should have 6 spaces + "测试"
        assert_eq!(formatted, "      测试");
        assert_eq!(formatted.width(), 10);

        // Long CJK tag truncated to width 5: "这是一个很长的标签" (each 2 cols)
        // A suffix of width 5 cannot fit 3 chars (6 cols), so it takes 2 chars (4 cols) + 1 pad space = 5 cols
        let formatted_long = format_tag_unicode("这是一个标签", 5);
        assert_eq!(formatted_long.width(), 5);
    }

    #[test]
    fn test_unicode_message_wrapping() {
        // CJK characters take 2 columns each.
        // term_width = 33, header_size = 28 -> wrap_area = 5 columns.
        // "你好世界": "你" (2) + "好" (2) = 4 cols. "世" (2) would be 6 cols > 5, so wraps!
        let msg = "你好世界";
        let wrapped = indent_wrap(msg, Some(33), 28);
        assert_eq!(wrapped, format!("你好\n{}世界", " ".repeat(28)));
    }

    #[test]
    fn test_timestamp_display_enabled() {
        let mut cfg = config();
        cfg.show_timestamp = true;
        let mut p = processor_with(cfg, &["com.mine"], &["42"]);
        let out = p.process_line("10-11 17:14:17.248 D/MyTag( 42): hello timestamp", None);
        assert!(out.starts_with("10-11 17:14:17.248"));
        assert!(out.contains("MyTag"));
        assert!(out.contains("hello timestamp"));
    }

    #[test]
    fn test_timestamp_display_disabled() {
        let mut cfg = config();
        cfg.show_timestamp = false;
        let mut p = processor_with(cfg, &["com.mine"], &["42"]);
        let out = p.process_line("10-11 17:14:17.248 D/MyTag( 42): hello timestamp", None);
        assert!(!out.contains("10-11 17:14:17.248"));
        assert!(out.contains("MyTag"));
        assert!(out.contains("hello timestamp"));
    }

    #[test]
    fn test_timestamp_threadtime_format() {
        let mut cfg = config();
        cfg.show_timestamp = true;
        let mut p = processor_with(cfg, &["com.mine"], &["42"]);
        let out = p.process_line("10-11 17:14:17.248   42   42 D MyTag: threadtime log", None);
        assert!(out.starts_with("10-11 17:14:17.248"));
        assert!(out.contains("MyTag"));
        assert!(out.contains("threadtime log"));
    }

    #[test]
    fn test_json_output() {
        let mut cfg = config();
        cfg.json = true;
        let mut p = processor_with(cfg, &["com.mine"], &["42"]);
        let out = p.process_line("10-11 17:14:17.248 D/MyTag( 42): hello json", None);
        assert_eq!(
            out.trim(),
            r#"{"type":"log","timestamp":"10-11 17:14:17.248","level":"D","tag":"MyTag","pid":"42","message":"hello json"}"#
        );
    }

    #[test]
    fn test_json_proc_start_and_death() {
        let mut cfg = config();
        cfg.json = true;
        let mut p = processor_with(cfg, &["com.mine"], &[]);

        // Start
        let start_out = p.process_line(
            "I/ActivityManager( 500): Start proc 42:com.mine/u0a123 for activity com.mine/.Main",
            None,
        );
        assert!(start_out.contains(r#"{"type":"proc_start","package":"com.mine","target":"activity com.mine/.Main","pid":"42""#));

        // Death
        let death_out = p.process_line(
            "I/ActivityManager( 500): Process com.mine (pid 42) has died",
            None,
        );
        assert_eq!(
            death_out.trim(),
            r#"{"type":"proc_death","package":"com.mine","pid":"42"}"#
        );
    }

    #[test]
    fn test_pid_display_enabled() {
        let mut cfg = config();
        cfg.show_pid = true;
        let mut p = processor_with(cfg, &["com.mine"], &["42"]);
        let out = p.process_line("D/MyTag( 42): hello pid", None);
        assert!(out.contains("   42"));
        assert!(out.contains("MyTag"));
        assert!(out.contains("hello pid"));
    }

    #[test]
    fn test_pid_display_disabled() {
        let mut cfg = config();
        cfg.show_pid = false;
        let mut p = processor_with(cfg, &["com.mine"], &["42"]);
        let out = p.process_line("D/MyTag( 42): hello pid", None);
        assert!(!out.contains("   42"));
        assert!(out.contains("MyTag"));
        assert!(out.contains("hello pid"));
    }

    #[test]
    fn test_pid_and_timestamp_together() {
        let mut cfg = config();
        cfg.show_timestamp = true;
        cfg.show_pid = true;
        let mut p = processor_with(cfg, &["com.mine"], &["42"]);
        let out = p.process_line("10-11 17:14:17.248 D/MyTag( 42): hello both", None);
        assert!(out.starts_with("10-11 17:14:17.248"));
        assert!(out.contains("MyTag"));
        assert!(out.contains("   42"));
        assert!(out.contains("hello both"));
    }

    #[test]
    fn test_backtrace_without_app_pid_is_not_attributed() {
        let mut p = processor(&["com.mine"]);
        // DEBUG native crash line when no app process was ever started
        let out = p.process_line("I/DEBUG( 999): #00 pc 00012345 /system/lib/libc.so", None);
        // Untracked PID without app_pid should not be printed
        assert!(out.is_empty());
    }

    #[test]
    fn test_zero_tag_width_with_timestamp_and_pid() {
        let mut cfg = config();
        cfg.tag_width = 0;
        cfg.show_timestamp = true;
        cfg.show_pid = true;
        let mut p = processor_with(cfg, &["com.mine"], &["42"]);
        let out = p.process_line("10-11 17:14:17.248 D/MyTag( 42): message line", None);
        assert!(out.starts_with("10-11 17:14:17.248    42  D  message line"));
    }

    #[test]
    fn test_multiple_pids_for_same_package() {
        let mut p = processor(&["com.mine"]);
        // Start two processes for the same package
        let _ = p.process_line(
            "I/ActivityManager( 500): Start proc 101:com.mine/u0a1 for act .A",
            None,
        );
        let _ = p.process_line(
            "I/ActivityManager( 500): Start proc 102:com.mine:sub/u0a1 for act .B",
            None,
        );

        // One dies
        let death_out = p.process_line(
            "I/ActivityManager( 500): Process com.mine (pid 101) has died",
            None,
        );
        assert!(death_out.contains("Process com.mine (PID: 101) ended"));

        // Second process is still tracked and emits logs
        let log_out = p.process_line("D/MyTag( 102): still alive", None);
        assert!(log_out.contains("still alive"));
    }
}
