# greb

greb tells you if git commits would rebase or cherry-pick cleanly onto a target, and it doesn't touch your checkout or branches

Pass the target alone to check `git rebase <target>` for the branch you're on, or follow it with commits in order (a branch name means its tip):

```console
$ greb main
clean 592badaeae0a2460c668e21de3ba6b8f58e35e27
$ greb main hotfix~1 hotfix
conflict 19bd83250202ca764b1c26275bf47e43f8949497
```

`clean` exits 0 with the final tree id, and `conflict` exits 1 with the commit greb stopped on but no file names

When greb can't answer it prints `greb: <reason>` to stderr and exits 2, which root commits and listed merges always cause and shallow or blobless clones sometimes do

Use it only on repositories you trust: their merge drivers can run

You need Git 2.45 or newer, and greb's only been tested on Windows with Git 2.55

There's no release or crate, so run `cargo build --release` and it's in `target/release`
