# Local council — correctness review of fisch_ash_ser.rs / fish_ctrl_02.rs

**Local council** — these perspectives all come from Claude playing different
roles, not from different AI vendors. Treat agreement as a shared starting
point to pressure-test, not as independent confirmation.

Question: Review fisch_ash_ser.rs (new, untested against the live game),
fish_ctrl_02.rs (earlier working variant), vision.rs, and control.rs for
correctness bugs — off-by-ones, inverted conditions, race conditions, unit
mismatches, dead code paths.

## 🗳️ Devil's Advocate

### Position
`fisch_ash_ser.rs` is a plausible-looking but fundamentally unverified redesign that quietly drops a safety net the working code relied on, and its one core behavioral decision (which way to compare ball_x vs sparkle_x) is exactly the kind of thing that fails silently and completely if the initial pixel measurements were wrong — clippy passing tells you nothing about any of this.

### Key points
- No hang/timeout safety net in `run_pin_tracking_control` (control.rs's loops have `CONTROL_TIMEOUT`; this one didn't).
- `BITE_CHECK_WINDOW = 2s` is hardcoded and untunable, vs. fish_ctrl_02's user-prompted default.
- The shape filter (`BALL_MIN_FILL_RATIO = 0.45`) is below the documented false-positive's measured 0.46 fill ratio — though the code's own comment discloses this and says area is the primary filter, so it's a disclosed tradeoff, not a hidden bug.
- The entire ball-is-proxy-for-pin model is a single untested hypothesis with no plausibility/OOB cross-check, unlike control.rs's predictive loop.
- `settings.aquarium_boost_count` is ignored in this binary (uses a hardcoded time interval instead).

### Risks & blind spots
- Passing `cargo build`/`clippy` is near-zero signal for any of this — all semantic/behavioral, not syntax.
- Hardcoded absolute screen coordinates mean a resolution/window change would silently produce "no ball ever detected," indistinguishable in logs from "fish isn't biting."

### Confidence
medium — code-level findings are concrete; "comparison direction backwards" risk is inherently speculative pending live-game verification.

## 🗳️ Simplicity Champion

### Position
The correctness risk is dwarfed by an architecture problem: four independent hand-rolled control loops plus a full duplicate copy of ~150 lines of orchestration boilerplate between the two binaries, none of it factored into the shared lib.

### Key points
- Whole-file boilerplate duplication between `fisch_ash_ser.rs` and `fish_ctrl_02.rs` (prompts, totem/aquarium/day-night scheduling, panic hook) — fish_ctrl_02's own comments document a real drift bug this exact pattern already caused once.
- `fisch_ash_ser.rs` reinvents blob detection instead of extending `vision::Blob`/`find_blobs`.
- `fish_ctrl_02.rs` also duplicates `vision::rgb_to_hsv` verbatim instead of calling the shared one.
- Per-tick PNG snapshotting is a lot of permanent machinery wrapped around a one-line comparison — reasonable as temporary field-test instrumentation, risky if left as the steady-state code path.
- The single-capture optimization (`COMBINED_REGION`) trades simple, easy-to-verify code for a likely-negligible latency win.

### Confidence
medium — duplication findings are grep-verifiable; the snapshot-machinery judgment depends on the author's intended lifespan for that code.

## 🗳️ Performance Optimizer

### Position
The hot loop is algorithmically fine, but the diagnostic instrumentation — an unbounded per-tick PNG queue — is the one design choice that can turn a long unattended session into disk/memory exhaustion.

### Key points
- `SnapshotSaver` uses an unbounded `mpsc::channel`; the hot loop enqueues a full-resolution annotated image on nearly every tick (~30ms cadence) with no cap/rotation.
- Per-tick allocation churn in `run_pin_tracking_control` (two image crops, fresh HSV vecs, fresh visited vecs) — small in absolute terms at this region size, but constant pressure.
- `fisch_ash_ser.rs`'s local blob finder re-checks the HSV predicate on every neighbor visit instead of precomputing a mask once like `vision::find_blobs` does — strictly worse than the shared implementation.
- `match_template_max` is O(n⁴)-ish with no integral-image optimization, but low-risk since it only runs every 2 minutes.

### Confidence
medium — static analysis only; no profiling run against a live session.

## 🗳️ Maintainability Advocate

### Position
Excellent local documentation, poor structural maintainability: near-identical logic copy-pasted 3-4 times, and the newest file bypasses the crate's existing calibration abstraction.

### Key points
- Triplicated boilerplate functions between the two binaries have already started to drift (different status-line clearing approaches for the same intent).
- Two competing `Blob`/`find_blobs` implementations under the same names — a correctness-adjacent trap for future grepping.
- `fisch_ash_ser.rs` hardcodes absolute screen pixel coordinates instead of using `calibration::region()`/`bar_hsv_ranges()` like `fish_ctrl_02.rs` does — fragile to window/resolution changes.
- Zero tests on `fisch_ash_ser.rs`'s core logic, unlike `fish_ctrl_02.rs` and `vision.rs`.
- `ENTER_COUNT_OVERRIDE`/`EXTRA_ENTER_COUNT` hardcoded to 0/None silently shadow `settings.toml`'s `cast_enter_count` in both binaries — a config value that looks live but isn't.

### Confidence
medium — duplication and dual-blob-finder findings are clear-cut; long-term cost depends on whether more bot variants keep getting spawned.

## Synthesis — angles, not consensus

**Shared starting points (stress-test these, don't treat as confirmed):** Three
of four members independently flagged the duplicated/reinvented blob-finder in
`fisch_ash_ser.rs` vs. `vision.rs`, and two independently flagged the
`ENTER_COUNT_OVERRIDE`/hardcoded-0 pattern silently overriding
`settings.toml`. Because all four are the same underlying model, this
convergence means "an attentive single reader would likely also notice this
from the diff," not "independent confirmation from different codebases of
knowledge."

**Genuine tensions:** Devil's-advocate and simplicity/maintainability pull in
different directions on the snapshot/logging machinery — devil sees it as
good instrumentation that's merely *reactive* (won't catch a wrong-direction
comparison in real time), while performance/maintainability see it as a
resource-growth risk if left in place after field-testing. Both are right for
different time horizons: keep it for the upcoming first live run, revisit
once `fisch_ash_ser` is confirmed working.

**Blind spots:** No member could verify the one thing that actually
determines correctness here — whether `ball_x < sparkle_x` is really the
right comparison direction for this bar skin. That's unverifiable from source
alone and can only be confirmed by running the bot against the live game.
Also unaddressed by any member: whether the hardcoded aquarium-boost interval
in `fisch_ash_ser.rs` (vs. cast-count in `fish_ctrl_02.rs`) is an intentional
redesign (it lines up with the recent "Rework aquarium boost" commit) rather
than a bug — worth confirming with the author before "fixing" it back to
match the older binary.

**Applied:** Added a `CONTROL_TIMEOUT`-based safety-net exit to
`run_pin_tracking_control` (fisch_ash_ser.rs), mirroring the pattern already
in control.rs — this was the one finding that was unambiguous, low-risk, and
purely additive (only fires if the loop would otherwise hang forever).

**Left for the user to decide** (all require live-game verification or are
plausible intentional design choices, not obvious bugs):
- `ENTER_COUNT_OVERRIDE = Some(0)` / `EXTRA_ENTER_COUNT = 0` in both binaries — ignores `settings.cast_enter_count`. Intentional current tuning, or forgotten?
- `fisch_ash_ser.rs`'s hardcoded 31-minute aquarium-boost interval vs. fish_ctrl_02's cast-count-based one — likely an intentional redesign matching the recent "Rework aquarium boost" commit, not a regression.
- `BITE_CHECK_WINDOW = 2s` hardcoded and non-configurable in fisch_ash_ser.rs, vs. fish_ctrl_02's user-prompted timeout (default higher) — may be correct for this bar skin's actual bite timing, unverifiable without a live run.
- Duplicated blob-finder, duplicated boilerplate between binaries, unbounded per-tick snapshot writes — real maintainability/resource-growth concerns, but refactoring them now (before `fisch_ash_ser` is confirmed working against the live game) risks introducing new bugs into code that hasn't been validated yet.
