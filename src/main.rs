use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, Write};
use std::process::{Command, ExitCode, ExitStatus, Stdio};

const CONFLICT_EXIT_CODE: u8 = 1;
const TROUBLE_EXIT_CODE: u8 = 2;

#[derive(Clone)]
struct Oid(String);

impl Oid {
    fn parse(bytes: &[u8]) -> Option<Self> {
        let is_lowercase_hex = bytes
            .iter()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
        if !is_lowercase_hex || !matches!(bytes.len(), 40 | 64) {
            return None;
        }
        String::from_utf8(bytes.to_vec()).ok().map(Self)
    }
}

impl AsRef<OsStr> for Oid {
    fn as_ref(&self) -> &OsStr {
        self.0.as_ref()
    }
}

impl fmt::Display for Oid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

struct RepoPath(Vec<u8>);

impl fmt::Display for RepoPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for character in String::from_utf8_lossy(&self.0).chars() {
            let is_bidi_control = matches!(
                character,
                '\u{61c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
            );
            if character.is_control() || is_bidi_control {
                write!(formatter, "{}", character.escape_unicode())?;
            } else {
                write!(formatter, "{character}")?;
            }
        }
        Ok(())
    }
}

enum Outcome {
    Clean { tree: Oid },
    Conflict { commit: Oid, paths: Vec<RepoPath> },
}

impl Outcome {
    fn exit_code(&self) -> ExitCode {
        match self {
            Self::Clean { .. } => ExitCode::SUCCESS,
            Self::Conflict { .. } => ExitCode::from(CONFLICT_EXIT_CODE),
        }
    }
}

#[derive(Clone, Copy)]
enum Role {
    Target,
    Head,
    Commit { position: usize },
}

impl fmt::Display for Role {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Target => formatter.write_str("target"),
            Self::Head => formatter.write_str("HEAD"),
            Self::Commit { position } => write!(formatter, "commit {position}"),
        }
    }
}

enum Trouble {
    Usage,
    NotACommit {
        role: Role,
    },
    RootCommit {
        commit: Oid,
    },
    ShallowCut {
        commit: Oid,
    },
    MergeCommit {
        commit: Oid,
    },
    GitNotRun(io::Error),
    GitFailed {
        subcommand: &'static str,
        status: ExitStatus,
    },
    UnexpectedOutput {
        subcommand: &'static str,
    },
    StdoutFailed(io::Error),
}

impl From<io::Error> for Trouble {
    fn from(error: io::Error) -> Self {
        Self::GitNotRun(error)
    }
}

impl fmt::Display for Trouble {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage => write!(formatter, "usage: greb <target> [<commit>...]"),
            Self::NotACommit { role } => write!(formatter, "cannot resolve {role}"),
            Self::RootCommit { commit } => {
                write!(formatter, "root commit {commit} is not supported")
            }
            Self::ShallowCut { commit } => {
                write!(
                    formatter,
                    "parent of {commit} is missing from this shallow clone"
                )
            }
            Self::MergeCommit { commit } => {
                write!(formatter, "merge commit {commit} is not supported")
            }
            Self::GitNotRun(error) => write!(formatter, "cannot run git: {error}"),
            Self::GitFailed { subcommand, status } => {
                write!(formatter, "git {subcommand} failed with {status}")
            }
            Self::UnexpectedOutput { subcommand } => {
                write!(formatter, "git {subcommand} printed something unexpected")
            }
            Self::StdoutFailed(error) => write!(formatter, "cannot write to stdout: {error}"),
        }
    }
}

fn git() -> Command {
    let mut command = Command::new("git");
    for (name, _) in env::vars_os() {
        let prefix = name.as_encoded_bytes().get(..4);
        if prefix.is_some_and(|bytes| bytes.eq_ignore_ascii_case(b"GIT_")) {
            command.env_remove(name);
        }
    }
    command.arg("--no-lazy-fetch").stdin(Stdio::null());
    command
}

fn git_stdout(command: &mut Command, subcommand: &'static str) -> Result<Vec<u8>, Trouble> {
    let output = command.output()?;
    if !output.status.success() {
        return Err(Trouble::GitFailed {
            subcommand,
            status: output.status,
        });
    }
    Ok(output.stdout)
}

fn resolve(revision: &OsStr, peel: &str, role: Role) -> Result<Oid, Trouble> {
    let mut peeled_revision = revision.to_owned();
    peeled_revision.push(peel);
    let output = git()
        .args(["rev-parse", "--verify", "--end-of-options"])
        .arg(peeled_revision)
        .arg("--")
        .output()?;
    if !output.status.success() {
        return Err(Trouble::NotACommit { role });
    }
    Oid::parse(output.stdout.trim_ascii()).ok_or(Trouble::NotACommit { role })
}

fn recorded_parents(commit: &Oid) -> Result<usize, Trouble> {
    const SUBCOMMAND: &str = "cat-file";
    let stdout = git_stdout(git().args([SUBCOMMAND, "commit"]).arg(commit), SUBCOMMAND)?;
    let header = stdout
        .split(|byte| *byte == b'\n')
        .take_while(|line| !line.is_empty());
    Ok(header.filter(|line| line.starts_with(b"parent ")).count())
}

fn single_parent(commit: &Oid) -> Result<Oid, Trouble> {
    const SUBCOMMAND: &str = "rev-list";
    let stdout = git_stdout(
        git()
            .args([SUBCOMMAND, "--parents", "--max-count=1", "--end-of-options"])
            .arg(commit)
            .arg("--"),
        SUBCOMMAND,
    )?;
    let mut parents = stdout.trim_ascii().split(u8::is_ascii_whitespace).skip(1);
    let Some(parent) = parents.next() else {
        let commit = commit.clone();
        return Err(match recorded_parents(&commit)? {
            0 => Trouble::RootCommit { commit },
            1 => Trouble::ShallowCut { commit },
            _ => Trouble::MergeCommit { commit },
        });
    };
    if parents.next().is_some() {
        let commit = commit.clone();
        return Err(Trouble::MergeCommit { commit });
    }
    Oid::parse(parent).ok_or(Trouble::UnexpectedOutput {
        subcommand: SUBCOMMAND,
    })
}

fn rebased_commits(target: &Oid, head: &Oid) -> Result<Vec<Oid>, Trouble> {
    const SUBCOMMAND: &str = "rev-list";
    let mut range = OsString::from(target);
    range.push("...");
    range.push(head);
    let stdout = git_stdout(
        git()
            .args([
                SUBCOMMAND,
                "--reverse",
                "--topo-order",
                "--no-merges",
                "--right-only",
                "--cherry-pick",
                "--end-of-options",
            ])
            .arg(range)
            .arg("--"),
        SUBCOMMAND,
    )?;
    stdout
        .split(u8::is_ascii_whitespace)
        .filter(|line| !line.is_empty())
        .map(|line| {
            Oid::parse(line).ok_or(Trouble::UnexpectedOutput {
                subcommand: SUBCOMMAND,
            })
        })
        .collect()
}

fn replay(onto: &Oid, commit: Oid, parent: &Oid) -> Result<Outcome, Trouble> {
    const SUBCOMMAND: &str = "merge-tree";
    let mut attribute_source = OsString::from("--attr-source=");
    attribute_source.push(onto);
    let mut merge_base = OsString::from("--merge-base=");
    merge_base.push(parent);
    let output = git()
        .arg(attribute_source)
        .args([SUBCOMMAND, "--write-tree", "-z", "--name-only"])
        .arg(merge_base)
        .arg(onto)
        .arg(&commit)
        .output()?;
    let is_conflict = match output.status.code() {
        Some(0) => false,
        Some(1) => true,
        _ => {
            return Err(Trouble::GitFailed {
                subcommand: SUBCOMMAND,
                status: output.status,
            });
        }
    };
    let mut fields = output.stdout.split(|byte| *byte == 0);
    let tree = fields
        .next()
        .and_then(Oid::parse)
        .ok_or(Trouble::UnexpectedOutput {
            subcommand: SUBCOMMAND,
        })?;
    Ok(if is_conflict {
        let paths = fields
            .take_while(|field| !field.is_empty())
            .map(|field| RepoPath(field.to_vec()))
            .collect();
        Outcome::Conflict { commit, paths }
    } else {
        Outcome::Clean { tree }
    })
}

fn simulate() -> Result<Outcome, Trouble> {
    let arguments: Vec<OsString> = env::args_os().skip(1).collect();
    let [target, revisions @ ..] = arguments.as_slice() else {
        return Err(Trouble::Usage);
    };
    let target = resolve(target, "^{commit}", Role::Target)?;
    let commits = if revisions.is_empty() {
        let head = resolve(OsStr::new("HEAD"), "^{commit}", Role::Head)?;
        rebased_commits(&target, &head)?
    } else {
        revisions
            .iter()
            .enumerate()
            .map(|(index, revision)| {
                let position = index + 1;
                resolve(revision, "^{commit}", Role::Commit { position })
            })
            .collect::<Result<Vec<_>, Trouble>>()?
    };
    let steps = commits
        .into_iter()
        .map(|commit| {
            let parent = single_parent(&commit)?;
            Ok((commit, parent))
        })
        .collect::<Result<Vec<_>, Trouble>>()?;
    let mut onto = resolve(target.as_ref(), "^{tree}", Role::Target)?;
    for (commit, parent) in steps {
        match replay(&onto, commit, &parent)? {
            Outcome::Clean { tree } => onto = tree,
            conflict @ Outcome::Conflict { .. } => return Ok(conflict),
        }
    }
    Ok(Outcome::Clean { tree: onto })
}

fn print_outcome(outcome: &Outcome) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    match outcome {
        Outcome::Clean { tree } => writeln!(stdout, "clean {tree}"),
        Outcome::Conflict { commit, paths } => {
            writeln!(stdout, "conflict {commit}")?;
            for path in paths {
                writeln!(stdout, "{path}")?;
            }
            Ok(())
        }
    }
}

fn report_trouble(trouble: &Trouble) -> ExitCode {
    #[expect(
        let_underscore_drop,
        clippy::let_underscore_must_use,
        reason = "with stderr closed the exit code is the only report left"
    )]
    let _ = writeln!(io::stderr(), "greb: {trouble}");
    ExitCode::from(TROUBLE_EXIT_CODE)
}

fn main() -> ExitCode {
    let outcome = match simulate() {
        Ok(outcome) => outcome,
        Err(trouble) => return report_trouble(&trouble),
    };
    match print_outcome(&outcome) {
        Err(error) if error.kind() != io::ErrorKind::BrokenPipe => {
            report_trouble(&Trouble::StdoutFailed(error))
        }
        _ => outcome.exit_code(),
    }
}

#[cfg(test)]
mod tests {
    use super::Oid;

    #[test]
    fn oid_is_bare_lowercase_hex_of_a_hash_length() {
        for valid in ["a".repeat(40), "a".repeat(64)] {
            assert!(Oid::parse(valid.as_bytes()).is_some(), "{valid}");
        }
        for invalid in [
            String::new(),
            "a".repeat(39),
            "a".repeat(41),
            format!("^{}", "a".repeat(40)),
            "A".repeat(40),
            "g".repeat(40),
        ] {
            assert!(Oid::parse(invalid.as_bytes()).is_none(), "{invalid}");
        }
    }
}
