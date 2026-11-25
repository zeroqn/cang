---
label: wayfinder:grilling
title: earlyoom as the guest's early responder - thresholds, protection, and what it must never kill
status: closed
blocked_by: ["01-memory-failure-ladder", "08-guard-guest-init-processes"]
claimed_by: "pi (agent, 2026-11-25, delegated by bob)"
---

## Question

Bob accepts **earlyoom** as the guest's early out-of-memory responder
(2026-11-25). Its stated purpose here is not "kill something eventually" - the
kernel already does that - but **kill before the guest reaches the wall**, so
kswapd never enters the long thrash whose compression steals the CPU the
interactive path needs. Settle the design with him.

The daemon exists in nixpkgs (`earlyoom-1.9.0`) and the image ships nothing
like it (zero hits for `earlyoom` in `nix/`). From its own README/MANPAGE:

- it samples `MemAvailable` and free swap up to 10 times a second and **acts
  only when both are below their minimum**; default `SIGTERM` at <= 10% of each,
  `SIGKILL` at <= 5%;
- it kills the process with the highest `oom_score`, and **skips processes whose
  `oom_score_adj` is -1000** (changelog v1.6.1), which is exactly the protection
  `process.rs:349,363` already gives the session supervisor and that
  [08](08-guard-guest-init-processes.md) extends to the workers;
- `--avoid` / `--prefer` regexes override the score order;
- `-p` sets earlyoom's *own* niceness to -20 and its `oom_score_adj` to
  **-100** - not -1000, so a guest that wants earlyoom skipped by both killers
  must set the score itself rather than rely on `-p`;
- `-M`/`-S` take absolute KiB minima instead of percentages, and
  `-s 100` makes it ignore swap entirely.

Decisions to make, with the ladder from [01](01-memory-failure-ladder.md) and
the arms from [05](05-response-arm-kit.md):

1. **Thresholds, in this guest's units.** Percent-of-what? `MemAvailable`
   includes reclaimable page cache, and this guest's `SwapFree` falls as pages
   park in zram - so does "both below the minimum" hold at the right moment, or
   does zram make the swap condition useless (`-s 100`)? Give the number the
   ladder actually supports, not the daemon's default.
2. **Who is protected, and how.** One mechanism or two: the supervisor's
   `-1000` buys earlyoom immunity for free, so does every worker from
   [08](08-guard-guest-init-processes.md) - confirm that is the intended
   coupling, and decide whether `nix-daemon`, podman's storage/driver, the
   session PTY pumps, and the shell itself are protected by score, by `--avoid`,
   or not at all.
3. **What a kill costs the build.** Earlyoom kills the *largest* scorer, which
   under a storm is a `rustc` (a crate fails, attributed in its own log) - or, if
   `cargo` itself is largest, the whole build. Say which failure the guest
   should prefer, and whether `--prefer` should name `rustc` deliberately.
4. **Where it runs.** A detached boot worker like the swap worker (own
   session, status file under `/run/cang/`, `--after-exec` gate), or synchronously
   on the enter path - and what it costs there.
5. **Interactions.** With MGLRU `min_ttl_ms` (note 02): the kernel's answer to
   thrash is a decisive kill at TTL expiry; earlyoom's is a kill before thrash
   starts. Which one wins first, and is having both a belt-and-braces or a
   race? With the host's `ReclaimMemory` watcher: the host asks the guest to
   drop page cache, which raises `MemAvailable` - does that silence earlyoom?
6. **Off, and observability.** Same knob as the rest (`CANG_GUEST_RESPONSIVE=0`),
   or its own? What does the guest log and the status file say after a kill, so a
   later arm can attribute "the build died because of the responder" rather than
   guessing?

Resolution = `notes/11-earlyoom.md` with the chosen flags verbatim, the
protection list, the failure semantics, and the arm that proved the daemon fired
*while the interactive path was still inside its budget* (the whole point - a
kill after the 9-second p99 would be worthless).

## Resolution

Closed 2026-11-25 with bob's delegation. **Shipped as code; the live proof is
owed and named.**

### The design, from the ladder's numbers

The early responder is `earlyoom` (nixpkgs 1.9.0), added to the image's tooling
layer (`nix/image/layers.nix`) and started by the responsive component
(`components/responsive.rs::start_early_responder`), which the enter path calls
as its own profiled step (`start-early-oom-responder`) before the workload exists.
It is gated by `CANG_GUEST_RESPONSIVE`, so the map's one switch covers it.

| decision | value | why, from [note 01](01-memory-failure-ladder.md) |
|---|---|---|
| trigger | `-m 10` | the wall arrives ~2 s after `MemAvailable` passes 96 MB of 4 GiB (2.4%) with the page cache already gone, and a plain build bottoms out near 25% on a 2 GiB guest - so the trigger must sit below a healthy build and above "nothing left". 10% is also earlyoom's own default. |
| swap | `-s 100` (ignore) | this guest's swap **is** zram: `SwapFree` drains as pages park in it, so free-swap is not a pressure signal here (measured: 3738 MB of 3932 MB of slots in use for 983 MB of stored pages). Verified against earlyoom's own manpage: `-s 100` "effectively ignores swap usage". |
| priority | nice -20 in its `pre_exec` **plus its own `oom_score_adj = -1000`**, and earlyoom's own `-p` is *not* passed | it has to act inside that 2 s window. `-p` is refused because the program applies its flags **after** it starts: it would rewrite the score `pre_exec` just wrote (-1000) to -100, leaving the responder killable by the very race it runs. The niceness `-p` buys is set in the same hook, one system call earlier. |
| protection | the same `-1000` the guard gives guest-init | earlyoom skips any process at `oom_score_adj -1000` (its v1.6.1 changelog), so `process::protect_from_oom`'s set and the responder's victim set are one definition: the guest cannot lose guest-init, nix-daemon or the workers, and the responder cannot take them either. |
| victim | whatever carries the highest `oom_score` | that is a `rustc` under a build (a crate fails, attributed in its own log) or the anonymous consumer that caused the pressure - **not** the session payload that the pre-fix exec path used to lose. |
| what it cannot rescue | `cang exec` work while it sat at -1000 | it was unkillable by design, which is why [12](12-exec-inherits-oom-protection.md) fixed the score rather than leaving it to the responder. |

**The score is written in the child, not by the parent.** `pre_exec` runs
`setsid` and then `open`/`write`/`close` of a static `-1000
` into the child's own
`/proc/self/oom_score_adj`: writing it from the parent would race the child's
lifetime, and protecting the *parent* instead would hand -1000 to the shell it
later execs, which is exactly the defect ticket 12 fixed. A guest without the
binary, or one that cannot write its own score, reports and continues on the
kernel's own killer.

**Tests** (`cargo test -p cang-guest-init`: 437 passed, 0 failed; clippy
`--all-targets -D warnings` and rustfmt clean): the argument vector is asserted
against the comment's claims (percentage, `-s 100`, and the *absence* of `-p`),
the trigger is bounded
(`5 <= m < 25` - below a healthy build, above nothing-left), and the score write is
exercised through the same `open`/`write`/`close` plumbing against a file, because
lowering a real score is refused without `CAP_SYS_RESOURCE`
(`fs/proc/base.c:1130-1136`) - which is why the responder is spawned by the root
guest-init.

### What is owed

**No live arm has run with the responder.** The claim "it fires before the wall,
kills the pressure's owner, and the session survives" needs one boot of
[ticket 01](01-memory-failure-ladder.md)'s arm-4 shape on an image that carries
`earlyoom`, and the kit in `tools/responsive-arm/` is the instrument for it:

1. `nix build .#container` + `podman load` (the layer changed: `pkgs.earlyoom` was
   added), then boot with `MODE=session`, `--mem 4`, the presser payload;
2. expect `/run/cang/earlyoom.log` to name the victim, `/run/cang/responsive.status`
   to read `state=on`, the console to have **no** `Write-error on swap-device`
   cascade line count of arm 4's kind, and `mm_stat`'s `mem_used_total` **not** to
   pin at `mem_limit` before the kill;
3. the discriminating control is arm 4 itself: 175 997 refused stores and 2.2M page
   scans in the second before its kill.

That arm is the last thing between this ticket and a recorded number, and it needs
the host's nix store and CPU window (ticket [09](09-rustc-job-cap.md)'s arm holds
both at the time of writing).
