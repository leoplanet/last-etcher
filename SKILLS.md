# SKILLS — development cycle for this repo

## The mantra

**Test-first, verify-after, loop-until-green.**

Every change follows this loop. No exceptions, no "I'll test later", no
shipping a fix that wasn't verified. This is what stops the wack-a-mole
(where fixing one screen breaks another).

### The loop

1. **Understand first.** Read the code the change touches, end to end. Trace
   the real flow. The ladder shortens the *solution*, never the *reading*.
2. **Write/adjust the test BEFORE the edit.** If the behaviour you're about to
   change has no test, write one that captures the intended behaviour (or the
   bug, red first). If an existing test encodes the old behaviour you're
   deliberately changing, **update the test first** — a failing test you didn't
   touch is a signal you broke something you didn't mean to.
3. **Make the minimal edit.**
4. **Run the full suite** (`cargo test`) immediately.
5. **If anything fails:** decide — is the test wrong (update it, with a
   reason) or is the code wrong (fix the code)? Never silence a test to make
   it green.
6. **Repeat 3–5 until the whole suite is green AND `cargo build` is clean**
   (0 errors, 0 warnings).

Only when the suite is green do you consider the change done.

### Non-negotiables

- A green build (0 warnings) + a green test suite is the **definition of done**.
  "It compiles" is not done. "The one test I ran passed" is not done.
- GUI changes are verified by the **headless render tests**
  (`src/gui.rs` → `render_tests`), which:
    - `renders_every_screen_without_panic` — drives the app through every
      screen and catches render panics (missing font styles, painter asserts).
    - `all_screens_fit_the_same_window` — **consistency guard**: renders every
      screen into the same fixed window and fails if any overflows. This is
      what catches the "bolted on / bunched at the top / overlapping" look.
  **Run `cargo test --lib` after every GUI change.** Painter-drawn overflow
  that a fixed window can't expose still needs a visual check — say so when
  you can't verify it by test.
- When a change is visual and can't be asserted, say so explicitly and verify
  it by running the app — don't claim it works unverified.
- One task in progress at a time; don't batch unrelated changes into one
  untested commit of edits.

## Commands

```sh
cargo build          # must be 0 errors, 0 warnings
cargo test           # full suite must be green
cargo run            # run the GUI (debug)
```

Build target is redirected to local disk (`.cargo/config.toml`) because the
project lives on a noexec FUSE mount.

## Layout of the code

- `src/lib.rs` — safety core (probe, guards, identity, iso, write, verify,
  confirm). UI-agnostic, fully unit-tested. **Do not weaken these tests.**
- `src/gui.rs` — egui front end. Rendering is hard to unit-test; the headless
  render test covers it. Logic (step transitions, nav state, progress) should
  be kept pure enough to test where practical.
- `src/theme.rs` — design tokens (colors, type scale, radii). Single source of
  truth for the look. Font sizes come ONLY from `theme::type_scale`.
- `src/icons.rs` — dependency-free vector glyphs.
- `tests/flash.rs` — end-to-end flash/verify against a file target.
- `src/gui.rs` → `render_tests` — headless render smoke test across all screens.
