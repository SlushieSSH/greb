# greb

greb is a command-line tool that simulates cherry-picking one git commit onto a target, leaving your checkout and branches alone

Pass the target, then the commit (a branch name means its tip):

```console
$ greb main feature
clean ece476fca0ed64a5a31fa4d2c9c10d977acd7a38
$ greb main hotfix
conflict
```

`clean` prints the resulting tree id and exits 0, and `conflict` exits 1 without naming files

When greb cannot answer it prints `greb: <reason>` to stderr and exits 2, which root and merge commits always cause and shallow or blobless clones sometimes do

Use it only on repositories you trust: their merge drivers can run

It needs Git 2.45 or newer and has only been tested on Windows with Git 2.55

There is no release or crate: `cargo build --release` puts the binary in `target/release`
