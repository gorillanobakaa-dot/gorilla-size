# gorilla-size — Gorilla Size Calculation

## What it does (Layman Track)

Your disk is full and you do not know why. `gorilla-size` looks at a folder and tells you, biggest first, what is
inside it and how much room each part takes: like opening every drawer in a house at once and weighing what is in
each one. It can **find** things ("every file bigger than 2 GB that nobody has touched for a year"), **read** them
("show me the last 20 lines of every log that mentions ERROR"), **compare** two measurements ("what grew since last
week?"), and **tidy up**: copy, move or send to the Recycle Bin, every step checked and undoable.

Two promises: it **never deletes anything for good** (the Recycle Bin is the furthest it goes, and it checks the
item really arrived there), and it **never connects to the internet**.

## Technical Definition (Developer Track)

A single static binary (Rust, no runtime, no installer, 2.6 MiB on Windows) that walks a directory tree in parallel
on every core and reports apparent or allocated size per entry as a sorted tree (dust/erdtree style), as a filtered
list (fd style, with folder sizes as a filter, which fd cannot do) or as JSON/JSONL for programs; reads inside what
it finds (head/tail/line ranges, literal or regex search, byte-capped); hashes (SHA-256), finds duplicates, empty
folders and broken links; copies, moves and recycles with verification, a journal and undo; holds large operations
for a reviewed plan. Junctions, symlinks and mount points are listed and never followed; hard links are counted once;
what was skipped, excluded or unreadable is always reported. On Windows, `--mft` reads the NTFS master file table
directly (administrator). A per-machine profile picks the fastest thread count for each disk.

## Contents

1. The 30-second version
2. Why it exists
3. Install
4. Measure
5. Find and read
6. Act: copy, move, recycle, undo
7. The machine profile
8. The master file table (`--mft`, Windows)
9. What it counts, and what it says it did not
10. For programs: the JSON contract
11. Security: doors open and closed
12. Building from source
13. License

## 1. The 30-second version

```
gorilla-size D:\                                   what takes the space, biggest first
gorilla-size D:\ -d 3 -f 10 --types                three levels, the 10 largest files, bytes by extension
gorilla-size find --type d --min-size 5G C:\       every folder of 5 GiB or more
gorilla-size find --ext log --grep ERROR --tail 5 . logs that mention ERROR, with their last 5 lines
gorilla-size find --ext iso,zip --older 1y . --recycle    old archives to the Recycle Bin (verified, undoable)
gorilla-size dupes D:\Photos                       the same bytes in more than one place
gorilla-size . -d 3 --json > before.json           a measurement to compare later
gorilla-size compare before.json after.json        what grew, shrank, appeared or went
gorilla-size undo <journal>                        put back what an action did
```

## 2. Why it exists

On 2026-10-16 a model cleaning up Firefox source trees (about 480,000 files each) measured them with `du`. It ran
for 25 minutes on the first folder and was stopped. A hand-typed `robocopy` line got the numbers. A hand-typed
"send to the Recycle Bin" call then destroyed one folder for good instead of recycling it, and the check said
"recycled" because it only looked at whether the folder had left its place. `gorilla-size` is the tool that should
have been used: same numbers, in under a second, and an action that is checked where it matters.

Measured on a Windows 11 laptop (Intel i7-1255U: 2 performance + 8 efficiency cores, NVMe SSD), warm file system
cache, medians of alternating runs:

| Tree | gorilla-size | du (Git for Windows) |
|---|---|---|
| Firefox 155 tree, 473,152 files, 4.35 GiB | 0.6 s | stopped after 25 minutes |
| a work folder, 956,014 files, 9.0 GiB | 1.2 s | – |
| a Documents folder, 3.2 million files, 216 GiB | 4.3 to 6.0 s (depends on load) | – |

The byte counts match robocopy's and the master file table's to the byte on the first two trees.

## 3. Install

Nothing to install. Copy the one file for your system anywhere and run it:

- Windows: `gorilla-size.exe` (the C runtime is built in; no Visual C++ package needed)
- Linux: `gorilla-size` built for `x86_64-unknown-linux-musl` (static: any distribution)

To run it from anywhere, put it in a folder on your PATH (Windows: for example `%USERPROFILE%\.local\bin`; Linux:
`~/.local/bin`).

## 4. Measure

```
gorilla-size [OPTIONS] [PATH ...]      gorilla-size types [PATH ...]      gorilla-size free
gorilla-size newest|oldest [PATH ...]  gorilla-size watch PATH --every 60 gorilla-size compare OLD.json NEW.json
```

| Option | Meaning |
|---|---|
| `-d N`, `-n N` | levels shown (1); entries per level (20, the rest summed in one line) |
| `-f N` | also the N largest files anywhere |
| `--types` | bytes and files by extension |
| `--disk` | size on disk (after compression, sparse files, clusters) instead of apparent size |
| `--links` | Windows: count a hard-linked file once (opens every file: much slower). Linux/macOS always do |
| `--gitignore`, `--exclude PAT` | skip what .gitignore files ignore; skip a pattern (gitignore syntax, repeatable) |
| `--no-hidden` | skip hidden entries (the count of what was skipped is reported) |
| `--reverse`, `--flat`, `--long` | smallest first; one flat list at `--depth`; attributes/permissions and dates |
| `--progress` | live count on stderr for long scans |
| `--cross-filesystems` | Linux/macOS: enter other mounted file systems too (default: stay on one) |
| `--list-links`, `--list-errors`, `--strict` | what was not followed / not readable; exit 3 if anything was unreadable |
| `-j N` | threads (default: the calibrated count for that disk, otherwise every core) |
| `--json`, `--ascii` | machine-readable; plain characters (automatic when piped) |

**watch** measures the folder again every `--every` seconds (60) and prints the growth since the last time
(`--count N` stops after N measurements). `--alert-growth 1G` prints an `ALERT` line (JSON: `"alert": true`) and
stops with exit 3 the first time one interval grows by more than 1 GiB: a build filling the disk at 3 a.m. wakes the
script that watches it. Shrinking never alerts. `--exclude`, `--gitignore` and `--no-hidden` apply exactly as in a
measurement (before 0.3.0 they were accepted and ignored).

## 5. Find and read

```
gorilla-size find [FILTERS] [OUTPUT] [READ] [PATH ...]
```

- **Filters:** `--name GLOB`, `--regex RE`, `--ext a,b`, `--min-size 2G`, `--max-size 10M`, `--newer 7d`,
  `--older 1y`, `--type f|d|l`, `--sort size|name|age`, `-n N`, `-i` / `-s` (case). A folder's size is everything
  under it, so `--type d --min-size 10G` finds the folders holding the space.
- **Output for the next step:** `--paths` (one per line), `-0` (NUL-separated), `--jsonl` (one JSON object per line),
  `--format '{path}\t{size}\t{date}'` (fields: path, name, bytes, size, kind, mtime, date).
- **Read in the same call:** `--head N`, `--tail N` (reads only the end, even of a 50 GiB log), `--lines A:B`,
  `--grep TEXT`, `--grep-re RE`, capped by `--max-bytes` (16 KiB per file) and `--max-hits` (20 per file). Binary
  files are recognized and never dumped.
- **Also:** `hash PATH ...` (SHA-256; on a CPU with the SHA extensions the hardware instructions are used, chosen
  at run time, and `--json` names the path in `sha256_impl`: `sha-ni` or `portable`), `dupes PATH ...` (size, then the first 64 KiB, then the full hash: unique
  sizes are never read), `empty PATH`, `reparse PATH` (every link, its target, broken or not).

## 6. Act: copy, move, recycle, undo

```
gorilla-size copy SRC ... --to DIR     gorilla-size move SRC ... --to DIR     gorilla-size recycle PATH ...
find ... --copy-to DIR | --move-to DIR | --recycle                            gorilla-size undo JOURNAL
```

- **copy:** every byte of every copied file is compared with its source. An existing target is refused unless
  `--overwrite`, which first moves the old one aside (`name.gorilla-bak-<time>`), never destroys it.
- **move:** on the same drive a rename, checked; across drives a verified copy and then a verified recycle of the
  source.
- **recycle:** to the Windows Recycle Bin or the freedesktop trash (Linux). On Windows the tool **does not ask
  Windows to recycle**: it writes the item's Recycle Bin record (`$I…`) and then **renames** the item into the bin
  (`$R…`), exactly what Explorer does. A rename cannot destroy anything; if it fails, the item stays where it was.
  Explorer shows the item in the bin with its name, original location and date, and can restore it. Why: on
  2026-10-16 the usual Windows call (`SHFileOperation`, "allow undo") deleted folders **permanently** instead of
  recycling them, once on a laptop and three times on GitHub's Windows runner, after every check had passed. It is
  **refused before anything is touched** when the account has no Recycle Bin on that drive (USB sticks, network
  shares, a machine where nothing was ever deleted), the drive is set to delete immediately, the item is bigger than
  the bin, the bin would have to purge older items to make room, or a path is longer than 259 characters.
- **undo:** every action writes a journal (`%LOCALAPPDATA%\gorilla\journal`, `~/.local/state/gorilla/journal`);
  `undo` reverses it: copies are recycled, moves go back, recycled items are restored.
- **Large operations:** a move or recycle above 1,000 files or 10 GiB (`--plan-files`, `--plan-bytes`) is **held**:
  the tool writes a plan file and stops (exit 4). Review it, then `gorilla-size apply PLAN`, which skips anything that
  changed since the plan was written. `--plan` writes one on purpose. Copies are never held.
- **There is no permanent delete in gorilla-size.** `gorilla-size trash list` shows what is in the Recycle Bin
  (original place, size, date) and `gorilla-size trash restore NAME` puts something back.
- **Emptying the bin is a separate program, `gorilla-trash`, for a person to run.** `gorilla-trash empty` shows
  what is in the bin and asks once (`[y/N]`), then deletes exactly those items **for good**, on every core; anything
  that arrived after the question is left alone. Without a terminal (an agent, a script) nothing can be asked, so it
  refuses unless given `--confirm N` with N equal to the number of items there.
  Read-only files are made writable first; a junction or symlink inside an item is removed as a link and the folder
  it points to is never touched (tested). Why: Explorer emptied 1,038,326 items one by one at about 2,700 per second.

## 7. The machine profile

`gorilla-size profile` shows the processor (performance and efficiency cores, instruction sets), the memory, the
SHA-256 code path this CPU gets (`sha-ni` or `portable`) and the disks it knows. Measured 2026-10-18 on the laptop
below, 256 MiB in memory: portable 143 MiB/s, SHA-NI 1,047 MiB/s (7.3x), same digests. `gorilla-size profile --recalibrate FOLDER` walks FOLDER with 1, 2, 4 ... every thread, alternating,
several rounds, and keeps the fastest count for that disk in `%LOCALAPPDATA%\gorilla\machine.json`
(`~/.local/state/gorilla/machine.json`). Every later run on that disk uses it. A different CPU makes it re-measure.

**Is the profile right?** `gorilla-size bench --profile FOLDER` walks FOLDER with the calibrated count and with
every core, alternating, 10 rounds (`--rounds N`) after a warm-up of each, and prints the median, the middle half
and the fastest/slowest run of each. It names a winner only when the middle halves do not overlap; otherwise it says
INCONCLUSIVE. `-j N` tests another count instead of the profile's. With no calibration for that disk and no `-j`
there is nothing to test: it stops with exit 2. `gorilla-size bench A B -- ARGS` does the same for two programs
(also 10 rounds by default since 0.3.0; it was 3).

Measured on the laptop above: 1 thread 7.6 s, 4 threads 2.6 s, 8 to 12 threads 1.4 s (equal within noise). On a fast
SSD every core is already best; the profile earns its keep on hard disks, USB sticks and older machines, where too
many threads make the disk slower. The calibration runs with a warm cache: it measures day-to-day use, not the first
scan after a reboot.

## 8. The master file table (`--mft`, Windows)

NTFS keeps a table with one record per file: its name, its parent folder and its size. `--mft` opens the volume
**read-only** (nothing is written, locked or dismounted; administrator rights are needed to open it raw) and reads
the whole table in large sequential blocks.

Measured: identical bytes and file counts to the normal walk on a 473,152-file tree and on a 956,014-file tree. On a
3.2-million-file folder that was being written to during the run, the two differed by 0.09 % of the files; this is
under investigation. The table is read whole every time (about 15 s on that laptop's drive), so `--mft` is not the
fastest way to measure one folder with a warm cache (the walk is). It wins on **whole drives**, on a **cold** cache,
and when hard links must be counted once: the walk with `--links` took 192 s on the Documents folder, `--mft` 16 s.
Alternate data streams are not counted (Explorer does not count them either).

## 9. What it counts, and what it says it did not

- **Junctions, symlinks, mount points:** listed, never followed, so nothing is counted twice; the report says how many.
- **Hard links:** one file with several names is counted once (Linux/macOS always; Windows with `--links` or `--mft`).
  When it was not checked, the report says so.
- **Excluded and hidden entries:** counted and reported, never silently left out.
- **Unreadable entries:** counted and listed (`--list-errors`); `--strict` turns them into exit code 3.
- **Units:** KiB, MiB, GiB, TiB (1024-based), so a number never claims more than it is.

## 10. For programs: the JSON contract

`--json` prints one object: `tool`, `version`, `mode`, `engine` (`walk`/`mft`), `size` (`apparent`/`disk`),
`hardlinks_counted_once`, `threads`, `threads_from` (`asked`/`profile`/`every core`), `seconds`, and `roots`: one
entry per PATH with `totals` (`bytes`, `disk_bytes`, `files`, `dirs`, `links_not_followed`, `unreadable`,
`other_filesystems`, `excluded`, `hidden_skipped`, `hardlink_repeats`, `hardlink_bytes_not_counted_twice`), `tree`,
`top_files`, `types`, `links`, `errors`, `other_filesystems`, and for `find`, `found`. Actions print `journal`, `ok`,
`failed` and `actions`; a held operation prints `held`, `why` and `plan`.

Exit codes: 0 done, 2 bad usage or missing path, 3 something failed (or `--strict` with unreadable entries),
4 held for a plan.

## 11. Security: doors open and closed

- **Network:** none. No update check, no telemetry, no download. The program contains no network code.
- **What it changes:** only what you ask with `copy`, `move`, `recycle`, `undo` and `apply`; everything else reads.
  It never deletes for good.
- **Dependencies:** 15 crates, vendored in `vendor/` with their checksums (`DEPENDENCIES.md` says why each one is
  there), built with `--offline --locked`. After the one download that made `vendor/`, nobody can change what goes
  into the binary without it showing in this repository's history. SHA-256, gitignore matching and the Recycle Bin
  code are written here, not imported. Windows calls are declared by hand against `kernel32`, `advapi32` and
  `shell32`; no Windows crate, no prebuilt import library.
- **Who can act through it:** whoever can replace the binary on your disk, as with any program. Check the SHA-256 on
  the release page against the file you have.

## 12. Building from source

```
cargo build --release --offline --locked
cargo test  --release --offline --locked
```

Linux, one static file: `cargo build --release --offline --locked --target x86_64-unknown-linux-musl`. The `--mft`
agreement test needs an elevated prompt: `cargo test --release --offline --locked -- --ignored`, or
`scripts/mft-check.ps1`.

## 13. License

AGPL-3.0-or-later. Use it, change it, share it; a version you ship or run as a service stays open under the same
license.
