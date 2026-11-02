use std::fs;
use std::path::{Path, PathBuf};
use std::process::{self, Command, Output, Stdio};

const GIT_CONFIG: &str =
    "[user]\n\tname = fixture\n\temail = fixture@example.invalid\n[core]\n\tautocrlf = false\n";

#[derive(Debug)]
struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Run {
    fn capture(output: &Output) -> Self {
        Self {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    fn assert_clean(&self, expected_tree: &str) {
        assert_eq!(self.code, Some(0), "{self:?}");
        assert_eq!(self.stdout, format!("clean {expected_tree}\n"), "{self:?}");
    }

    fn assert_conflict(&self, commit: &str) {
        assert_eq!(self.code, Some(1), "{self:?}");
        assert_eq!(self.stdout, format!("conflict {commit}\n"), "{self:?}");
    }

    fn assert_trouble(&self, message: &str) {
        assert_eq!(self.code, Some(2), "{self:?}");
        assert_eq!(self.stdout, "", "{self:?}");
        assert_eq!(self.stderr, format!("greb: {message}\n"), "{self:?}");
    }
}

struct Fixture {
    home: PathBuf,
    repo: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        Self::with_object_format(name, "sha1")
    }

    fn with_object_format(name: &str, object_format: &str) -> Self {
        let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
        if root.exists() {
            fs::remove_dir_all(&root).expect("a previous fixture is safe to delete");
        }
        let home = root.join("home");
        let repo = root.join("repo");
        fs::create_dir_all(&home).expect("the target dir is writable");
        fs::create_dir_all(&repo).expect("the target dir is writable");
        fs::write(home.join(".gitconfig"), GIT_CONFIG).expect("home was just created");
        let fixture = Self { home, repo };
        fixture.git(&[
            "init",
            "-q",
            "-b",
            "main",
            &format!("--object-format={object_format}"),
        ]);
        fixture
    }

    fn in_repo(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(program);
        command
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join("xdg"));
        command
    }

    fn git(&self, arguments: &[&str]) -> String {
        let output = self
            .in_repo("git")
            .args(arguments)
            .output()
            .expect("git is installed");
        assert!(
            output.status.success(),
            "git {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn commit(&self, files: &[(&str, &str)]) -> String {
        for (name, contents) in files {
            fs::write(self.repo.join(name), contents).expect("the fixture repo is writable");
            self.git(&["add", name]);
        }
        self.git(&["commit", "-q", "-m", "fixture commit"]);
        self.git(&["rev-parse", "HEAD"])
    }

    fn greb_command(&self) -> Command {
        self.in_repo(env!("CARGO_BIN_EXE_greb"))
    }

    fn greb(&self, arguments: &[&str]) -> Run {
        let output = self
            .greb_command()
            .args(arguments)
            .output()
            .expect("cargo built greb");
        Run::capture(&output)
    }

    fn cherry_pick_tree(&self, target: &str, commits: &[&str]) -> String {
        self.git(&["checkout", "-q", "--detach", target]);
        for commit in commits {
            self.git(&["cherry-pick", commit]);
        }
        self.git(&["rev-parse", "HEAD^{tree}"])
    }

    fn cherry_pick_stop(&self, target: &str, commits: &[&str]) -> String {
        self.git(&["checkout", "-q", "--detach", target]);
        let picked = self
            .in_repo("git")
            .arg("cherry-pick")
            .args(commits)
            .output()
            .expect("git is installed");
        assert!(!picked.status.success(), "cherry-pick was expected to stop");
        let unmerged = self.git(&["diff", "--name-only", "--diff-filter=U"]);
        assert!(
            !unmerged.is_empty(),
            "cherry-pick stopped without a conflict"
        );
        self.git(&["rev-parse", "CHERRY_PICK_HEAD"])
    }
}

struct Diverged {
    fixture: Fixture,
    base: String,
    target: String,
    commit: String,
}

fn diverged_on_one_file(name: &str) -> Diverged {
    let fixture = Fixture::new(name);
    let base = fixture.commit(&[("shared.txt", "base\n")]);
    fixture.git(&["switch", "-q", "-c", "feature"]);
    let commit = fixture.commit(&[("shared.txt", "feature\n")]);
    fixture.git(&["switch", "-q", "main"]);
    let target = fixture.commit(&[("shared.txt", "target\n")]);
    Diverged {
        fixture,
        base,
        target,
        commit,
    }
}

fn diverged_on_two_files(fixture: &Fixture) -> (String, String) {
    fixture.commit(&[("target.txt", "base\n"), ("feature.txt", "base\n")]);
    fixture.git(&["switch", "-q", "-c", "feature"]);
    let commit = fixture.commit(&[("feature.txt", "feature\n")]);
    fixture.git(&["switch", "-q", "main"]);
    let target = fixture.commit(&[("target.txt", "target\n")]);
    (target, commit)
}

#[test]
fn clean_commit_gives_the_tree_cherry_pick_builds() {
    let fixture = Fixture::new("clean");
    let (target, commit) = diverged_on_two_files(&fixture);
    assert_ne!(target, commit);

    let run = fixture.greb(&[&target, &commit]);

    assert_eq!(fixture.git(&["rev-parse", "HEAD"]), target);
    assert_eq!(fixture.git(&["status", "--porcelain"]), "");
    run.assert_clean(&fixture.cherry_pick_tree(&target, &[&commit]));
}

#[test]
fn sha256_repo_gives_the_tree_cherry_pick_builds() {
    let fixture = Fixture::with_object_format("sha256", "sha256");
    let (target, commit) = diverged_on_two_files(&fixture);
    assert_eq!(target.len(), 64);

    let run = fixture.greb(&[&target, &commit]);

    run.assert_clean(&fixture.cherry_pick_tree(&target, &[&commit]));
}

#[test]
fn conflicting_commit_stops_cherry_pick_too() {
    let Diverged {
        fixture,
        target,
        commit,
        ..
    } = diverged_on_one_file("conflict");

    let run = fixture.greb(&[&target, &commit]);

    assert_eq!(fixture.git(&["status", "--porcelain"]), "");
    run.assert_conflict(&fixture.cherry_pick_stop(&target, &[&commit]));
}

#[test]
fn only_the_commits_own_change_is_replayed() {
    let fixture = Fixture::new("own-change");
    fixture.commit(&[("shared.txt", "base\n"), ("other.txt", "base\n")]);
    fixture.git(&["switch", "-q", "-c", "feature"]);
    fixture.commit(&[("shared.txt", "feature\n")]);
    let commit = fixture.commit(&[("other.txt", "feature\n")]);
    fixture.git(&["switch", "-q", "main"]);
    let target = fixture.commit(&[("shared.txt", "target\n")]);
    let tip_merge = fixture
        .in_repo("git")
        .args(["merge-tree", "--write-tree", &target, &commit])
        .output()
        .expect("git is installed");
    assert_eq!(tip_merge.status.code(), Some(1), "tips must conflict here");

    let run = fixture.greb(&[&target, &commit]);

    run.assert_clean(&fixture.cherry_pick_tree(&target, &[&commit]));
}

fn fixture_with_union_branch(name: &str) -> (Fixture, String, String, String) {
    let fixture = Fixture::new(name);
    fixture.commit(&[("notes.txt", "base\n")]);
    fixture.git(&["switch", "-q", "-c", "feature"]);
    let commit = fixture.commit(&[("notes.txt", "feature\n")]);
    fixture.git(&["switch", "-q", "main"]);
    let plain_target = fixture.commit(&[("notes.txt", "target\n")]);
    let union_target = fixture.commit(&[(".gitattributes", "notes.txt merge=union\n")]);
    (fixture, plain_target, union_target, commit)
}

#[test]
fn attributes_on_the_target_decide_a_clean_merge() {
    let (fixture, plain_target, union_target, commit) = fixture_with_union_branch("attr-target");
    fixture.git(&["checkout", "-q", "--detach", &plain_target]);

    let run = fixture.greb(&[&union_target, &commit]);

    run.assert_clean(&fixture.cherry_pick_tree(&union_target, &[&commit]));
}

#[test]
fn attributes_in_the_checkout_cannot_fake_a_clean_merge() {
    let (fixture, plain_target, _, commit) = fixture_with_union_branch("attr-checkout");

    let run = fixture.greb(&[&plain_target, &commit]);

    run.assert_conflict(&fixture.cherry_pick_stop(&plain_target, &[&commit]));
}

#[test]
fn each_commit_lands_on_the_one_before_it() {
    let fixture = Fixture::new("chain");
    fixture.commit(&[("notes.txt", "one\n"), ("other.txt", "base\n")]);
    fixture.git(&["switch", "-q", "-c", "feature"]);
    let first = fixture.commit(&[("notes.txt", "one\ntwo\n")]);
    let second = fixture.commit(&[("notes.txt", "one\ntwo\nthree\n")]);
    fixture.git(&["switch", "-q", "main"]);
    let target = fixture.commit(&[("other.txt", "target\n")]);
    fixture.greb(&[&target, &second]).assert_conflict(&second);

    let run = fixture.greb(&[&target, &first, &second]);

    run.assert_clean(&fixture.cherry_pick_tree(&target, &[&first, &second]));
}

#[test]
fn replay_stops_at_the_first_conflict() {
    let fixture = Fixture::new("first-conflict");
    fixture.commit(&[("shared.txt", "base\n"), ("other.txt", "base\n")]);
    fixture.git(&["switch", "-q", "-c", "feature"]);
    let first = fixture.commit(&[("shared.txt", "feature\n")]);
    let second = fixture.commit(&[("other.txt", "feature\n")]);
    fixture.git(&["switch", "-q", "main"]);
    let target = fixture.commit(&[("shared.txt", "target\n")]);

    let run = fixture.greb(&[&target, &first, &second]);

    run.assert_conflict(&fixture.cherry_pick_stop(&target, &[&first, &second]));
}

#[test]
fn conflict_names_the_commit_that_stopped() {
    let fixture = Fixture::new("later-conflict");
    fixture.commit(&[("shared.txt", "base\n"), ("other.txt", "base\n")]);
    fixture.git(&["switch", "-q", "-c", "feature"]);
    let first = fixture.commit(&[("other.txt", "feature\n")]);
    let second = fixture.commit(&[("shared.txt", "feature\n")]);
    fixture.git(&["switch", "-q", "main"]);
    let target = fixture.commit(&[("shared.txt", "target\n")]);

    let run = fixture.greb(&[&target, &first, &second]);

    run.assert_conflict(&fixture.cherry_pick_stop(&target, &[&first, &second]));
}

#[test]
fn attributes_from_an_earlier_commit_apply_to_later_ones() {
    let fixture = Fixture::new("attr-chain");
    fixture.commit(&[("notes.txt", "base\n")]);
    fixture.git(&["switch", "-q", "-c", "feature"]);
    let attributes = fixture.commit(&[(".gitattributes", "notes.txt merge=union\n")]);
    let edit = fixture.commit(&[("notes.txt", "feature\n")]);
    fixture.git(&["switch", "-q", "main"]);
    let target = fixture.commit(&[("notes.txt", "target\n")]);
    fixture.greb(&[&target, &edit]).assert_conflict(&edit);

    let run = fixture.greb(&[&target, &attributes, &edit]);

    run.assert_clean(&fixture.cherry_pick_tree(&target, &[&attributes, &edit]));
}

#[test]
fn inherited_git_variables_cannot_redirect_greb() {
    let fixture = Fixture::new("inherited");
    let (target, commit) = diverged_on_two_files(&fixture);
    let missing = fixture.home.join("missing");
    let expected_tree = fixture.cherry_pick_tree(&target, &[&commit]);

    for variable in [
        "GIT_DIR",
        "git_dir",
        "GIT_WORK_TREE",
        "GIT_OBJECT_DIRECTORY",
    ] {
        let output = fixture
            .greb_command()
            .env(variable, &missing)
            .args([&target, &commit])
            .output()
            .expect("cargo built greb");
        Run::capture(&output).assert_clean(&expected_tree);
    }
}

#[test]
fn closed_stdout_keeps_the_conflict_exit_code() {
    let Diverged {
        fixture,
        target,
        commit,
        ..
    } = diverged_on_one_file("closed-stdout");
    let mut child = fixture
        .greb_command()
        .args([&target, &commit])
        .stdout(Stdio::piped())
        .spawn()
        .expect("cargo built greb");
    drop(child.stdout.take());

    let status = child.wait().expect("greb was spawned");

    assert_eq!(status.code(), Some(1));
}

#[test]
fn revisions_that_are_not_one_commit_are_trouble() {
    let Diverged {
        fixture,
        target,
        commit,
        ..
    } = diverged_on_one_file("not-a-commit");

    for revision in [
        "missing",
        "",
        "^main",
        "main..feature",
        "main^{tree}",
        "--version",
        "--output=greb-was-tricked",
    ] {
        fixture
            .greb(&[revision, &commit])
            .assert_trouble("cannot resolve target");
        fixture
            .greb(&[&target, revision])
            .assert_trouble("cannot resolve commit 1");
        fixture
            .greb(&[&target, &commit, revision])
            .assert_trouble("cannot resolve commit 2");
    }
    assert!(!fixture.repo.join("greb-was-tricked").exists());
}

#[test]
fn ref_named_like_an_option_is_still_a_ref() {
    let fixture = Fixture::new("dash-ref");
    let (target, commit) = diverged_on_two_files(&fixture);
    fixture.git(&["update-ref", "refs/heads/-dash", &target]);

    let run = fixture.greb(&["-dash", &commit]);

    run.assert_clean(&fixture.cherry_pick_tree(&target, &[&commit]));
}

#[test]
fn root_commit_is_trouble_not_a_guess() {
    let Diverged {
        fixture,
        base,
        target,
        ..
    } = diverged_on_one_file("root");
    assert_eq!(fixture.git(&["rev-list", "--count", &base]), "1");

    fixture
        .greb(&[&target, &base])
        .assert_trouble("root commits are not supported");
}

#[test]
fn root_commit_message_cannot_fake_a_parent() {
    let fixture = Fixture::new("root-message");
    fixture.git(&[
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "parent of everything",
    ]);
    let root = fixture.git(&["rev-parse", "HEAD"]);

    fixture
        .greb(&[&root, &root])
        .assert_trouble("root commits are not supported");
}

#[test]
fn shallow_cut_is_not_called_a_root_commit() {
    let Diverged {
        fixture, target, ..
    } = diverged_on_one_file("shallow");
    let clone = fixture.repo.with_file_name("clone");
    let clone_path = clone.to_str().expect("fixture paths are UTF-8");
    let repo_path = fixture.repo.display().to_string().replace('\\', "/");
    let source = format!("file:///{}", repo_path.trim_start_matches('/'));
    fixture.git(&["clone", "-q", "--depth", "1", &source, clone_path]);
    assert_eq!(
        fixture.git(&["-C", clone_path, "rev-parse", "--is-shallow-repository"]),
        "true"
    );
    assert_eq!(
        fixture.git(&[
            "-C",
            clone_path,
            "rev-list",
            "--parents",
            "--max-count=1",
            &target
        ]),
        target,
        "rev-list must hide the parent here"
    );

    let output = fixture
        .greb_command()
        .current_dir(&clone)
        .args([&target, &target])
        .output()
        .expect("cargo built greb");

    Run::capture(&output).assert_trouble("parent of commit is missing from this shallow clone");
}

#[test]
fn merge_commit_is_trouble_not_a_guess() {
    let fixture = Fixture::new("merge");
    let (target, commit) = diverged_on_two_files(&fixture);
    fixture.git(&["merge", "-q", "--no-ff", "-m", "fixture merge", &commit]);
    let merge = fixture.git(&["rev-parse", "HEAD"]);
    assert_eq!(fixture.git(&["rev-parse", "HEAD^2"]), commit);

    fixture
        .greb(&[&target, &merge])
        .assert_trouble("merge commits are not supported");
}

#[test]
fn wrong_argument_count_is_trouble() {
    let fixture = Fixture::new("arguments");

    for arguments in [&[][..], &["main"][..]] {
        fixture
            .greb(arguments)
            .assert_trouble("usage: greb <target> <commit>...");
    }
}

#[test]
fn outside_a_repository_is_trouble() {
    let fixture = Fixture::new("outside");
    let outside = std::env::temp_dir().join(format!("greb-outside-{}", process::id()));
    fs::create_dir_all(&outside).expect("the system temp dir is writable");
    let probe = fixture
        .in_repo("git")
        .current_dir(&outside)
        .args(["rev-parse", "--git-dir"])
        .output()
        .expect("git is installed");
    assert!(
        !probe.status.success(),
        "the temp dir must not be in a repo"
    );

    let output = fixture
        .greb_command()
        .current_dir(&outside)
        .args(["HEAD", "HEAD"])
        .output()
        .expect("cargo built greb");

    Run::capture(&output).assert_trouble("cannot resolve target");
    fs::remove_dir(&outside).expect("the test left the temp dir empty");
}

#[test]
fn git_failure_is_trouble_not_a_verdict() {
    let fixture = Fixture::new("corrupt");
    let (target, commit) = diverged_on_two_files(&fixture);
    let tree = fixture.git(&["rev-parse", &format!("{target}^{{tree}}")]);
    let (directory, file) = tree.split_at(2);
    let tree_object = fixture
        .repo
        .join(".git")
        .join("objects")
        .join(directory)
        .join(file);
    fs::remove_file(tree_object).expect("a new tree is a loose object");

    let run = fixture.greb(&[&target, &commit]);

    assert_eq!(run.code, Some(2), "{run:?}");
    assert_eq!(run.stdout, "", "{run:?}");
    assert!(
        run.stderr.starts_with("greb: git merge-tree failed"),
        "{run:?}"
    );
}

#[test]
fn promisor_repo_cannot_run_ssh_through_a_missing_commit() {
    let fixture = Fixture::new("promisor");
    let (target, _) = diverged_on_two_files(&fixture);
    let marker = fixture.home.join("ssh-ran");
    let ssh_command = format!("touch '{}'; exit 1", marker.display()).replace('\\', "/");
    fixture.git(&["config", "extensions.partialClone", "origin"]);
    fixture.git(&[
        "config",
        "remote.origin.url",
        "ssh://nobody.invalid/repo.git",
    ]);
    fixture.git(&["config", "remote.origin.promisor", "true"]);
    fixture.git(&["config", "core.sshCommand", &ssh_command]);
    let missing_commit = "1".repeat(target.len());
    fixture.git(&[
        "update-ref",
        "--no-deref",
        "refs/heads/placeholder",
        &target,
    ]);
    let ghost_ref = fixture.repo.join(".git/refs/heads/placeholder");
    fs::write(ghost_ref, format!("{missing_commit}\n")).expect("loose refs are writable");
    let probe = fixture
        .in_repo("git")
        .args(["rev-parse", "--verify", "placeholder^{commit}"])
        .output()
        .expect("git is installed");
    assert!(!probe.status.success());
    assert!(marker.exists(), "plain git must reach ssh in this fixture");
    fs::remove_file(&marker).expect("the marker was just checked");

    fixture
        .greb(&["placeholder", &target])
        .assert_trouble("cannot resolve target");

    assert!(!marker.exists(), "greb ran the repo's ssh command");
}
