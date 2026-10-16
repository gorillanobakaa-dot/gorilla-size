# Dependencies

Every crate is vendored in `vendor/` (with cargo's checksums) and the build runs `--offline --locked`. A new crate is
a new door: it is added only with a reason here, and the change shows in this repository's history.

Compiled into the binary (`cargo tree -e normal`):

| Crate | Why |
|---|---|
| rayon, rayon-core | the parallel walk: every core, work stealing between threads |
| crossbeam-deque, crossbeam-epoch, crossbeam-utils | rayon's work-stealing queues |
| either | used by rayon |
| serde_json | `--json` output and reading reports back for `compare` and `apply` |
| serde_core, itoa, memchr, zmij | used by serde_json (number formatting, byte search) |
| regex, regex-automata, regex-syntax, aho-corasick | `--regex` and `--grep-re` (added 2026-10-16 with the owner's yes) |

Present in `vendor/` but not compiled in (cargo resolves them for optional features): serde, serde_derive,
proc-macro2, quote, syn, unicode-ident.

Written here instead of imported: SHA-256 (`src/sha256.rs`, checked against the NIST vectors), gitignore matching
(`src/ignore.rs`), globs (`src/glob.rs`), the Recycle Bin and freedesktop trash code (`src/actions.rs`), the NTFS
master file table reader (`src/mft.rs`), all Windows API declarations (`src/os.rs`, `src/fastdir.rs`,
`src/profile.rs`).
