//! Package, tag, and log-level filtering logic.
//!
//! Handles:
//! - Package matching, including named child processes (e.g. `com.example:sync`).
//! - Log tag whitelisting (`-t`) and blacklisting (`-i`) using anchored regexes.
//! - Priority thresholds for Android log levels (`V < D < I < W < E < F`).

use clap::ValueEnum;
use regex::Regex;
use std::collections::HashSet;

/// Standard Android log level priorities.
///
/// Implements `Ord` so log levels can be directly compared with `<` and `>=`.
/// For instance, `LogLevel::Warn >= LogLevel::Info`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
pub enum LogLevel {
    /// Verbose (lowest priority, shows everything)
    #[value(name = "V", alias = "v", alias = "verbose", alias = "VERBOSE")]
    Verbose = 0,
    /// Debug
    #[value(name = "D", alias = "d", alias = "debug", alias = "DEBUG")]
    Debug = 1,
    /// Info
    #[value(name = "I", alias = "i", alias = "info", alias = "INFO")]
    Info = 2,
    /// Warning
    #[value(
        name = "W",
        alias = "w",
        alias = "warn",
        alias = "warning",
        alias = "WARN",
        alias = "WARNING"
    )]
    Warn = 3,
    /// Error
    #[value(name = "E", alias = "e", alias = "error", alias = "ERROR")]
    Error = 4,
    /// Fatal / Assert (highest priority)
    #[value(name = "F", alias = "f", alias = "fatal", alias = "FATAL")]
    Fatal = 5,
}

impl LogLevel {
    /// Maps a single character (e.g. 'D', 'i', 'w') to its corresponding [`LogLevel`].
    /// Returns `None` if the character does not match any recognized Android level badge.
    pub fn from_char(c: char) -> Option<Self> {
        match c.to_ascii_uppercase() {
            'V' => Some(LogLevel::Verbose),
            'D' => Some(LogLevel::Debug),
            'I' => Some(LogLevel::Info),
            'W' => Some(LogLevel::Warn),
            'E' => Some(LogLevel::Error),
            'F' => Some(LogLevel::Fatal),
            _ => None,
        }
    }
}

/// Matches Android process and package names according to pidcat conventions.
///
/// Supports three match categories:
/// 1. Catchall packages (e.g. `com.example.app` matches `com.example.app` and any child `com.example.app:worker`).
/// 2. Specific named processes (e.g. `com.example.app:remote` only matches that specific subprocess).
/// 3. Pass-through mode when no package arguments are provided (`all = true`).
pub struct PackageFilter {
    all: bool,
    catchall_packages: HashSet<String>,
    named_processes: HashSet<String>,
}

impl PackageFilter {
    /// Builds a filter matching the given packages / process names.
    ///
    /// If `packages` is empty, matches every process on the device (like pidcat's all mode).
    pub fn new(packages: &[String]) -> Self {
        if packages.is_empty() {
            return Self {
                all: true,
                catchall_packages: HashSet::new(),
                named_processes: HashSet::new(),
            };
        }

        let mut catchall_packages = HashSet::new();
        let mut named_processes = HashSet::new();

        for pkg in packages {
            // "com.example:" syntax explicitly targets child processes
            if let Some(stripped) = pkg.strip_suffix(':') {
                named_processes.insert(stripped.to_string());
            } else if pkg.contains(':') {
                // "com.example:proc" targets a named sub-process
                named_processes.insert(pkg.clone());
            } else {
                // "com.example" targets the main package and any child processes
                catchall_packages.insert(pkg.clone());
            }
        }

        Self {
            all: false,
            catchall_packages,
            named_processes,
        }
    }

    /// Evaluates whether a given package or process token is allowed by this filter.
    pub fn matches(&self, token: &str) -> bool {
        if self.all {
            return true;
        }
        // Exact match for named subprocess
        if self.named_processes.contains(token) {
            return true;
        }
        // Check if prefix before colon matches a catchall package
        if let Some(idx) = token.find(':') {
            self.catchall_packages.contains(&token[..idx])
        } else {
            self.catchall_packages.contains(token)
        }
    }
}

/// Compiles a user-supplied tag pattern so it must match the *whole* tag.
///
/// The non-capturing group matters: `^Foo|Bar$` would match any tag
/// starting with `Foo` or ending with `Bar`. Wrapping in `^(?:...)$` ensures
/// full-string anchoring.
fn compile_tag_pattern(pattern: &str) -> Result<Regex, regex::Error> {
    Regex::new(&format!("^(?:{})$", pattern.trim()))
}

/// Dual whitelist and blacklist filter for Android log tags.
pub struct TagFilter {
    include_regexes: Vec<Regex>,
    ignore_regexes: Vec<Regex>,
}

impl TagFilter {
    /// Compiles inclusion (`-t`) and exclusion (`-i`) regex patterns.
    pub fn new(tags: &[String], ignored_tags: &[String]) -> Result<Self, regex::Error> {
        let include_regexes = tags
            .iter()
            .map(|t| compile_tag_pattern(t))
            .collect::<Result<_, _>>()?;
        let ignore_regexes = ignored_tags
            .iter()
            .map(|t| compile_tag_pattern(t))
            .collect::<Result<_, _>>()?;

        Ok(Self {
            include_regexes,
            ignore_regexes,
        })
    }

    /// Returns `true` if the given tag passes both blacklist and whitelist checks.
    ///
    /// Rules:
    /// - If any ignore regex matches, the tag is rejected.
    /// - If include regexes are specified, at least one must match; otherwise the tag is rejected.
    /// - If include regexes list is empty, any tag not explicitly ignored is allowed.
    pub fn allows(&self, tag: &str) -> bool {
        if !self.ignore_regexes.is_empty() && self.ignore_regexes.iter().any(|r| r.is_match(tag)) {
            return false;
        }
        if !self.include_regexes.is_empty() && !self.include_regexes.iter().any(|r| r.is_match(tag))
        {
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_package_filter() {
        let pkgs = vec![
            "com.example.app".to_string(),
            "com.example.other:service".to_string(),
        ];
        let filter = PackageFilter::new(&pkgs);
        assert!(filter.matches("com.example.app"));
        assert!(filter.matches("com.example.app:ui"));
        assert!(filter.matches("com.example.other:service"));
        assert!(!filter.matches("com.example.other"));
        assert!(!filter.matches("com.other.app"));
    }

    #[test]
    fn test_package_filter_empty_matches_everything() {
        let filter = PackageFilter::new(&[]);
        assert!(filter.matches("com.anything"));
        assert!(filter.matches("com.anything:remote"));
    }

    /// Regression (#3): `-a com.mine` must still restrict *package matching*
    /// (used for start/death banners) to `com.mine`; `-a` only affects which
    /// log lines are shown, which is decided outside the filter.
    #[test]
    fn test_package_filter_is_independent_of_all_flag() {
        let filter = PackageFilter::new(&["com.mine".to_string()]);
        assert!(filter.matches("com.mine"));
        assert!(!filter.matches("com.other"));
    }

    #[test]
    fn test_tag_filter() {
        let filter =
            TagFilter::new(&["MyTag.*".to_string()], &["MyTagIgnore".to_string()]).unwrap();
        assert!(filter.allows("MyTag123"));
        assert!(!filter.allows("MyTagIgnore"));
        assert!(!filter.allows("OtherTag"));
    }

    /// Regression (#2): alternations must be anchored as a whole.
    #[test]
    fn test_tag_filter_alternation_is_fully_anchored() {
        let filter = TagFilter::new(&["Foo|Bar".to_string()], &[]).unwrap();
        assert!(filter.allows("Foo"));
        assert!(filter.allows("Bar"));
        assert!(!filter.allows("FooXYZ"));
        assert!(!filter.allows("XBar"));

        let ignore = TagFilter::new(&[], &["Foo|Bar".to_string()]).unwrap();
        assert!(!ignore.allows("Foo"));
        assert!(ignore.allows("FooXYZ"));
        assert!(ignore.allows("XBar"));
    }
}
