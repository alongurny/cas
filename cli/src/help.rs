//! Every command and option of `cas`, once. The help texts, the manual
//! page, the command reference in docs/ and the shell completions are all
//! printed from this table, so they cannot disagree.

use crate::Res;

/// What a command's last words are, for completion.
#[derive(Clone, Copy, PartialEq)]
pub enum Words {
    /// Nothing the shell can guess.
    None,
    /// A file or directory.
    Path,
}

pub struct Cmd {
    pub name: &'static str,
    /// Its group in the overview.
    pub group: &'static str,
    /// Each way to write it, without the leading `cas`.
    pub forms: &'static [&'static str],
    /// One line.
    pub about: &'static str,
    /// The rest, as paragraphs; may be empty.
    pub more: &'static str,
    /// Its second words, when it has a fixed set.
    pub subs: &'static [&'static str],
    /// The options it reads, besides the general ones.
    pub opts: &'static [&'static str],
    pub words: Words,
}

pub struct Opt {
    pub flag: &'static str,
    /// The placeholder of its value; empty for a switch.
    pub value: &'static str,
    pub about: &'static str,
    /// Read by every command.
    pub general: bool,
    /// The values it takes, when they are a fixed set.
    pub values: &'static [&'static str],
}

const fn opt(flag: &'static str, value: &'static str, about: &'static str) -> Opt {
    Opt { flag, value, about, general: false, values: &[] }
}

const fn general(flag: &'static str, value: &'static str, about: &'static str) -> Opt {
    Opt { flag, value, about, general: true, values: &[] }
}

pub const OPTIONS: &[Opt] = &[
    general("--store", "<dir>|<url>", "the store: a directory (default $CAS_STORE, else .cas) or an https URL"),
    general("--token-file", "<file>", "the file holding the token for a store under access control"),
    general("--ca", "<file>", "trust the CA certificates in <file> instead of the public ones"),
    general("--meta", "<dir>", "the store holding the access policy (default .cas-meta)"),
    general("-j", "<threads>", "the most threads a command uses (default: one for each core)"),
    general("-H", "<history>", "the history to work on (default main)"),
    general("-n", "<commit>", "a commit: latest, a key or its start; a tag on a release history (default latest)"),
    general("--help", "", "explain the command instead of running it (also -h)"),
    general("--version", "", "print the version of cas"),
    opt("--author", "<name>", "who makes the commit (default $CAS_AUTHOR, else $USER)"),
    opt("--notes", "<file>", "notes for the step: what `status <dir> --diff` prints, with `# note` lines added"),
    opt("--annotate", "<path>=<file>", "notes on the file <path> as it is: its lines as `| line`, with `# note` lines"),
    opt("--diff", "", "print the line diff, ready to add notes to"),
    opt("--annotated", "", "print the line diff with the notes of the step"),
    opt("--as", "<name>", "the name of the history on the other side, or of the release history"),
    opt("--tag", "<version>", "the semantic version of the release, such as 1.4.0"),
    Opt {
        flag: "--change",
        value: "<kind>",
        about: "what the release changes since the one before: breaking, feature, patch or security",
        general: false,
        values: &["breaking", "feature", "patch", "security"],
    },
    opt("--changelog", "<file>", "the text that says what the release changes"),
    opt("--all", "", "every commit the commit reaches, not only the commit itself"),
    opt("--quiet", "", "print the outcome only, not a line for each proof"),
    opt("--from", "<store>", "fetch the proofs that are not here from this store"),
    opt("--full", "", "walk the whole store, not only what came since the last full run"),
    opt("--cert", "<file>", "the PEM certificate chain to serve with"),
    opt("--key", "<file>", "the PEM private key to serve with"),
    opt("--open", "", "serve to anyone, without an access policy"),
    opt("--prove", "", "prove each history of commits as it moves (needs cas-prove)"),
    opt("--max-body", "<bytes>", "the largest request body accepted (default 64 MiB)"),
    opt("--nodes", "", "start a history in the earlier format, which the forge still keeps"),
    opt("--compatible", "", "on a history in the earlier format, mark the step compatible"),
    opt("--json", "", "print the forge's answer as JSON"),
    opt("--project", "<owner>/<project>", "the project on the forge, when --store does not name it"),
];

const fn cmd(
    name: &'static str,
    group: &'static str,
    forms: &'static [&'static str],
    about: &'static str,
    more: &'static str,
) -> Cmd {
    Cmd { name, group, forms, about, more, subs: &[], opts: &[], words: Words::None }
}

const WORK: &str = "Work on a directory";
const READ: &str = "Read a history";
const JOIN: &str = "Join and share histories";
const STORE: &str = "The store";
const FORGE: &str = "On a forge";
const TOOL: &str = "The tool";

pub const COMMANDS: &[Cmd] = &[
    Cmd {
        opts: &["--author", "--notes", "--annotate", "--nodes"],
        words: Words::Path,
        ..cmd(
            "commit",
            WORK,
            &["commit <dir> [--author <name>] [--notes <file>] [--annotate <path>=<file>]..."],
            "add <dir> as a new commit after the tip",
            "Prints the key of the new commit. A first commit starts the history.\n\n\
             There is no commit message. A commit says why through notes: --notes takes the diff \
             that `status <dir> --diff` prints with `# note` lines added, on the whole step, on a \
             path or on a line; --annotate takes one file as it is, each line as `| line`, with \
             `# note` lines before the lines they are about.\n\n\
             <dir>/.cas-index caches file sizes and times, so files that did not change are not \
             read again.",
        )
    },
    Cmd {
        opts: &["--diff"],
        words: Words::Path,
        ..cmd(
            "status",
            WORK,
            &["status <dir>", "status <dir> --diff"],
            "how <dir> differs from the commit",
            "Each changed path is printed as added, deleted or changed. Nothing is stored.\n\n\
             With --diff, prints the line diff from the commit to <dir>, which is what \
             `commit --notes` takes after notes are added. This stores the blobs of <dir>'s files.",
        )
    },
    Cmd {
        words: Words::Path,
        ..cmd("reset", WORK, &["reset <dir>"], "make <dir> the commit's version again", "Changes in <dir> are lost.")
    },
    Cmd {
        words: Words::Path,
        ..cmd("clean", WORK, &["clean <dir>"], "remove from <dir> the paths the commit does not have", "")
    },
    Cmd {
        subs: &["pop"],
        opts: &["--author"],
        words: Words::Path,
        ..cmd(
            "stash",
            WORK,
            &["stash <dir>", "stash pop <dir>"],
            "set the changes in <dir> aside, or bring them back",
            "`stash <dir>` keeps <dir> as a commit in the history stash/<history> and resets <dir>. \
             `stash pop <dir>` puts the newest one back into <dir> and drops it; <dir> must have no \
             changes of its own.",
        )
    },
    Cmd {
        words: Words::Path,
        ..cmd(
            "watch",
            WORK,
            &["watch <dir>"],
            "follow the changes in <dir> until stopped",
            "While it runs, commit and status look only at what changed (inotify), which matters \
             in a large directory.",
        )
    },
    Cmd {
        words: Words::Path,
        ..cmd(
            "checkout",
            WORK,
            &["checkout <dest> [<prefix>]"],
            "write the commit's version, or the part under <prefix>, to <dest>",
            "",
        )
    },
    Cmd {
        words: Words::Path,
        ..cmd(
            "sparse",
            WORK,
            &["sparse <dest> <path>..."],
            "write only the named files and directories of the commit to <dest>",
            "A later commit of <dest> keeps everything that was not checked out.",
        )
    },
    cmd(
        "log",
        READ,
        &["log"],
        "the commits, newest first",
        "Each line is the commit, its parents in brackets, its time (UTC), its author and the \
         first line of its note. On a release history each line is a tag and its release, \
         followed by what it changes.",
    ),
    cmd("ls", READ, &["ls [<prefix>]"], "the paths of the commit's version", ""),
    Cmd {
        opts: &["--annotated"],
        ..cmd(
            "diff",
            READ,
            &["diff", "diff --annotated"],
            "what the commit changed",
            "Prints each changed path with its format before and after, from each parent. With \
             --annotated, prints the line diff with the notes of the step.",
        )
    },
    cmd(
        "notes",
        READ,
        &["notes [<path>]"],
        "the notes of the commit",
        "Without <path>, the noted paths. With <path>, its note and the file's lines with the \
         note on each, in the form `commit --annotate` takes.",
    ),
    cmd(
        "manifest",
        READ,
        &["manifest"],
        "what a proof of the commit covers",
        "Lists each blob the commit brings that is not a file: its type, its key and its length. \
         A proof for this list says that each of them is valid. The last line gives the totals \
         and the key of the manifest, which is what a proof is named by.",
    ),
    Cmd {
        opts: &["--all", "--quiet"],
        ..cmd(
            "prove",
            READ,
            &["prove [--all] [--quiet]"],
            "make a proof that what the commit brings is valid",
            "The proof covers the manifest of the commit (`cas manifest`) and is kept in the store \
             beside it. With --all, every commit the commit reaches that has no proof yet gets \
             one, oldest first. `serve --prove` runs this whenever a history moves.\n\n\
             Proving is done by the program cas-prove, built from prove/ (docs/proofs.md).",
        )
    },
    Cmd {
        opts: &["--all", "--quiet", "--from"],
        ..cmd(
            "verify",
            READ,
            &["verify [--all] [--quiet] [--from <store>]"],
            "check the proof of the commit",
            "Checks the proof kept for the manifest of the commit, or with --all of every commit \
             it reaches. A proof is checked against the manifest alone: no listed blob is read. \
             With --from, a proof \
             that is not here is first fetched from that store and kept, so a client holds only \
             the proofs it asked for. Fails when a commit has no proof or a proof does not verify.",
        )
    },
    cmd("summary", READ, &["summary"], "how many directories and files the commit's version has", ""),
    cmd("path-log", READ, &["path-log <path>"], "the commits that changed <path>", ""),
    cmd(
        "last-modified",
        READ,
        &["last-modified [<prefix>]"],
        "each entry under <prefix> with the commit that last changed it",
        "",
    ),
    cmd("merge-base", READ, &["merge-base <a> <b>"], "the best common ancestor of commits <a> and <b>", ""),
    Cmd {
        opts: &["--author"],
        ..cmd(
            "merge",
            JOIN,
            &["merge <other>"],
            "merge history <other> into this one as one commit",
            "Paths are merged three ways, and the lines of a text file both sides changed. On a \
             conflict nothing is recorded and the conflicts are listed.",
        )
    },
    cmd("rebase", JOIN, &["rebase <onto>"], "replay this history's own commits onto history <onto>", ""),
    Cmd {
        opts: &["--as"],
        words: Words::Path,
        ..cmd(
            "push",
            JOIN,
            &["push <store> [--as <name>]"],
            "send the history to another store",
            "Only what the other store lacks is sent. The history there is created, or moved \
             forward when this one extends it; one that has diverged is refused.",
        )
    },
    Cmd {
        opts: &["--as"],
        words: Words::Path,
        ..cmd(
            "pull",
            JOIN,
            &["pull <store> [--as <name>]"],
            "fetch the history from another store",
            "The same rules as push, the other way.",
        )
    },
    Cmd {
        opts: &["--tag", "--change", "--changelog", "--as"],
        ..cmd(
            "publish",
            JOIN,
            &["publish --tag <version> [--change <kind> --changelog <file>] [--as <name>]"],
            "add the commit's version to the release history as a release",
            "The release history is `releases`, or --as <name>. A first release has no change. A \
             later one says its change from the release before it: its kind and a changelog.",
        )
    },
    cmd(
        "compatible",
        JOIN,
        &["-H releases compatible <from> <to>"],
        "whether upgrading from release <from> to <to> is safe",
        "Prints compatible or breaking, from the kinds of change between the two.",
    ),
    cmd(
        "tag",
        JOIN,
        &["tag <name>"],
        "name a node of a history in the earlier format",
        "A history of commits has no tags of its own: `publish --tag` names a release.",
    ),
    Cmd {
        words: Words::Path,
        ..cmd(
            "import-git",
            JOIN,
            &["import-git <repo> [<rev>]"],
            "import the commits of a git repository as a new history",
            "<rev> is HEAD unless given. Refused when the history exists. A commit's message \
             becomes the note of its step.",
        )
    },
    Cmd {
        opts: &["--full"],
        ..cmd(
            "gc",
            STORE,
            &["gc [--full]"],
            "remove blobs that no history reaches",
            "A blob is removed only after two runs in a row found it unreachable, so a writer has \
             the time of one run to link what it wrote.",
        )
    },
    cmd(
        "fsck",
        STORE,
        &["fsck"],
        "check every reachable blob: readable, valid and complete",
        "Fails when a blob is missing or not valid in its format, and names it.",
    ),
    cmd("objects", STORE, &["objects"], "every blob key in the store", ""),
    cmd("show", STORE, &["show <key>"], "print a blob", ""),
    Cmd {
        subs: &["make", "show"],
        words: Words::Path,
        ..cmd(
            "annotated",
            STORE,
            &["annotated make <file>", "annotated show <key>"],
            "store or print a text with a note on each line",
            "",
        )
    },
    Cmd {
        opts: &["--cert", "--key", "--open", "--max-body", "--prove"],
        words: Words::Path,
        ..cmd(
            "serve",
            STORE,
            &["serve <addr> --cert <file> --key <file> [--open] [--prove] [--max-body <bytes>]"],
            "serve the store over HTTPS",
            "HTTPS is the only protocol. Requests are decided by the access policy in --meta, \
             unless --open serves to anyone. With --prove, each history of commits is proven as it \
             moves, as `prove --all` does, so that those who fetch from it can `verify --from` it.",
        )
    },
    Cmd {
        subs: &["show", "user", "grant", "revoke"],
        ..cmd(
            "access",
            STORE,
            &[
                "access show",
                "access user <name>",
                "access grant <who> '<action> <scope>'",
                "access revoke <who> '<action> <scope>'",
            ],
            "the access policy of a served store",
            "The policy is in the store --meta names. `access user` adds a user or gives them a \
             new token, printed once. A grant is an action and a scope, such as 'read blobs' or \
             'write var:main'; <who> is a user or '*' for anyone.",
        )
    },
    cmd(
        "signup",
        FORGE,
        &["signup <name> [--email <e>]", "login <name>", "whoami", "password"],
        "make an account, sign in, or change the password",
        "Passwords are read from standard input. login writes the token to --token-file, or prints it.",
    ),
    cmd("login", FORGE, &[], "", ""),
    cmd("whoami", FORGE, &[], "", ""),
    cmd("password", FORGE, &[], "", ""),
    Cmd {
        subs: &["edit"],
        ..cmd(
            "account",
            FORGE,
            &["account <name>", "account edit [--display <d>] [--email <e>]"],
            "show or edit an account",
            "",
        )
    },
    Cmd {
        subs: &["list", "make", "revoke"],
        ..cmd(
            "token",
            FORGE,
            &[
                "token list",
                "token make <label> [--scope all|read|project:<owner>/<project>] [--days <n>]",
                "token revoke <key>",
            ],
            "the tokens of the account",
            "",
        )
    },
    Cmd {
        subs: &["create", "member"],
        ..cmd(
            "org",
            FORGE,
            &["org create <name>", "org member <org> <user> owner|member|remove"],
            "organizations and their members",
            "",
        )
    },
    cmd("projects", FORGE, &["projects"], "the projects you can read", ""),
    Cmd {
        subs: &["create", "show", "delete", "fork", "edit", "member", "protect", "gate", "label", "webhook", "runner"],
        ..cmd(
            "project",
            FORGE,
            &[
                "project create <name> [--owner <o>] [--description <d>] [--public]",
                "project show",
                "project delete --yes",
                "project fork [--owner <o>] [--name <n>]",
                "project edit [--description <d>] [--visibility public|private] [--readme <path>]",
                "project member <who> read|write|maintain|admin|remove",
                "project protect <history> on|off",
                "project gate <history> [--approvals <n>] [--checks <a,b>] [--resolved]",
                "project label <name> [--color <c>] [--set <s>] [--rule one|any]",
                "project label <name> remove",
                "project webhook list",
                "project webhook add <url> [--secret-file <f>] [--events <k,..>]",
                "project webhook remove <id>",
                "project runner list",
                "project runner <account> add|remove",
            ],
            "make and manage a project",
            "",
        )
    },
    cmd("histories", FORGE, &["histories"], "the project's histories and their tips", ""),
    cmd("blame", FORGE, &["blame <path>"], "each line of <path> with the commit that last changed it", ""),
    cmd("checks", FORGE, &["checks"], "the check results of a commit", ""),
    Cmd {
        subs: &[
            "list", "open", "show", "edit", "close", "reopen", "approve", "merge", "thread", "reply", "resolve",
            "promote",
        ],
        ..cmd(
            "proposal",
            FORGE,
            &[
                "proposal list [--state open|merged|closed|all]",
                "proposal open <title> --source <history> [--target <history>]",
                "proposal show <n>",
                "proposal edit <n> --title <t>",
                "proposal close|reopen|approve|merge <n>",
                "proposal thread <n> <path>:<line> [--old] <text>|- [--suggestion <text>]",
                "proposal reply <n> <thread> <text>|- [--suggestion <text>]",
                "proposal resolve|promote <n> <thread> [--undo]",
            ],
            "propose, review and merge a change",
            "A thread is a review thread on a line of the diff (--old: the line's old side). \
             promote makes a thread's note the merged step's note on its line.",
        )
    },
    Cmd {
        subs: &["list", "open", "show", "comment", "edit", "close", "reopen", "label"],
        ..cmd(
            "issue",
            FORGE,
            &[
                "issue list [--state open|closed|all] [--label <l>]",
                "issue open <title> [--body <text>|-] [--label <l>]...",
                "issue open <title> --at <path>:<line> [--suggestion <text>]",
                "issue show <n>",
                "issue comment <n> <text>|-",
                "issue edit <n> [--title <t>] [--body <text>|-] [--assign <a,b>]",
                "issue close|reopen <n>",
                "issue label <n> +<add> -<remove>...",
            ],
            "issues of a project",
            "",
        )
    },
    Cmd {
        subs: &["list", "publish", "archive"],
        ..cmd(
            "release",
            FORGE,
            &[
                "release list",
                "release publish <tag> --changelog <file> [--compatible]",
                "release archive <tag> <file.tar.gz>",
            ],
            "releases of a project",
            "",
        )
    },
    Cmd {
        subs: &["add", "pull"],
        ..cmd("mirror", FORGE, &["mirror add <name> <git url>", "mirror pull <name>"], "mirror a git repository", "")
    },
    cmd(
        "events",
        FORGE,
        &["events [--after <n>] [--follow]"],
        "the event log of a project",
        "--follow waits for new events.",
    ),
    cmd(
        "help",
        TOOL,
        &["help [<command>]", "<command> --help"],
        "explain cas, or one command",
        "`help --man` prints the manual page and `help --markdown` the command reference.",
    ),
    Cmd {
        subs: SHELLS,
        ..cmd(
            "completions",
            TOOL,
            &["completions fish|bash|zsh"],
            "print the completion script for a shell",
            "fish: cas completions fish > ~/.config/fish/completions/cas.fish\n\n\
             bash: cas completions bash > ~/.local/share/bash-completion/completions/cas\n\n\
             zsh: cas completions zsh > a file named _cas in a directory of $fpath",
        )
    },
    cmd("version", TOOL, &["version", "--version"], "print the version of cas", ""),
];

pub const SHELLS: &[&str] = &["fish", "bash", "zsh"];

/// The groups, in the order the overview prints them.
const GROUPS: &[&str] = &[WORK, READ, JOIN, STORE, FORGE, TOOL];

pub fn version() -> String {
    format!("cas {}", env!("CARGO_PKG_VERSION"))
}

pub fn find(name: &str) -> Option<&'static Cmd> {
    let c = COMMANDS.iter().find(|c| c.name == name)?;
    // A command without forms of its own is explained with the one before it.
    if c.forms.is_empty() {
        let i = COMMANDS.iter().position(|x| x.name == name)?;
        return COMMANDS[..i].iter().rev().find(|x| !x.forms.is_empty());
    }
    Some(c)
}

fn option(flag: &str) -> &'static Opt {
    OPTIONS.iter().find(|o| o.flag == flag).expect("a listed option")
}

fn option_line(o: &Opt) -> String {
    let head = if o.value.is_empty() { o.flag.to_string() } else { format!("{} {}", o.flag, o.value) };
    // The text hangs under itself when it is longer than the line.
    let text = wrap(o.about, 30);
    format!("  {head:26}  {}", text.trim())
}

/// Text wrapped to 78 columns after `indent` spaces.
fn wrap(text: &str, indent: usize) -> String {
    let mut out = String::new();
    for para in text.split("\n\n") {
        if !out.is_empty() {
            out.push('\n');
        }
        let mut line = " ".repeat(indent);
        for word in para.split_whitespace() {
            if line.len() + word.len() + 1 > 78 && !line.trim().is_empty() {
                out.push_str(line.trim_end());
                out.push('\n');
                line = " ".repeat(indent);
            }
            line.push_str(word);
            line.push(' ');
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// The overview: `cas help`.
pub fn overview() -> String {
    let mut out = String::from(
        "cas keeps directories in a content-addressed store: every version, commit and note\n\
         is a blob named by its hash, shown as the name of the hash and its digits\n\
         (blake3:1a2b...).\n\n\
         usage: cas [<option>...] <command> [<argument>...]\n",
    );
    for g in GROUPS {
        out.push_str(&format!("\n{g}\n"));
        for c in COMMANDS.iter().filter(|c| c.group == *g && !c.forms.is_empty()) {
            out.push_str(&format!("  {:15} {}\n", c.name, c.about));
        }
    }
    out.push_str("\nOptions of every command\n");
    for o in OPTIONS.iter().filter(|o| o.general) {
        out.push_str(&option_line(o));
        out.push('\n');
    }
    out.push_str("\n`cas help <command>` explains one command.\n");
    out
}

/// The forms of a command, one on each line: what an error shows.
pub fn usage(name: &str) -> String {
    match find(name) {
        Some(c) => {
            let lines: Vec<String> = c.forms.iter().map(|f| format!("cas {f}")).collect();
            format!("usage: {}\n`cas help {}` says more.", lines.join("\n       "), c.name)
        }
        None => "`cas help` lists the commands.".into(),
    }
}

/// The first word after a command, as its first form names it.
pub fn first_word(name: &str) -> &'static str {
    find(name).and_then(|c| c.forms.first()).and_then(|f| f.split(' ').nth(1)).unwrap_or("a word")
}

/// One command: `cas help <command>`.
pub fn command(name: &str) -> Res<String> {
    let c = find(name).ok_or(format!("no command {name}\n`cas help` lists the commands."))?;
    let lines: Vec<String> = c.forms.iter().map(|f| format!("cas {f}")).collect();
    let mut out = format!("usage: {}\n\n{}", lines.join("\n       "), wrap(&sentence(c.about), 0));
    if !c.more.is_empty() {
        out.push('\n');
        out.push_str(&wrap(c.more, 0));
    }
    if !c.opts.is_empty() {
        out.push_str("\nOptions\n");
        for f in c.opts {
            out.push_str(&option_line(option(f)));
            out.push('\n');
        }
    }
    out.push_str("\n`cas help` lists the options of every command.\n");
    Ok(out)
}

fn sentence(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => format!("{}{}.", f.to_uppercase(), c.as_str()),
        None => String::new(),
    }
}

/// The command reference as Markdown: docs/commands.md.
pub fn markdown() -> String {
    let mut out = String::from(
        "# cas commands\n\n\
         This page is printed by `cas help --markdown`; a test keeps it equal to the tool.\n\n\
         ```\ncas [<option>...] <command> [<argument>...]\n```\n\n## Options of every command\n\n",
    );
    for o in OPTIONS.iter().filter(|o| o.general) {
        out.push_str(&md_option(o));
    }
    for g in GROUPS {
        out.push_str(&format!("\n## {g}\n"));
        for c in COMMANDS.iter().filter(|c| c.group == *g && !c.forms.is_empty()) {
            out.push_str(&format!("\n### {}\n\n```\n", c.name));
            for f in c.forms {
                out.push_str(&format!("cas {f}\n"));
            }
            out.push_str(&format!("```\n\n{}\n", sentence(c.about)));
            if !c.more.is_empty() {
                out.push_str(&format!("\n{}\n", c.more));
            }
            if !c.opts.is_empty() {
                out.push('\n');
                for f in c.opts {
                    out.push_str(&md_option(option(f)));
                }
            }
        }
    }
    out
}

fn md_option(o: &Opt) -> String {
    let head = if o.value.is_empty() { o.flag.to_string() } else { format!("{} {}", o.flag, o.value) };
    format!("- `{head}`: {}\n", o.about)
}

/// The manual page, cas(1), in roff.
pub fn man() -> String {
    let esc = |s: &str| s.replace('\\', "\\e").replace('-', "\\-");
    let mut out = format!(
        ".TH CAS 1 \"\" \"{}\" \"cas\"\n.SH NAME\ncas \\- versions of directories in a content\\-addressed store\n\
         .SH SYNOPSIS\n.B cas\n[\\fIoption\\fR...] \\fIcommand\\fR [\\fIargument\\fR...]\n\
         .SH DESCRIPTION\nEvery version, commit and note is a blob named by its hash, shown as the name of \
         the hash and its digits (blake3:1a2b...). A history is a name that holds its newest commit.\n\
         .SH OPTIONS\n",
        version()
    );
    for o in OPTIONS.iter().filter(|o| o.general) {
        out.push_str(&format!(".TP\n\\fB{}\\fR {}\n{}\n", esc(o.flag), esc(o.value), esc(o.about)));
    }
    for g in GROUPS {
        out.push_str(&format!(".SH {}\n", g.to_uppercase()));
        for c in COMMANDS.iter().filter(|c| c.group == *g && !c.forms.is_empty()) {
            for (i, f) in c.forms.iter().enumerate() {
                out.push_str(if i == 0 { ".TP\n" } else { ".TQ\n" });
                out.push_str(&format!("\\fBcas\\fR {}\n", esc(f)));
            }
            out.push_str(&format!("{}\n", esc(&sentence(c.about))));
            for para in c.more.split("\n\n").filter(|p| !p.is_empty()) {
                out.push_str(&format!(".IP\n{}\n", esc(para)));
            }
            for f in c.opts {
                let o = option(f);
                out.push_str(&format!(".IP\n\\fB{}\\fR {}: {}\n", esc(o.flag), esc(o.value), esc(o.about)));
            }
        }
    }
    out.push_str(
        ".SH EXIT STATUS\n0 when the command did what it says, 1 when it could not, 2 when it was not \
         written correctly.\n.SH ENVIRONMENT\n.TP\nCAS_STORE\nThe store, when \\-\\-store is not given.\n\
         .TP\nCAS_AUTHOR\nThe author of a commit, when \\-\\-author is not given.\n",
    );
    out
}

/// Whether the option belongs after the command `c`.
fn takes(c: &Cmd, o: &Opt) -> bool {
    c.opts.contains(&o.flag)
}

/// The completion script for a shell.
pub fn completions(shell: &str) -> Res<String> {
    let listed: Vec<&Cmd> = COMMANDS.iter().collect();
    let names: Vec<&str> = listed.iter().map(|c| c.name).collect();
    let about = |c: &Cmd| find(c.name).map_or("", |c| c.about);
    match shell {
        "fish" => {
            let mut out = String::from(
                "# Completions of cas for fish, printed by `cas completions fish`.\n\
                 function __cas_command\n    set -l words (commandline -opc)\n    set -e words[1]\n    \
                 set -l skip 0\n    for w in $words\n        if test $skip -eq 1\n            set skip 0\n            \
                 continue\n        end\n        switch $w\n            case ",
            );
            let valued: Vec<&str> = OPTIONS.iter().filter(|o| !o.value.is_empty()).map(|o| o.flag).collect();
            out.push_str(&valued.join(" "));
            out.push_str(
                "\n                set skip 1\n            case '-*'\n            case '*'\n                \
                 echo $w\n                return 0\n        end\n    end\n    return 1\nend\n\
                 function __cas_no_command\n    not __cas_command >/dev/null\nend\n\
                 function __cas_using\n    test (__cas_command) = $argv[1] 2>/dev/null\nend\n\
                 function __cas_histories\n    command cas __histories 2>/dev/null\nend\n\
                 complete -c cas -f\n",
            );
            for c in &listed {
                out.push_str(&format!(
                    "complete -c cas -n __cas_no_command -a {} -d '{}'\n",
                    c.name,
                    about(c).replace('\'', "\\'")
                ));
            }
            for o in OPTIONS {
                let (kind, name) = match o.flag.strip_prefix("--") {
                    Some(long) => ("-l", long),
                    None => ("-s", &o.flag[1..]),
                };
                let cond = if o.general {
                    String::new()
                } else {
                    let users: Vec<&str> = listed.iter().filter(|c| takes(c, o)).map(|c| c.name).collect();
                    if users.is_empty() {
                        String::new()
                    } else {
                        format!(" -n 'contains -- (__cas_command) {}'", users.join(" "))
                    }
                };
                let arg = if o.value.is_empty() {
                    ""
                } else if !o.values.is_empty() {
                    " -x"
                } else if o.value.contains("file") || o.value.contains("dir") {
                    " -r -F"
                } else {
                    " -x"
                };
                let values = if o.values.is_empty() { String::new() } else { format!(" -a '{}'", o.values.join(" ")) };
                let values =
                    if o.flag == "-H" || o.flag == "--as" { " -a '(__cas_histories)'".to_string() } else { values };
                out.push_str(&format!(
                    "complete -c cas{cond} {kind} {name}{arg}{values} -d '{}'\n",
                    o.about.replace('\'', "\\'")
                ));
            }
            for c in &listed {
                if !c.subs.is_empty() {
                    out.push_str(&format!("complete -c cas -n '__cas_using {}' -a '{}'\n", c.name, c.subs.join(" ")));
                }
                if c.words == Words::Path {
                    out.push_str(&format!("complete -c cas -n '__cas_using {}' -F\n", c.name));
                }
            }
            out.push_str(&format!("complete -c cas -n '__cas_using help' -a '{}'\n", names.join(" ")));
            for c in ["merge", "rebase"] {
                out.push_str(&format!("complete -c cas -n '__cas_using {c}' -a '(__cas_histories)'\n"));
            }
            Ok(out)
        }
        "bash" => {
            let mut out = String::from(
                "# Completions of cas for bash, printed by `cas completions bash`.\n_cas() {\n    \
                 local cur=${COMP_WORDS[COMP_CWORD]} prev=${COMP_WORDS[COMP_CWORD-1]} cmd= i skip=0\n    \
                 for ((i = 1; i < COMP_CWORD; i++)); do\n        if ((skip)); then skip=0; continue; fi\n        \
                 case ${COMP_WORDS[i]} in\n            ",
            );
            let valued: Vec<&str> = OPTIONS.iter().filter(|o| !o.value.is_empty()).map(|o| o.flag).collect();
            out.push_str(&valued.join("|"));
            out.push_str(
                ") skip=1 ;;\n            -*) ;;\n            *) cmd=${COMP_WORDS[i]}; break ;;\n        esac\n    done\n    \
                 case $prev in\n",
            );
            for o in OPTIONS.iter().filter(|o| !o.value.is_empty()) {
                let action = if !o.values.is_empty() {
                    format!("COMPREPLY=($(compgen -W '{}' -- \"$cur\"))", o.values.join(" "))
                } else if o.flag == "-H" || o.flag == "--as" {
                    "COMPREPLY=($(compgen -W \"$(command cas __histories 2>/dev/null)\" -- \"$cur\"))".to_string()
                } else if o.value.contains("file") || o.value.contains("dir") {
                    "COMPREPLY=($(compgen -f -- \"$cur\"))".to_string()
                } else {
                    "COMPREPLY=()".to_string()
                };
                out.push_str(&format!("        {}) {action}; return ;;\n", o.flag));
            }
            out.push_str("    esac\n    if [[ $cur == -* ]]; then\n        local opts='");
            let generals: Vec<&str> = OPTIONS.iter().filter(|o| o.general).map(|o| o.flag).collect();
            out.push_str(&generals.join(" "));
            out.push_str("'\n        case $cmd in\n");
            for c in listed.iter().filter(|c| !c.opts.is_empty()) {
                out.push_str(&format!("            {}) opts+=' {}' ;;\n", c.name, c.opts.join(" ")));
            }
            out.push_str(
                "        esac\n        COMPREPLY=($(compgen -W \"$opts\" -- \"$cur\"))\n        return\n    fi\n    \
                 case $cmd in\n",
            );
            out.push_str(&format!("        '') COMPREPLY=($(compgen -W '{}' -- \"$cur\")) ;;\n", names.join(" ")));
            out.push_str(&format!("        help) COMPREPLY=($(compgen -W '{}' -- \"$cur\")) ;;\n", names.join(" ")));
            out.push_str(
                "        merge|rebase) COMPREPLY=($(compgen -W \"$(command cas __histories 2>/dev/null)\" -- \"$cur\")) ;;\n",
            );
            for c in listed.iter().filter(|c| !c.subs.is_empty() || c.words == Words::Path) {
                let subs = if c.subs.is_empty() {
                    String::new()
                } else {
                    format!("$(compgen -W '{}' -- \"$cur\") ", c.subs.join(" "))
                };
                let files = if c.words == Words::Path { "$(compgen -f -- \"$cur\")" } else { "" };
                out.push_str(&format!("        {}) COMPREPLY=({subs}{files}) ;;\n", c.name));
            }
            out.push_str("    esac\n}\ncomplete -o filenames -F _cas cas\n");
            Ok(out)
        }
        "zsh" => {
            let q = |s: &str| s.replace('\'', "'\\''").replace('[', "\\[").replace(']', "\\]").replace(':', "\\:");
            let mut out = String::from(
                "#compdef cas\n# Completions of cas for zsh, printed by `cas completions zsh`.\n\
                 _cas_histories() {\n    local -a names\n    names=(${(f)\"$(command cas __histories 2>/dev/null)\"})\n    \
                 compadd -a names\n}\n_cas_word_or_file() {\n    compadd -- \"$@\"\n    _files\n}\n_cas() {\n    local -a general commands\n    general=(\n",
            );
            let spec = |o: &Opt| {
                let arg = if o.value.is_empty() {
                    String::new()
                } else if !o.values.is_empty() {
                    format!(":{}:({})", q(o.value), o.values.join(" "))
                } else if o.flag == "-H" || o.flag == "--as" {
                    format!(":{}:_cas_histories", q(o.value))
                } else if o.value.contains("file") || o.value.contains("dir") {
                    format!(":{}:_files", q(o.value))
                } else {
                    format!(":{}:", q(o.value))
                };
                format!("'{}[{}]{arg}'", o.flag, q(o.about))
            };
            for o in OPTIONS.iter().filter(|o| o.general) {
                out.push_str(&format!("        {}\n", spec(o)));
            }
            out.push_str("    )\n    commands=(\n");
            for c in &listed {
                out.push_str(&format!("        '{}:{}'\n", c.name, q(about(c))));
            }
            out.push_str(
                "    )\n    local curcontext=$curcontext state line\n    \
                 _arguments -C $general '1: :->command' '*:: :->rest' && return\n    case $state in\n        \
                 command) _describe command commands ;;\n        rest)\n            case $line[1] in\n",
            );
            for c in &listed {
                let mut parts: Vec<String> = c.opts.iter().map(|f| spec(option(f))).collect();
                if !c.subs.is_empty() && c.words == Words::Path {
                    parts.push(format!("'1: :_cas_word_or_file {}'", c.subs.join(" ")));
                } else if !c.subs.is_empty() {
                    parts.push(format!("'1: :({})'", c.subs.join(" ")));
                }
                if c.name == "help" {
                    parts.push(format!("'1: :({})'", names.join(" ")));
                }
                if c.name == "merge" || c.name == "rebase" {
                    parts.push("'1: :_cas_histories'".into());
                }
                if c.words == Words::Path {
                    parts.push("'*: :_files'".into());
                }
                if parts.is_empty() {
                    continue;
                }
                out.push_str(&format!("                {}) _arguments $general {} ;;\n", c.name, parts.join(" ")));
            }
            out.push_str("            esac ;;\n    esac\n}\n_cas \"$@\"\n");
            Ok(out)
        }
        _ => Err(format!("no completions for {shell}: {}", SHELLS.join(", "))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every command the program runs is in the table, with options that exist.
    #[test]
    fn the_table_covers_the_commands() {
        let run = crate::forge::COMMANDS.iter().chain(crate::commits::COMMANDS.iter().copied().flatten());
        for name in run {
            assert!(find(name).is_some(), "{name} is not in the table");
        }
        for c in COMMANDS {
            for f in c.opts {
                assert!(OPTIONS.iter().any(|o| o.flag == *f), "{} names {f}", c.name);
            }
            assert!(c.forms.is_empty() || !c.about.is_empty(), "{}", c.name);
            command(c.name).unwrap();
        }
    }

    /// No line of the help is wider than a terminal, and nothing is in
    /// capitals but the names the environment gives.
    #[test]
    fn the_help_fits_a_terminal() {
        let mut texts = vec![overview()];
        texts.extend(COMMANDS.iter().map(|c| command(c.name).unwrap()));
        for t in texts {
            for l in t.lines() {
                assert!(l.chars().count() <= 100, "{l}");
                for w in l.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '$') {
                    let shout = w.len() > 1 && w.chars().all(|c| c.is_ascii_uppercase() || c == '_');
                    assert!(!shout || ["HTTPS", "PEM", "CA", "JSON", "UTC", "URL", "HEAD"].contains(&w), "{w} in {l}");
                }
            }
        }
    }
}
