//! Shell completion generators with dynamic package completion.

/// Generates a standalone Zsh completion script for `logcat`.
///
/// Configures `_arguments` with all options and binds positional package arguments
/// to a dynamic state handler invoking `logcat --complete-packages 2>/dev/null`.
pub fn generate_zsh() -> &'static str {
    r#"#compdef logcat

_logcat() {
    local context curcontext="$curcontext" state line
    typeset -A opt_args

    local -a packages
    _arguments -C \
        '(-w --tag-width)'{-w,--tag-width}'[Width of log tag]:tag_width: ' \
        '(-l --min-level)'{-l,--min-level}'[Minimum level to be displayed]:min_level:(verbose debug info warn error fatal V D I W E F v d i w e f)' \
        '--color-gc[Color garbage collection]' \
        '--always-display-tags[Always display the tag name]' \
        '--current[Filter logcat by current running app]' \
        '(-s --serial -d --device -e --emulator)'{-s,--serial}'[Device serial number (adb -s option)]:serial: ' \
        '(-s --serial -d --device -e --emulator)'{-d,--device}'[Use first device for log input (adb -d option)]' \
        '(-s --serial -d --device -e --emulator)'{-e,--emulator}'[Use first emulator for log input (adb -e option)]' \
        '(-c --clear)'{-c,--clear}'[Clear the entire log before running]' \
        '*'{-t,--tag}'[Filter output by specified tag(s)]:tag: ' \
        '*'{-i,--ignore-tag}'[Filter output by ignoring specified tag(s)]:ignored_tag: ' \
        '(-a --all)'{-a,--all}'[Print all log messages]' \
        '(-T --timestamp)'{-T,--timestamp}'[Display timestamps]' \
        '(-p --show-pid)'{-p,--show-pid}'[Display process ID]' \
        '--json[Output structured JSON lines]' \
        '--color[When to colorize output]:color:(auto always never)' \
        '--generate-completions[Generate shell completions (zsh, bash)]:shell:(zsh bash)' \
        '(-h --help)'{-h,--help}'[Print help]' \
        '(-V --version)'{-V,--version}'[Print version]' \
        '*:package:->packages' && return 0

    case "$state" in
        packages)
            packages=(${(f)"$(logcat --complete-packages 2>/dev/null)"})
            _describe -t packages 'installed package' packages
            ;;
    esac
}

_logcat "$@"
"#
}

/// Generates a standalone Bash completion script for `logcat`.
///
/// Configures `complete -F _logcat` with flag completions and dynamic
/// package completions queried via `logcat --complete-packages 2>/dev/null`.
pub fn generate_bash() -> &'static str {
    r#"_logcat() {
    local cur prev words cword
    _init_completion -n : 2>/dev/null || {
        cur="${COMP_WORDS[COMP_CWORD]}"
        prev="${COMP_WORDS[COMP_CWORD-1]}"
    }

    case "$prev" in
        -w|--tag-width|-s|--serial|-t|--tag|-i|--ignore-tag)
            return 0
            ;;
        -l|--min-level)
            COMPREPLY=($(compgen -W "verbose debug info warn error fatal V D I W E F v d i w e f" -- "$cur"))
            return 0
            ;;
        --color)
            COMPREPLY=($(compgen -W "auto always never" -- "$cur"))
            return 0
            ;;
        --generate-completions)
            COMPREPLY=($(compgen -W "zsh bash" -- "$cur"))
            return 0
            ;;
    esac

    if [[ "$cur" == -* ]]; then
        local opts="-w --tag-width -l --min-level --color-gc --always-display-tags --current -s --serial -d --device -e --emulator -c --clear -t --tag -i --ignore-tag -a --all -T --timestamp -p --show-pid --json --color --generate-completions -h --help -V --version"
        COMPREPLY=($(compgen -W "$opts" -- "$cur"))
        return 0
    fi

    # Dynamic package completion from adb
    local packages
    packages=$(logcat --complete-packages 2>/dev/null)
    if [[ -n "$packages" ]]; then
        COMPREPLY=($(compgen -W "$packages" -- "$cur"))
    fi
}

complete -F _logcat logcat
"#
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zsh_completion_contains_dynamic_packages() {
        let script = generate_zsh();
        assert!(script.contains("logcat --complete-packages 2>/dev/null"));
        assert!(script.contains("_arguments"));
        assert!(script.contains("--generate-completions"));
    }

    #[test]
    fn test_bash_completion_contains_dynamic_packages() {
        let script = generate_bash();
        assert!(script.contains("logcat --complete-packages 2>/dev/null"));
        assert!(script.contains("complete -F _logcat logcat"));
        assert!(script.contains("--generate-completions"));
    }
}
