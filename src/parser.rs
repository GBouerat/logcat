//! Regular expressions and line parsers for Android logcat streams.
//!
//! Handles:
//! - Multi-format log lines: `brief`, `time`, and `threadtime`.
//! - Process startup lines across Android 4.x through modern Android 15.
//! - Process termination lines from `ActivityManager`.
//! - Native crash backtrace frames from `debuggerd` (`DEBUG`).
//! - Dumpsys output for identifying the current foreground package.
//! - Memory GC and StrictMode duration highlighting rules.

use regex::Regex;
use std::sync::LazyLock;

use crate::color::{termcolor, Color, RESET};

/// Android bug/trace lines that pollute stdout and should be discarded.
pub static BUG_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r".*nativeGetEnabledTags.*").unwrap());

/// Android logcat `brief` format: `<level>/<tag>(<pid>): <message>`
pub static LOG_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([A-Z])/(.+?)\( *(\d+)\): (.*?)$").unwrap());

/// Android logcat `time` format: `MM-DD HH:MM:SS.mmm <level>/<tag>(<pid>): <message>`
pub static TIME_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(\d{2}-\d{2}\s+\d{2}:\d{2}:\d{2}\.\d{3})\s+([A-Z])/(.+?)\( *(\d+)\): (.*?)$")
        .unwrap()
});

/// Android logcat default `threadtime` format: `MM-DD HH:MM:SS.mmm <pid> <tid> <level> <tag>: <message>`
pub static THREADTIME_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(\d{2}-\d{2}\s+\d{2}:\d{2}:\d{2}\.\d{3})\s+(\d+)\s+\d+\s+([A-Z])\s+(.+?)\s*:\s?(.*)$",
    )
    .unwrap()
});

/// Process start format (Android 4.x to 5.0):
/// `ActivityManager: Start proc <package> for <target>: pid=<pid> uid=<uid> gids=<gids>`
pub static PID_START: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^.*: Start proc ([a-zA-Z0-9._:]+) for ([a-z]+ [^:]+): pid=(\d+) uid=(\d+) gids=(.*)$",
    )
    .unwrap()
});

/// Modern process start format (Android 5.1+):
/// `ActivityManager: Start proc <pid>:<package>/<uid> for <target>`
pub static PID_START_5_1: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^.*: Start proc (\d+):([a-zA-Z0-9._:]+)/[a-z0-9]+ for (.*)$").unwrap()
});

/// Dalvik runtime startup message (legacy devices):
/// `E/dalvikvm( <pid> ): >>>>> <package> [ userId:0 | appId:<uid> ]`
pub static PID_START_DALVIK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?:\d{2}-\d{2}\s+\d{2}:\d{2}:\d{2}\.\d{3}\s+)?E/dalvikvm\(\s*(\d+)\): >>>>> ([a-zA-Z0-9._:]+) \[ userId:0 \| appId:(\d+) \]$",
    )
    .unwrap()
});

/// Process killed by ActivityManager:
/// `Killing <pid>:<package>/<uid>: <reason>`
pub static PID_KILL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^Killing (\d+):([a-zA-Z0-9._:]+)/[^:]+: (.*)$").unwrap());

/// Process retired when low on memory:
/// `No longer want <package> (pid <pid>): <reason>`
pub static PID_LEAVE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^No longer want ([a-zA-Z0-9._:]+) \(pid (\d+)\): .*$").unwrap());

/// Modern process death message (Android 7+):
/// `Process <package> (pid <pid>) has died: ...`
pub static PID_DEATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^Process ([a-zA-Z0-9._:]+) \(pid (\d+)\) has died(?:\.?:?.*)?$").unwrap()
});

/// Native crash backtrace line emitted by `DEBUG` / `debuggerd`:
/// `#00 pc 00012345 /system/lib/libc.so`
pub static BACKTRACE_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^#(.*?)pc\s(.*)$").unwrap());

/// StrictMode disk or network violation duration indicator
pub static STRICT_MODE_RULE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(StrictMode policy violation)(; ~duration=)(\d+ ms)").unwrap());

/// Dalvik / ART Garbage Collection pause and memory reclaimed statistics
pub static GC_RULE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(GC_(?:CONCURRENT|FOR_M?ALLOC|EXTERNAL_ALLOC|EXPLICIT) )(freed <?\d+.)(, \d+\% free \d+./\d+., )(paused \d+ms(?:\+\d+ms)?)").unwrap()
});

/// TaskRecord dumpsys pattern for older Android versions
pub static DUMPSYS_TASK_RECORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"TaskRecord.*A[= ]([^ \^\}\r\n]+)").unwrap());

/// ActivityRecord dumpsys pattern for Android 10+
pub static DUMPSYS_ACTIVITY_RECORD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:mResumedActivity|topResumedActivity|mFocusedApp|mFocusedWindow|mCurrentFocus)[:=].*?([a-zA-Z0-9._]+)/").unwrap()
});

/// Structured representation of a successfully parsed logcat entry.
#[derive(Debug, PartialEq, Eq)]
pub struct ParsedLogLine<'a> {
    /// Extracted timestamp string (e.g. `10-11 17:14:17.248`), if present in stream.
    pub timestamp: Option<&'a str>,
    /// Single-character level indicator ('V', 'D', 'I', 'W', 'E', 'F').
    pub level: char,
    /// Logging tag identifier.
    pub tag: &'a str,
    /// Process ID string.
    pub pid: &'a str,
    /// Message body payload.
    pub message: &'a str,
}

/// Parses an incoming raw line into a [`ParsedLogLine`], trying `time`, `threadtime`, and `brief` formats in order.
///
/// Returns `None` if the line is not a valid log entry (e.g. header marker, kernel message, bug line).
pub fn parse_log_line(line: &str) -> Option<ParsedLogLine<'_>> {
    if BUG_LINE.is_match(line) {
        return None;
    }

    // Try `time` format first: "MM-DD HH:MM:SS.mmm L/tag(pid): msg"
    if let Some(caps) = TIME_LINE.captures(line) {
        let timestamp = caps.get(1).map(|m| m.as_str());
        let level = caps.get(2)?.as_str().chars().next()?;
        let tag = caps.get(3)?.as_str().trim();
        let pid = caps.get(4)?.as_str();
        let message = caps.get(5)?.as_str();

        return Some(ParsedLogLine {
            timestamp,
            level,
            tag,
            pid,
            message,
        });
    }

    // Try `threadtime` format: "MM-DD HH:MM:SS.mmm pid tid L tag: msg"
    if let Some(caps) = THREADTIME_LINE.captures(line) {
        let timestamp = caps.get(1).map(|m| m.as_str());
        let pid = caps.get(2)?.as_str();
        let level = caps.get(3)?.as_str().chars().next()?;
        let tag = caps.get(4)?.as_str().trim();
        let message = caps.get(5)?.as_str();

        return Some(ParsedLogLine {
            timestamp,
            level,
            tag,
            pid,
            message,
        });
    }

    // Fall back to `brief` format: "L/tag(pid): msg"
    if let Some(caps) = LOG_LINE.captures(line) {
        let level = caps.get(1)?.as_str().chars().next()?;
        let tag = caps.get(2)?.as_str().trim();
        let pid = caps.get(3)?.as_str();
        let message = caps.get(4)?.as_str();

        return Some(ParsedLogLine {
            timestamp: None,
            level,
            tag,
            pid,
            message,
        });
    }

    None
}

/// Parsed metadata from an Android process creation log.
#[derive(Debug, PartialEq, Eq)]
pub struct ProcessStart<'a> {
    pub package: &'a str,
    pub target: &'a str,
    pub pid: &'a str,
    pub uid: &'a str,
    pub gids: &'a str,
}

/// Detects and parses process launch messages from ActivityManager.
pub fn parse_start_proc(line: &str) -> Option<ProcessStart<'_>> {
    // Android 5.1+ format
    if let Some(caps) = PID_START_5_1.captures(line) {
        return Some(ProcessStart {
            pid: caps.get(1)?.as_str(),
            package: caps.get(2)?.as_str(),
            target: caps.get(3)?.as_str(),
            uid: "",
            gids: "",
        });
    }

    // Android 4.x / 5.0 legacy format
    if let Some(caps) = PID_START.captures(line) {
        return Some(ProcessStart {
            package: caps.get(1)?.as_str(),
            target: caps.get(2)?.as_str(),
            pid: caps.get(3)?.as_str(),
            uid: caps.get(4)?.as_str(),
            gids: caps.get(5)?.as_str(),
        });
    }

    // Dalvik VM launch format
    if let Some(caps) = PID_START_DALVIK.captures(line) {
        return Some(ProcessStart {
            pid: caps.get(1)?.as_str(),
            package: caps.get(2)?.as_str(),
            target: "",
            uid: caps.get(3)?.as_str(),
            gids: "",
        });
    }

    None
}

/// Parsed metadata from an Android process death log.
#[derive(Debug, PartialEq, Eq)]
pub struct ProcessDeath<'a> {
    pub pid: &'a str,
    pub package: &'a str,
}

/// Detects and parses process termination messages from ActivityManager.
pub fn parse_death<'a>(tag: &str, message: &'a str) -> Option<ProcessDeath<'a>> {
    if tag != "ActivityManager" {
        return None;
    }

    // Check "Killing <pid>:<pkg>..."
    if let Some(caps) = PID_KILL.captures(message) {
        return Some(ProcessDeath {
            pid: caps.get(1)?.as_str(),
            package: caps.get(2)?.as_str(),
        });
    }

    // Check "No longer want <pkg> (pid <pid>)..."
    if let Some(caps) = PID_LEAVE.captures(message) {
        return Some(ProcessDeath {
            package: caps.get(1)?.as_str(),
            pid: caps.get(2)?.as_str(),
        });
    }

    // Check modern "Process <pkg> (pid <pid>) has died..."
    if let Some(caps) = PID_DEATH.captures(message) {
        return Some(ProcessDeath {
            package: caps.get(1)?.as_str(),
            pid: caps.get(2)?.as_str(),
        });
    }

    None
}

/// Applies syntax highlighting rules to message bodies (e.g. StrictMode violations and GC stats).
pub fn apply_rules(message: &str, color_gc: bool, use_color: bool) -> String {
    let mut result = message.to_string();

    if use_color {
        // Highlight StrictMode violations in red/yellow
        if let Some(caps) = STRICT_MODE_RULE.captures(&result) {
            let p1 = &caps[1];
            let p2 = &caps[2];
            let p3 = &caps[3];
            let rest = &result[caps[0].len()..];
            let replacement = format!(
                "{}{}{}{}{}{}{}",
                termcolor(Some(Color::Red), None),
                p1,
                RESET,
                p2,
                termcolor(Some(Color::Yellow), None),
                p3,
                RESET
            );
            result = format!("{}{}", replacement, rest);
        }

        // Highlight GC pauses in green and yellow
        if color_gc {
            if let Some(caps) = GC_RULE.captures(&result) {
                let p1 = &caps[1];
                let p2 = &caps[2];
                let p3 = &caps[3];
                let p4 = &caps[4];
                let rest = &result[caps[0].len()..];
                let replacement = format!(
                    "{}{}{}{}{}{}{}{}",
                    p1,
                    termcolor(Some(Color::Green), None),
                    p2,
                    RESET,
                    p3,
                    termcolor(Some(Color::Yellow), None),
                    p4,
                    RESET
                );
                result = format!("{}{}", replacement, rest);
            }
        }
    }

    result
}

/// Parses a line of `ps` or `ps -A` output to extract `(pid, process_name)`.
pub fn parse_ps_line(line: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 8 {
        return None;
    }
    // Check if parts[1] is a PID (number)
    if parts[1].parse::<u32>().is_ok() {
        let pid = parts[1].to_string();
        let name = parts[parts.len() - 1].to_string();
        return Some((pid, name));
    }
    None
}

/// Extracts the active foreground application package name from `dumpsys activity activities`.
pub fn parse_current_app(dumpsys_output: &str) -> Option<String> {
    // 1. Legacy TaskRecord pattern: .*TaskRecord.*A[= ]([^ ^}]*)
    if let Some(caps) = DUMPSYS_TASK_RECORD.captures(dumpsys_output) {
        if let Some(m) = caps.get(1) {
            return Some(m.as_str().to_string());
        }
    }

    // 2. Modern ActivityRecord pattern (Android 10+)
    if let Some(caps) = DUMPSYS_ACTIVITY_RECORD.captures(dumpsys_output) {
        if let Some(m) = caps.get(1) {
            return Some(m.as_str().to_string());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_log_line() {
        let line = "D/dalvikvm( 1234): GC_CONCURRENT freed 2048K";
        let parsed = parse_log_line(line).unwrap();
        assert_eq!(parsed.timestamp, None);
        assert_eq!(parsed.level, 'D');
        assert_eq!(parsed.tag, "dalvikvm");
        assert_eq!(parsed.pid, "1234");
        assert_eq!(parsed.message, "GC_CONCURRENT freed 2048K");
    }

    #[test]
    fn test_parse_time_log_line() {
        let line = "10-11 17:14:17.248 W/linker  (  232): Warning: linker warning";
        let parsed = parse_log_line(line).unwrap();
        assert_eq!(parsed.timestamp, Some("10-11 17:14:17.248"));
        assert_eq!(parsed.level, 'W');
        assert_eq!(parsed.tag, "linker");
        assert_eq!(parsed.pid, "232");
        assert_eq!(parsed.message, "Warning: linker warning");
    }

    #[test]
    fn test_parse_threadtime_log_line() {
        let line = "10-11 12:00:00.000  1234  5678 D MyTag   : hello threadtime";
        let parsed = parse_log_line(line).unwrap();
        assert_eq!(parsed.timestamp, Some("10-11 12:00:00.000"));
        assert_eq!(parsed.level, 'D');
        assert_eq!(parsed.tag, "MyTag");
        assert_eq!(parsed.pid, "1234");
        assert_eq!(parsed.message, "hello threadtime");
    }

    #[test]
    fn test_parse_start_proc() {
        let line = "I/ActivityManager( 567): Start proc 1234:com.example.app/u0a123 for activity com.example.app/.MainActivity";
        let start = parse_start_proc(line).unwrap();
        assert_eq!(start.pid, "1234");
        assert_eq!(start.package, "com.example.app");
        assert_eq!(start.target, "activity com.example.app/.MainActivity");

        let line2 = "I/ActivityManager( 567): Start proc com.example.app for activity com.example.app/.MainActivity: pid=1234 uid=10000 gids=3003";
        let start2 = parse_start_proc(line2).unwrap();
        assert_eq!(start2.pid, "1234");
        assert_eq!(start2.package, "com.example.app");
        assert_eq!(start2.uid, "10000");
    }

    #[test]
    fn test_parse_death() {
        let tag = "ActivityManager";
        let msg = "Killing 1234:com.example.app/u0a123: stop";
        let death = parse_death(tag, msg).unwrap();
        assert_eq!(death.pid, "1234");
        assert_eq!(death.package, "com.example.app");

        let msg2 = "Process com.example.app (pid 1234) has died.";
        let death2 = parse_death(tag, msg2).unwrap();
        assert_eq!(death2.pid, "1234");
        assert_eq!(death2.package, "com.example.app");

        let msg3 = "Process com.example.app (pid 1234) has died: fg  TOP";
        let death3 = parse_death(tag, msg3).unwrap();
        assert_eq!(death3.pid, "1234");
        assert_eq!(death3.package, "com.example.app");
    }

    #[test]
    fn test_parse_ps() {
        let line = "u0_a123  12345 567   123456 45678 sys_epoll_wait  00 S com.example.app";
        let (pid, name) = parse_ps_line(line).unwrap();
        assert_eq!(pid, "12345");
        assert_eq!(name, "com.example.app");
    }

    #[test]
    fn test_apply_rules() {
        let strict = "StrictMode policy violation; ~duration=319 ms: disk write";
        let replaced = apply_rules(strict, false, true);
        assert!(replaced.contains("\x1b[31mStrictMode policy violation\x1b[0m"));
        assert!(replaced.contains("\x1b[33m319 ms\x1b[0m"));

        let gc = "GC_CONCURRENT freed 2048K, 20% free 4096K/8192K, paused 2ms+3ms, total 15ms";
        let replaced_gc = apply_rules(gc, true, true);
        assert!(replaced_gc.contains("\x1b[32mfreed 2048K\x1b[0m"));
        assert!(replaced_gc.contains("\x1b[33mpaused 2ms+3ms\x1b[0m"));
    }

    #[test]
    fn test_parse_current_app() {
        let legacy_dumpsys = "  * TaskRecord{abc1234 #1 A=com.example.app U=0 StackId=1 sz=1}";
        assert_eq!(
            parse_current_app(legacy_dumpsys),
            Some("com.example.app".to_string())
        );

        let modern_dumpsys =
            "  mResumedActivity: ActivityRecord{123 u0 com.example.app/.MainActivity t12}";
        assert_eq!(
            parse_current_app(modern_dumpsys),
            Some("com.example.app".to_string())
        );
    }

    #[test]
    fn test_backtrace_regex() {
        let bt = "#00 pc 00012345 /system/lib/libc.so";
        assert!(BACKTRACE_LINE.is_match(bt));
    }
}
