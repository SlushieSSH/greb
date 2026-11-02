# greb

greb tells you if git commits would cherry-pick cleanly onto a target, and it doesn't touch your checkout or branches

Pass the target, then the commits in order (a branch name means its tip):

```console
$ greb main feature~1 feature
clean 9003566fa47020eefd293fb9ae8ffe85e8279ffb
$ greb main hotfix~1 hotfix
conflict 19bd83250202ca764b1c26275bf47e43f8949497
```

`clean` exits 0 with the final tree id, and `conflict` exits 1 with the commit greb stopped on but no file names

When greb can't answer it prints `greb: <reason>` to stderr and exits 2, which root and merge commits always cause and shallow or blobless clones sometimes do

Use it only on repositories you trust: their merge drivers can run

You need Git 2.45 or newer, and greb's only been tested on Windows with Git 2.55

There's no release or crate, so run `cargo build --release` and it's in `target/release`
