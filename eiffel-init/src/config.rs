//! `/cmd` and `/env`: one entry per line, read once and unlinked.

use std::io::ErrorKind;

/// The Go supervisor is gone. A `cmd` that still names it would silently lose
/// whatever it was supervising, so it is rejected instead.
const GONE: &str = "/sbin/pid1";

/// Read a config file and unlink it. A missing file is an empty list.
pub fn read(path: &str) -> Result<String, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(String::new()),
        Err(e) => return Err(format!("read {path}: {e}")),
    };
    if let Err(e) = std::fs::remove_file(path) {
        log!("unlink {path}: {e}");
    }
    Ok(text)
}

/// One absolute program path per line, no arguments.
pub fn parse_cmd(text: &str) -> Result<Vec<String>, String> {
    let programs: Vec<String> = lines(text).iter().map(|l| (*l).to_string()).collect();
    if programs.iter().any(|p| p == GONE) {
        return Err("cmd: /sbin/pid1 is gone; list only your programs".to_string());
    }
    if programs.is_empty() {
        return Err("cmd: no programs listed".to_string());
    }
    Ok(programs)
}

/// One `KEY=VALUE` per line; the value may contain further `=`.
pub fn parse_env(text: &str) -> Result<Vec<(String, String)>, String> {
    lines(text)
        .iter()
        .map(|line| match line.split_once('=') {
            Some((key, value)) => Ok((key.to_string(), value.to_string())),
            None => Err(format!("env: line without '=': {line}")),
        })
        .collect()
}

/// Split on `\n`, dropping one trailing newline. Lines are kept verbatim: no
/// trimming, no comments, and an empty file yields nothing.
fn lines(text: &str) -> Vec<&str> {
    let body = text.strip_suffix('\n').unwrap_or(text);
    if body.is_empty() {
        Vec::new()
    } else {
        body.split('\n').collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trailing_newline_is_not_an_entry() {
        assert_eq!(
            parse_cmd("/app/one\n/app/two\n").unwrap(),
            ["/app/one", "/app/two"]
        );
        assert_eq!(parse_cmd("/app/one").unwrap(), ["/app/one"]);
    }

    #[test]
    fn empty_cmd_is_rejected() {
        assert!(parse_cmd("").is_err());
        assert!(parse_cmd("\n").is_err());
    }

    #[test]
    fn stale_pid1_line_is_rejected() {
        let err = parse_cmd("/sbin/pid1\n/app/example\n").unwrap_err();
        assert_eq!(err, "cmd: /sbin/pid1 is gone; list only your programs");
    }

    #[test]
    fn empty_env_file_is_an_empty_list() {
        assert!(parse_env("").unwrap().is_empty());
        assert!(parse_env("\n").unwrap().is_empty());
    }

    #[test]
    fn env_splits_at_the_first_equals() {
        let env = parse_env("PORT=5000\nURL=https://x/?a=b\n").unwrap();
        assert_eq!(env[0], ("PORT".to_string(), "5000".to_string()));
        assert_eq!(env[1], ("URL".to_string(), "https://x/?a=b".to_string()));
        assert_eq!(
            parse_env("EMPTY=\n").unwrap(),
            [("EMPTY".to_string(), String::new())]
        );
    }

    #[test]
    fn env_without_equals_is_rejected() {
        assert_eq!(
            parse_env("PORT\n").unwrap_err(),
            "env: line without '=': PORT"
        );
    }
}
