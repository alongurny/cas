//! The help, the completions and the exit codes of the `cas` binary.

use std::path::Path;
use std::process::Command;

fn cas(dir: &Path, args: &[&str]) -> (i32, String, String) {
    let o =
        Command::new(env!("CARGO_BIN_EXE_cas")).current_dir(dir).env_remove("CAS_STORE").args(args).output().unwrap();
    (o.status.code().unwrap(), String::from_utf8(o.stdout).unwrap(), String::from_utf8(o.stderr).unwrap())
}

/// The files kept in the repository are what the tool prints. To renew
/// them: cas help --markdown > docs/commands.md, cas help --man >
/// docs/cas.1, cas completions <shell> > completions/<its file>.
#[test]
fn the_kept_files_are_what_the_tool_prints() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    for (args, file) in [
        (&["help", "--markdown"][..], "docs/commands.md"),
        (&["help", "--man"], "docs/cas.1"),
        (&["completions", "fish"], "completions/cas.fish"),
        (&["completions", "bash"], "completions/cas.bash"),
        (&["completions", "zsh"], "completions/_cas"),
    ] {
        let (code, out, _) = cas(root, args);
        assert_eq!(code, 0);
        assert_eq!(out, std::fs::read_to_string(root.join(file)).unwrap(), "{file} is stale");
    }
}

#[test]
fn help_and_version() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    let (code, all, _) = cas(d, &["help"]);
    assert_eq!(code, 0);
    assert!(all.contains("\n  commit ") && all.contains("\n  completions "));
    // The same text three ways.
    let (_, one, _) = cas(d, &["help", "stash"]);
    assert!(one.starts_with("usage: cas stash <dir>\n       cas stash pop <dir>\n"));
    assert_eq!(cas(d, &["stash", "--help"]).1, one);
    assert_eq!(cas(d, &["-h", "stash"]).1, one);
    assert_eq!(cas(d, &["--help"]).1, all);
    let version = format!("cas {}\n", env!("CARGO_PKG_VERSION"));
    assert_eq!(cas(d, &["--version"]), (0, version.clone(), String::new()));
    assert_eq!(cas(d, &["version"]).1, version);
    // None of these made a store.
    assert!(!d.join(".cas").exists());
}

/// 0 when the command did what it says, 1 when it could not, 2 when it
/// was not written correctly; an error is on standard error alone.
#[test]
fn exit_codes() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    let (code, out, err) = cas(d, &[]);
    assert_eq!((code, out.as_str()), (2, ""));
    assert!(err.contains("usage: cas [<option>...] <command>"));
    assert_eq!(cas(d, &["nope"]), (2, String::new(), "no command nope\n`cas help` lists the commands.\n".into()));
    assert_eq!(cas(d, &["--nope", "log"]).0, 2);
    // An option the command does not read, or a word past its last
    // argument, is refused before anything runs.
    for words in [&["log", "--nope"][..], &["log", "--author", "x"], &["ls", "a", "b"], &["checkout", "a", "b", "c"]] {
        let (code, _, err) = cas(d, words);
        assert_eq!(code, 2, "{words:?}");
        assert!(err.starts_with(&format!("{} takes no ", words[0])) && err.contains("usage: "), "{err}");
    }
    let (code, _, err) = cas(d, &["commit", "nope"]);
    assert_eq!((code, err.as_str()), (1, "nope is not a directory\n"));
    assert_eq!(cas(d, &["help", "nope"]).0, 2);
    assert_eq!(cas(d, &["completions", "tcsh"]).0, 1);
    let (code, _, err) = cas(d, &["commit"]);
    assert_eq!(code, 2);
    assert!(err.starts_with("missing <dir>\nusage: cas commit <dir> "), "{err}");
    // Reading where there is no store fails, and makes none.
    let (code, _, err) = cas(d, &["log"]);
    assert_eq!(code, 1);
    assert!(err.starts_with("no store at .cas"), "{err}");
    assert!(!d.join(".cas").exists());

    std::fs::create_dir(d.join("w")).unwrap();
    std::fs::write(d.join("w/a"), "a\n").unwrap();
    assert_eq!(cas(d, &["--author", "Dana", "commit", "w"]).0, 0);
    assert_eq!(cas(d, &["-n", "ffff", "ls"]), (1, String::new(), "no commit ffff in history main\n".into()));
    assert_eq!(cas(d, &["-H", "other", "log"]).0, 0);
    assert_eq!(cas(d, &["__histories"]).1, "main\n");
}

/// The completion scripts are sound in the shells that are installed here.
#[test]
fn the_completion_scripts_parse() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    for (shell, file) in
        [("bash", "completions/cas.bash"), ("fish", "completions/cas.fish"), ("zsh", "completions/_cas")]
    {
        match Command::new(shell).arg("-n").arg(root.join(file)).output() {
            Ok(o) => assert!(o.status.success(), "{shell}: {}", String::from_utf8_lossy(&o.stderr)),
            Err(_) => eprintln!("{shell} is not installed: {file} not checked"),
        }
    }
}
