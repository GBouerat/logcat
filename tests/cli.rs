use std::io::Write;
use std::process::{Command, Stdio};

fn run_binary_with_input(args: &[&str], input: &str) -> (String, String, bool) {
    let binary = env!("CARGO_BIN_EXE_logcat");
    let mut child = Command::new(binary)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn logcat binary");

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(input.as_bytes())
            .expect("Failed to write to child stdin");
    }

    let output = child.wait_with_output().expect("Failed to wait on child");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (stdout, stderr, output.status.success())
}

#[test]
fn test_cli_help_flag() {
    let (stdout, _, success) = run_binary_with_input(&["--help"], "");
    assert!(success);
    assert!(stdout.contains("Filter and highlight adb logcat output"));
    assert!(stdout.contains("--generate-completions"));
    assert!(stdout.contains("--show-pid"));
    assert!(stdout.contains("--timestamp"));
    assert!(stdout.contains("--json"));
}

#[test]
fn test_cli_version_flag() {
    let (stdout, _, success) = run_binary_with_input(&["--version"], "");
    assert!(success);
    assert!(stdout.contains("logcat 0.1.0"));
}

#[test]
fn test_cli_generate_completions_zsh() {
    let (stdout, _, success) = run_binary_with_input(&["--generate-completions", "zsh"], "");
    assert!(success);
    assert!(stdout.contains("#compdef logcat"));
    assert!(stdout.contains("logcat --complete-packages"));
}

#[test]
fn test_cli_generate_completions_bash() {
    let (stdout, _, success) = run_binary_with_input(&["--generate-completions", "bash"], "");
    assert!(success);
    assert!(stdout.contains("complete -F _logcat logcat"));
}

#[test]
fn test_cli_piped_log_filtering_by_package() {
    let stream = "\
I/ActivityManager( 500): Start proc 1234:com.example.app/u0a1 for act .Main\n\
D/MyTag( 1234): message from tracked app\n\
D/OtherTag( 9999): message from untracked app\n";

    let (stdout, _, success) = run_binary_with_input(&["com.example.app"], stream);
    assert!(success);
    assert!(stdout.contains("Process com.example.app created"));
    assert!(stdout.contains("message from tracked app"));
    assert!(!stdout.contains("message from untracked app"));
}

#[test]
fn test_cli_piped_json_mode() {
    let stream = "\
10-11 17:14:17.248 I/ActivityManager( 500): Start proc 1234:com.example.app/u0a1 for act .Main\n\
10-11 17:14:17.250 D/MyTag( 1234): hello json cli\n\
10-11 17:14:17.260 I/ActivityManager( 500): Process com.example.app (pid 1234) has died\n";

    let (stdout, _, success) = run_binary_with_input(&["--json", "com.example.app"], stream);
    assert!(success);

    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(lines.len(), 3);
    assert!(lines[0].contains(r#""type":"proc_start""#));
    assert!(lines[1].contains(r#""type":"log""#));
    assert!(lines[1].contains(r#""message":"hello json cli""#));
    assert!(lines[2].contains(r#""type":"proc_death""#));
}

#[test]
fn test_cli_piped_min_level_filter() {
    let stream = "\
D/MyTag( 1234): debug log\n\
W/MyTag( 1234): warning log\n\
E/MyTag( 1234): error log\n";

    let (stdout, _, success) = run_binary_with_input(&["-a", "-l", "W"], stream);
    assert!(success);
    assert!(!stdout.contains("debug log"));
    assert!(stdout.contains("warning log"));
    assert!(stdout.contains("error log"));
}

#[test]
fn test_cli_piped_timestamp_and_pid_formatting() {
    let stream = "10-11 17:14:17.248 I/MyTag( 1234): formatted entry\n";
    let (stdout, _, success) = run_binary_with_input(&["-a", "-T", "-p"], stream);
    assert!(success);
    assert!(stdout.starts_with("10-11 17:14:17.248"));
    assert!(stdout.contains(" 1234"));
    assert!(stdout.contains("MyTag"));
    assert!(stdout.contains("formatted entry"));
}
