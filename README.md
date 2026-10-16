# gorilla-size — Gorilla Size Calculation

## What it does (Layman Track)

Your disk is full and you do not know why. `gorilla-size` looks at a folder and tells you, biggest first, what is
inside it and how much room each part takes: like opening every drawer in a house at once and weighing what is in
each one. It can also **find** things ("every file bigger than 2 GB that nobody has touched for a year") and
**compare** two measurements ("what grew since last week?").

It only looks. It never deletes, moves or changes anything, and it never connects to the internet.

## Technical Definition (Developer Track)

A single static binary (Rust, no runtime, no installer) that walks a directory tree in parallel on every core and
reports apparent or allocated size per entry, as a sorted tree (dust/erdtree style), as a filtered list (fd style,
with folder sizes as a filter, which fd cannot do), or as JSON for programs. Junctions, symlinks and mount points are
listed and never followed; hard links are counted once; unreadable entries are counted and listed, never silently
skipped. On Windows, `--mft` reads the NTFS master file table of the volume directly (administrator), the method
WizTree uses.

## Table of contents

1. The 30-second version
2. Why it exists
3. Install
4. Reports
5. Find
6. Compare
7. The master file table (`--mft`, Windows)
8. What it counts, and what it says it did not
9. For programs: the JSON contract
10. Security: doors open and closed
11. Building from source
12. Licence

## 1. The 30-second version

```
gorilla-size C:\Users\me\Documents             what takes the space here, biggest first
gorilla-size -d 3 -f 10 D:\                     three levels deep, plus the ten largest files anywhere
gorilla-size find --type d --min-size 5G C:\    every folder of 5 GiB or more
gorilla-size find --ext iso,zip --older 1y .    old archives nobody has opened in a year
gorilla-size . --json > before.json             a measurement a program (or a later compare) can read
gorilla-size compare before.json after.json     what grew, shrank, appeared or went
```

## 2. Why it exists

On 2026-10-16 a model cleaning up four Firefox source trees (about 480,000 files each) measured them with `du`. It
ran for 25 minutes on the first folder and was stopped. A hand-typed `robocopy` line then got the numbers, but a
hand-typed line is not a tool: nobody can test it, and the next session types it again. `gorilla-size` is the tool.

Measured the same day on a Windows 11 laptop (12 threads, NTFS, SSD), against robocopy's totals:

| Tree | gorilla-size | robocopy |
|---|---|---|
| Firefox 157 working tree, 481,410 files | 4.47 GiB in 11.5 s (first, cold) | 4.5 GB |
| Firefox 155 tree, 538,473 files | 9.02 GiB in 1.8 s | 9.0 GB |
| Firefox 158 pristine tree, 482,864 files | 4.42 GiB in 0.43 s (warm cache) | – |

Times depend on the disk and on whether the file system cache is warm; the byte counts do not.

## 3. Install

There is nothing to install. Copy the one file for your system anywhere and run it:

- Windows: `gorilla-size.exe` (the C runtime is inside it: no Visual C++ package needed)
- Linux: `gorilla-size` built for `x86_64-unknown-linux-musl` (static: any distribution)

To make it a command everywhere, put the file in a folder on your PATH (Windows: for example
`%USERPROFILE%\.local\bin`; Linux: `~/.local/bin`).

## 4. Reports

```
gorilla-size [OPTIONS] [PATH ...]
```

| Option | Meaning |
|---|---|
| `-d, --depth N` | levels shown (default 1) |
| `-n, --number N` | entries per level (default 20); the rest are summed into one "... N more" line |
| `-f, --files N` | also the N largest files anywhere under PATH |
| `--disk` | size on disk (allocated: after compression, sparse files and cluster rounding) instead of apparent size. Linux: free. Windows: one extra call per file |
| `--links` | Windows: count a hard-linked file once (opens each file; slower). Linux and macOS always do |
| `--cross-filesystems` | Linux/macOS: walk into other mounted file systems too (default: stay on the one PATH is on) |
| `--list-links`, `--list-errors` | list what was not followed / not readable |
| `-j, --threads N` | worker threads (default: every core) |
| `--json` | machine-readable (section 9) |
| `--ascii` | plain characters; automatic when the output is piped |
| `--strict` | exit 3 when anything could not be read |

## 5. Find

```
gorilla-size find [--name GLOB] [--ext a,b] [--min-size S] [--max-size S] [--newer AGE] [--older AGE]
                  [--type f|d|l] [--sort size|name|age] [-n N] [PATH ...]
```

Sizes: `500M`, `2G`, `1.5T` (binary: 1 G = 1024³ bytes, as Explorer and du mean it). Ages: `90s`, `30m`, `12h`,
`7d`, `2w`, `1y`. A folder's size is everything under it, so `--type d --min-size 10G` finds the folders that hold
the space. Names ignore case on Windows and macOS (`-s` to match case).

## 6. Compare

```
gorilla-size -d 3 --json D:\Work > monday.json
gorilla-size -d 3 --json D:\Work > friday.json
gorilla-size compare monday.json friday.json
```

Every entry both reports list, sorted by how much it changed: `grew`, `shrank`, `new`, `gone`.

## 7. The master file table (`--mft`, Windows)

NTFS keeps a table with one record per file: its name, its parent folder and its size. Reading that table from start
to end, in large sequential blocks, is far faster than asking for every folder one by one. It needs administrator
rights because the volume is opened raw. It opens it **read-only**: nothing is written, locked or dismounted.

```
gorilla-size --mft C:\           (from an administrator prompt)
```

Said plainly: alternate data streams are not counted (Explorer does not count them either), and only NTFS has the
table; on any other file system use the normal mode.

## 8. What it counts, and what it says it did not

- **Junctions, symlinks, mount points:** listed, never followed, so nothing is counted twice. The report says how many.
- **Hard links:** one file with several names is counted once (Linux/macOS always; Windows with `--links` or `--mft`).
  When it was not checked, the report says so instead of guessing.
- **Unreadable entries:** counted and listed (`--list-errors`); `--strict` turns them into exit code 3.
- **Apparent vs on disk:** the default is what the files hold. `--disk` is what they occupy.
- **Units:** printed as KiB, MiB, GiB, TiB (1024-based), so a number never claims more than it is.

## 9. For programs: the JSON contract

`--json` prints one object: `tool`, `version`, `mode` (`report`/`find`), `engine` (`walk`/`mft`), `size`
(`apparent`/`disk`), `hardlinks_counted_once`, `threads`, `seconds`, and `roots`: one entry per PATH with
`path`, `seconds`, `totals` (`bytes`, `disk_bytes`, `files`, `dirs`, `links_not_followed`, `unreadable`,
`other_filesystems`, `hardlink_repeats`, `hardlink_bytes_not_counted_twice`), `tree` (name, kind, bytes, disk_bytes,
files, dirs, mtime, children — to `--depth`), `top_files`, `links`, `errors`, `other_filesystems`, and for `find`,
`found`. Exit codes: 0 done, 2 bad usage or missing path, 3 `--strict` with unreadable entries.

## 10. Security: doors open and closed

- **Network:** none. There is no update check, no telemetry, no download, no "phone home". The program has no
  network code.
- **Writes:** none. It reads directory listings and file metadata; it never opens a file's contents (the `--mft`
  mode reads the volume's table, read-only).
- **Dependencies:** 11 crates (rayon and its crossbeam parts; serde_json and its helpers), all vendored in `vendor/`
  with their checksums, built with `--offline --locked`. After the one download that made `vendor/`, nobody can
  change what goes into the binary without it showing in this repository's history. Windows calls are declared by
  hand against `kernel32` and `advapi32`; no Windows crate, no prebuilt import library.
- **Who can act through it:** whoever can replace the binary on your disk, as with any program. Check the SHA-256 on
  the release page against the file you have.

## 11. Building from source

```
cargo build --release --offline --locked
cargo test  --release --offline --locked
```

Linux, one static file: `cargo build --release --offline --locked --target x86_64-unknown-linux-musl`. The `--mft`
agreement test needs an elevated prompt: `cargo test --release --offline --locked -- --ignored`.

## 12. Licence

AGPL-3.0-or-later. Use it, change it, share it; a version you ship or run as a service stays open under the same
licence.
