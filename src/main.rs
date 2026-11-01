use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, Write};
use std::process::{Command, ExitCode, ExitStatus, Stdio};

const CONFLICT_EXIT_CODE: u8 = 1;
const TROUBLE_EXIT_CODE: u8 = 2;

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

enum Outcome {
    Clean { tree: Oid },
    Conflict,
}

impl Outcome {
    fn exit_code(&self) -> ExitCode {
        match self {
            Self::Clean { .. } => ExitCode::SUCCESS,
            Self::Conflict => ExitCode::from(CONFLICT_EXIT_CODE),
        }
    }
}

enum Trouble {
    Usage,
    NotACommit {
        role: &'static str,
    },
    RootCommit,
    ShallowCut,
    MergeCommit,
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
            Self::Usage => write!(formatter, "usage: greb <target> <commit>"),
            Self::NotACommit { role } => write!(formatter, "cannot resolve {role}"),
            Self::RootCommit => write!(formatter, "root commits are not supported"),
            Self::ShallowCut => {
                write!(
                    formatter,
                    "parent of commit is missing from this shallow clone"
                )
            }
            Self::MergeCommit => write!(formatter, "merge commits are not supported"),
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

fn resolve_commit(revision: &OsStr, role: &'static str) -> Result<Oid, Trouble> {
    let mut peeled_revision = revision.to_owned();
    peeled_revision.push("^{commit}");
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

fn has_recorded_parent(commit: &Oid) -> Result<bool, Trouble> {
    const SUBCOMMAND: &str = "cat-file";
    let stdout = git_stdout(git().args([SUBCOMMAND, "commit"]).arg(commit), SUBCOMMAND)?;
    let mut header = stdout
        .split(|byte| *byte == b'\n')
        .take_while(|line| !line.is_empty());
    Ok(header.any(|line| line.starts_with(b"parent ")))
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
        return Err(if has_recorded_parent(commit)? {
            Trouble::ShallowCut
        } else {
            Trouble::RootCommit
        });
    };
    if parents.next().is_some() {
        return Err(Trouble::MergeCommit);
    }
    Oid::parse(parent).ok_or(Trouble::UnexpectedOutput {
        subcommand: SUBCOMMAND,
    })
}

fn replay(target: &Oid, commit: &Oid, parent: &Oid) -> Result<Outcome, Trouble> {
    const SUBCOMMAND: &str = "merge-tree";
    let mut attribute_source = OsString::from("--attr-source=");
    attribute_source.push(target);
    let mut merge_base = OsString::from("--merge-base=");
    merge_base.push(parent);
    let output = git()
        .arg(attribute_source)
        .args([SUBCOMMAND, "--write-tree", "-z"])
        .arg(merge_base)
        .arg(target)
        .arg(commit)
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
    let first_field = output.stdout.split(|byte| *byte == 0).next();
    let tree = first_field
        .and_then(Oid::parse)
        .ok_or(Trouble::UnexpectedOutput {
            subcommand: SUBCOMMAND,
        })?;
    Ok(if is_conflict {
        Outcome::Conflict
    } else {
        Outcome::Clean { tree }
    })
}

fn simulate() -> Result<Outcome, Trouble> {
    let mut arguments = env::args_os().skip(1);
    let (Some(target), Some(commit), None) = (arguments.next(), arguments.next(), arguments.next())
    else {
        return Err(Trouble::Usage);
    };
    let target = resolve_commit(&target, "target")?;
    let commit = resolve_commit(&commit, "commit")?;
    let parent = single_parent(&commit)?;
    replay(&target, &commit, &parent)
}

fn print_outcome(outcome: &Outcome) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    match outcome {
        Outcome::Clean { tree } => writeln!(stdout, "clean {tree}"),
        Outcome::Conflict => writeln!(stdout, "conflict"),
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
