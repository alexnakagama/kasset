# Tasks

Every group ends green: `cargo test` (the whole suite) and `cargo clippy --all-targets -- -D warnings` pass before the group is handed off for review. Each group keeps the `//!` docs of the modules it touches, and its line in `CLAUDE.md`'s Architecture section, accurate.

## 1. Pure inference (new `src/library/infer.rs`)

- [ ] 1.1 Add `src/library/infer.rs` (declared from `library.rs`) with a `//!` doc stating its role and invariants: pure, no I/O, input is a path relative to the library root, every returned string sanitised, and inference never overrides tags (the caller enforces that). Implement `Inferred` and `infer(relative)` as in D1–D3 (leading number, first spaced dash/en dash/em dash split, label vocabulary, bracketed groups anywhere, trailing unbracketed run only when it starts with "official", never empty, folder rule needing ≥ 2 parent components after skipping a disc-folder parent, with the file-name artist winning). Verify with unit tests, one per spec scenario of *Artist and title from the file name*, *Track number from the file name*, *Video suffixes removed from inferred titles* and *Album and artist from folders*, plus:
  - `" - Song"`, `"Band - "` and `" - "` do not split (empty side);
  - `1999 - Song` gives artist "1999", no number; `01.mp3`-style stems (number only) give no number and no title (the caller falls back to the stem); `50 Cent - In Da Club` and `2 Chainz - Song` keep the number in the artist, `99 Problems` and `10 Song` give no number and keep the full title, `10 - Song` and `10. Song` give number 10, and `01 Band - Song` gives number 1 and artist "Band"; `Song - Official Video` gives "Song" (no dangling dash);
  - `00 - Intro` gives no number and title "Intro"; `01 – Song` and `01 — Song` (en/em dash) give number 1 and title "Song";
  - non-ASCII stems (`Björk - Jóga`, `坂本龍一 — energy flow`, a 300-character stem of multibyte characters) neither panic nor cut inside a character;
  - `[Official Video]` with square brackets, `(official AUDIO)` in mixed case, and two label groups in a row are all removed; `(Live)` and `(Remix)` stay;
  - a stem of 256+ characters and one full of brackets (`((((`, unclosed `[`) neither panic nor stall;
  - control characters in the stem and in folder names are sanitised (spec *Control characters in a file name*);
  - disc folders: `CD1`, `cd 1`, `Disc 2`, `DISK-02`, `disc_3` are skipped; `CD`, `CD123`, `Discography`, `CDs` are not (they are taken as the album); a disc folder with only one folder above it gives no folder values;
  - Windows-style relative paths via `Path::components` (unix-independent test using `PathBuf::from_iter`).

## 2. Wire into the scan (`src/library.rs`)

- [ ] 2.1 Add `read_track_in(root, path)` with the merge order of D5, and keep `read_track(path)` as the file-name-only wrapper. Make `scan` call `read_track_in(root, path)` (`root: &Path`, `path: PathBuf`). Existing `library.rs` and `player.rs` tests must pass unchanged (the untagged test files such as `my track.mp3` have no dash and no number, and `read_track` never looks at folders). Update the `read_track` doc comment. Verify with tests on temp files (the in-memory WAV from `test_support`):
  - an untagged `Band - Song (Official Audio).wav` at the root scans as artist "Band", title "Song", album "Unknown";
  - an untagged `<root>/Band/LP/02 Song.wav` scans as artist "Band", album "LP", number 2, title "Song", and the same file under `<root>/Band/LP/CD1/` gives the same values;
  - a WAV whose tag has artist "Real" and title "T" under `<root>/X/Y/Other - Name.wav` keeps "Real" and "T", and gets album "Y" (spec *Tags win over the file name*);
  - a WAV tagged with artist "Band" and no title under `Other - Name.wav` gives artist "Band", title "Name" (spec *Artist tag present, title tag missing*);
  - a tag track number > 0 beats the inferred one (`07 Song.wav` tagged 3 gives 3), and tag number 0 or none uses the inferred one;
  - `read_track_in` with a path that is not under `root` falls back to the file name only (no folder inference, no panic);
  - `scan` sorts the inferred artists with tagged ones (spec *Library ordering* unchanged).
- [ ] 2.2 Update `README.md` (feature line: tags, else the file name `Artist - Title` and `Artist/Album/` folders, with video labels removed) and the `library.rs` line in `CLAUDE.md`'s Architecture section. Verify by reading the rendered README section.

## 3. Integration check

- [ ] 3.1 Run `cargo run -- ~/Música` (or a temp folder with copies of untagged MP3s named like YouTube downloads) and confirm by eye that artist and title are filled in, labels are gone, and "Calling (Spider-Man Across the Spider-Verse)" stays Unknown/unchanged. Run `cargo +1.89 build` (no new dependencies, so the MSRV is unchanged).

## Workflow follow-up

- Hand each group to the reviewer and fix its findings before starting the next.
- Sync the delta into `openspec/specs/music-library/spec.md` and archive the change once it is reviewed.
