# Repository and crate structure

**A recommendation to think over, not a decision.** Nothing here is in the decisions log of
[`STATUS.md`](STATUS.md), and nothing in the tree changed because of it. Written on 2 October 2026
from three independent analyses of how Sighurt's crates and repositories could be split or merged
in the long term, each from one lens:

- **maintenance**: what changes together, how changes are reviewed, what the tooling assumes;
- **contributors**: what someone forking the chrome, adding an engine or writing a guest app needs;
- **build and CI cost**: dependency graphs, rebuild times, lockfiles, disk.

Where the analyses disagreed, the tree was checked; [the last section](#where-the-analyses-disagreed)
says what was found. Numbers without a source were counted in this tree on 2 October 2026.

## In short

- The nine product crates are the crates of plan §5, and each boundary enforces a rule that a
  merge would lose. The feeling of "too many crates" comes from the other 13 workspace members:
  10 example guests, the `fullstack-notes` backend and 2 finished experiments.
- A repository boundary pays only where the contract across it is not Rust types.
  `sighurt-api` is rebuilt with the workspace and is "not a stable native plugin ABI" (§5), so
  everything compiled against it has to move in lockstep. That rules out separate repositories
  for the chrome or the engines today. The guest SDK (the `"oxide"` ABI), a future
  out-of-process engine (a wire protocol) and bench (the diagnostics format) are the only places
  where a boundary could sit.
- What actually costs time is the size of Servo's and the Wasm engine's dependency graphs (645
  and 549 packages), and workspace-wide commands that reach Servo by accident. Neither needs
  another repository to fix.
- **Recommended direction:** keep one repository and the nine crates. Trim and fence the
  workspace: the experiments go once `sig` carries their evidence, `default-members` gets added,
  a test checks that the SDK and the engine agree, and the SDK gets its own version. Revisit at
  the triggers in [Later](#later-and-what-reopens-the-question), and at M7 at the latest.

## The structure today

The root `Cargo.toml` has 22 members.

| Crate | Rust lines (src + tests and examples) | Packages in its graph¹ | Project dependencies | Used by |
|---|---|---|---|---|
| `sighurt-api` | 2 228 | 1 | — | every native crate |
| `sighurt-core` | 4 758 + 1 828 | 43 | api | `sig` |
| `sighurt-wayland` | 6 300 + 1 747 | 51 | api | `sig`, every engine |
| `sighurt-ui` | 3 816 + 1 620 | 58 | api (decision 10) | `sig` |
| `sighurt-engine-demo` | 817 | 52 | api, wayland | `sig` (`engine-demo`) |
| `sighurt-engine-servo` | 3 248 + 1 112 | 645 | api, wayland | `sig` (`engine-servo`) |
| `sighurt-engine-wasm` | 16 949 + 2 464 | 549 | api, wayland | `sig` (`engine-wasm`) |
| `sighurt-browser` (`sig`) | 3 682 + 80 | 206 with the demo engine, 935 by default | all of the above | — |
| `sighurt-sdk` | 4 333 | 1 (wasm32) | — | the guests |

¹ `cargo tree -e normal,build` for the Linux target. With normal edges only the counts are
1, 43, 47, 57, 48, 608, 519, 197 and 893. The analyses quoted one or the other.

The other members, and what lies around them:

- **Example guests**: 10 `cdylib` crates for `wasm32-unknown-unknown` and the native
  `fullstack-notes` backend (axum), 2.4k lines in all. They are the SDK's compatibility corpus
  (§24). The Wasm engine's tests, its `headless.sh` and CI load four of them from
  `target/wasm32-unknown-unknown/release`.
- **Experiments**: `m0-coexist` (1.4k lines) and `m1-mixed` (3.0k), the evidence for M0 and M1.
  Both depend on Servo, and `m0-coexist` enables Servo's `testbinding` feature.
- **`bench/`**: Python standard library only. It holds 228 of the 460 tracked files (2.5 of
  6.5 MB), 183 of them result JSON, and builds `sig` on louise-pc from a `git archive` of a
  revision.
- **`tools/`, `shell.nix`, `nix/`**: the machine tooling. The WebKit track is adding (uncommitted)
  a pinned nixpkgs and a WPE WebKit build into a prefix outside the Nix store.

There is one `Cargo.lock`, with 1 229 packages, seeded from Servo's (decision 5). It is the graph
`sig` links: M0 proved in it that Servo and Wasmtime coexist (decision 26), and decision 4's
single libwayland holds in it.

### Three kinds of contract

| Contract | What it is | How stable | Can a repository boundary cross it? |
|---|---|---|---|
| Rust types | `sighurt-api`, plus `TargetLease`, `FramePermit`, `GlHandles` and `ShmFrame` from `sighurt-wayland` | Internal, rebuilt with the workspace (§5). Nothing is `#[non_exhaustive]`, and each id kind is one process-wide counter (decision 9) | Only with lockstep releases: two api versions in one build are two incompatible sets of types |
| The `"oxide"` Wasm ABI | The engine registers 211 host functions; the SDK imports 210 of them | Permanent (CLAUDE.md); guest-interface breaks are versioned on their own (§15) | Yes |
| Text formats | Configuration version 1, the session file, and `sig`'s diagnostics, which bench parses | Versioned, or local to one machine | Yes |

### Who works on what

| Contributor | Needs | Never compiles |
|---|---|---|
| Writes a keymap or theme | `config.toml` and the command names; no crate | anything |
| Forks the chrome | `sighurt-ui` (api, cosmic-text, tiny-skia, unicode-segmentation; 62 tests and golden images with a bundled font, no compositor). `sig` uses 14 concrete chrome items and no trait (`Chrome`, `CpuPainter`, `Snapshot`, `TabSnapshot`, `UiAction`, `Colors`, …), so a fork keeps them or edits `app.rs` (about 1 400 lines). | Servo, Wasmtime |
| Adds an engine (WPE WebKit next) | `EngineRuntime` (api), the four wayland types, a feature on `sighurt-browser` and about 5 `cfg` sites in `registry.rs` (`sig` has 20 such sites today). It probably also needs api or wayland changes: §23 says to extend the api where a new engine shows a missing concept, and a zero-copy dma-buf path is deferred (§22). The dominant cost is WebKit's own native build, wherever the crate lives. | the other engines, with `--no-default-features --features engine-wpe,engine-demo` |
| Writes Wasm guests | `sighurt-sdk` (no dependencies), or upstream `oxide-sdk`. The SDK isn't published, so guests depend on it by path from a clone of this repository, and running them needs `sig` with the Wasm engine (549 packages). | Servo |

## Pain points

Roughly in order of cost:

1. **Workspace-wide commands reach Servo.** The root is a virtual workspace with no
   `default-members`, so a plain `cargo build` or `cargo test` there builds every member, Servo
   included. `--workspace --exclude sighurt-engine-servo --exclude sighurt-browser` still builds
   Servo through the two experiments. `ci.yml` excludes them too, and CLAUDE.md's "before a
   commit" commands were corrected the same way during this run (uncommitted). A build that
   includes `m0-coexist` also unifies `testbinding` into `sig`'s Servo (M0.md, open problem 10).
2. **Servo's constraints apply to every crate.**
   - Dependencies are built at `opt-level = 3` (decision 6). A clean `sig` with the demo engine
     took 94 s, against 37 s with dependencies unoptimized (contributor analysis, this desktop,
     one run each).
   - `rust-version` is 1.95 (decision 7), chosen for louise-pc's toolchain. With the pinned
     nixpkgs both machines now compile with 1.98.1, so nothing checks 1.95 any more
     ([`WPE.md`](WPE.md), open problem 2).
   - No broad `cargo update`.
   - Any `Cargo.lock` change, even one for the axum backend, triggers `servo.yml`.
3. **The Wasm engine is heavy for what most work on it touches.** It pulls in 549 packages, and
   its crate alone takes 35 s to compile from scratch and 4.3 GB of the desktop's incremental
   cache. Without webrtc, ffmpeg, nokhwa, gilrs, cpal, rodio, tokio-tungstenite, rfd and
   starship-battery its graph would be 317 packages (build analysis).
4. **One Nix shell for every footprint.** `shell.nix` serves core work, Servo, the Wasm engine
   and now WPE WebKit. The uncommitted WebKit work pins nixpkgs, as §21 asks, and adds the WPE
   prefix only when it exists, leaving the shell identical otherwise. §21 suggests a fast shell
   and an engine shell "if the dependency footprint warrants it"; for an outside contributor who
   only wants the core and the chrome, it already does. Entering nix-shell costs about 2.2 s per
   `tools/cargo` call (build analysis), as long as a whole demo rebuild.
5. **Things that ride along in every diff.** The integration commit aecc3e1 changed 192 files:
   123 under `bench/` (98 of them result JSON) and 11 in the experiments, which have to follow
   every api change or stop compiling.
6. **Release and versioning tooling from the old layout.** `release.yml` builds `sig` and
   `sig-wasm` with `-p sighurt-ui -p sighurt-engine-wasm` and targets macOS. Its `sed` of
   `version = "…"` matches nothing, now that the crates use `version.workspace = true`. It says
   "the browser, the WASM engine and the guest SDK are versioned together", which §5 and §15
   contradict. `sighurt-sdk` already has a `version` of its own, currently equal to the
   workspace's 0.7.0.
7. **Nothing checks that the SDK and the engine agree.** The engine registers
   `api_video_set_pip` (a no-op in `video.rs`), and the SDK has no import for it. No test
   compares the two lists.
8. **CI has never run on this layout.** The `rewrite` branch hasn't been pushed: origin has only
   `main`. Both workflows install `stable`, while the workspace declares 1.95.
9. **Disk.** `target/` on this desktop is 36 GB, with ten separately fingerprinted builds of
   Servo's `script` crate (build analysis).

The number of product crates is not a pain point, and neither are the fast loops they give.
Measured on this desktop, debug builds with dependencies already built (build analysis):

- an edit in the core or the chrome rebuilds that crate's tests in about 1 s;
- `sig` with the demo engine rebuilds in 2.2 s after an edit to `main.rs`, and in 3.8 s (6
  crates) after an edit to the api;
- with Servo and Wasm compiled in, an edit costs 10–14 s, most of it the link (an edit to
  `main.rs` takes 6.5 s with lld instead of 10.4 s).

## What changes together

**The api, the core and the engines.** No commit has touched `sighurt-api` since 0944db3. That
is only because the parallel tracks were told to add new items only, so changes queued up instead,
and each one crosses layers:

| Queued change | Decisions | Crates it touches |
|---|---|---|
| `NavigationCommitted` says what a commit did to history | 41, 55 | api, core, all three engines |
| A field identity in `TextInputState` | 44, 55 | api, wayland, core, ui, servo |
| A way for Servo to hand over links that the core routes to another engine | 55 | api, core, servo |
| A `Send` pixel handle split from `FramePermit` | 45 | wayland, wasm, demo, sig |
| Requests for `<select>`, dialogs and HTTP authentication | 16 | api, core, servo, sig |
| The Wasm engine's service bridge | 25 | wasm, core, sig |

The first of these is being made during this run, uncommitted: a `history: Option<HistoryEffect>`
field on `NavigationCommitted`, with matching edits in the core and in all three engines.
`ViewId::serial()` was added for `sig`'s diagnostics the same way. Inside one workspace, each is
a single commit, and `cargo check` shows every user that broke. Across repositories, each would
be a release of the api, then an update of every engine, then one of `sig`, in that order.

**The chrome, the core and `sig`.** The chrome's input is a `Snapshot` that `sig` builds from
the core's `TabSummary`, so every new fact about a tab touches three crates. Every chrome decision
so far crossed that line:

- decision 18 (status strip): ui, wayland, sig;
- decisions 19 and 40 (command line, who owns focus): ui, core, sig;
- decision 21 (colours): ui, core, sig;
- decisions 46 and 47 (what the strip shows first): sig, ui.

**The SDK and the Wasm engine.** Of the 149 commits before the rewrite (merges excluded, up to
359cb36), 45 touched the SDK's sources. 42 of those also touched the host side, and 41 touched
files that now live in `sighurt-engine-wasm`. Only 3 changed the SDK alone. Adding a host
function means editing both.

**The two halves of the Wasm engine.** Only 7 of its 35 source files (3.3k of 16.9k lines) use
`sighurt-api` or `sighurt-wayland`. The rest is the host functions, the scheduler and pools, the
display list and text, none of which sees the browser. Decisions 42, 43 and 45 still changed
both halves at once.

**bench and `sig`'s diagnostics.** The diagnostics format and its parsers in
`bench/benchlib/parsers.py` changed together (decisions 47 and 49).

## Options

Each option below gives what it gains and what it costs through each of the three lenses. They
are not exclusive: B can be combined later with C, D or E, once their triggers hold.

### A. Keep the current structure

22 members, one repository, one lockfile.

| Lens | For | Against |
|---|---|---|
| Maintenance | Api changes are atomic. One diff shows every layer, and three of §28's five review questions (field ownership, late events, renderer leaks) are about several crates at once. `tools/remote`, `headless.sh` and bench's `git archive` work on one tree. | 13 members that aren't the product. The experiments follow every api change. bench JSON is most of some diffs. |
| Contributors | One clone gets the whole browser through path dependencies. | Servo's pins (lockfile, `rust-version`, `opt-level`) apply to people who never build Servo. The chrome's contract is implicit. |
| Build and CI | The one lockfile is the graph that ships. `-p` and features already keep Servo out of chrome, core, demo and guest builds. One `target/` shares the crates Servo and Wasm have in common. | Plain root commands build Servo. Exclusion lists in CI and CLAUDE.md have to agree, and they have disagreed. A guest-side lockfile change triggers `servo.yml`. |

### B. One repository, a tidier workspace (recommended)

The nine product crates stay as they are. Around them:

1. **`default-members`**: api, core, wayland, ui, engine-demo and sdk. A plain `cargo build`
   or `cargo test` at the root then never builds Servo or Wasmtime; `--workspace` still means
   everything. CI's main job and CLAUDE.md's "before a commit" commands could drop `--workspace`
   and their exclusion lists, adding the Wasm engine and `sig` without Servo as explicit `-p`
   jobs, as `ci.yml` already does for `sig`. A new engine (WPE) would then need no new exclusion
   anywhere. Whether the Wasm engine belongs in the default set is open: its tests need the
   guests built first.
2. **The experiments: tag and delete them** once `sig` with Servo reproduces their evidence:
   zero readbacks, no chrome paints during page animation, an idle loop that sleeps, the River
   run. M7 at the latest. M0.md, M1.md and STATUS then name the tag, as was done with
   `gpui-baseline` (decision 8). Until then they stay members. M0's evidence is that the engines
   link "from the root lockfile with no version change", which would no longer hold as written
   for an experiment with a lockfile of its own. Deleting them doesn't remove Servo's
   `testbinding` build: the Servo adapter's tests enable it too.
3. **A test that the engine's registered host functions and the SDK's imports are the same set**,
   with any exceptions listed.
4. **The SDK versioned on its own** (§5, §15). `release.yml` gets rebuilt around `sig` when CI
   and the documents are updated at M7 (§23).
5. **A light shell and an engine shell** (§21). The two shells must give the crates they share
   (wayland-sys, smithay-client-toolkit, fontconfig) the same `PKG_CONFIG_PATH`, or use separate
   target directories. Otherwise switching between them reruns those build scripts and rebuilds
   everything above them ([`WPE.md`](WPE.md), "Why not `PKG_CONFIG_PATH`").
6. **A note that `tools/heavy`, `tools/remote` and `tools/cargo` are the maintainers' machine
   tools**; outside contributors can use plain cargo.
7. Optional, later: keep only bench summaries tracked if the results grow.

| Lens | For | Against |
|---|---|---|
| Maintenance | Everything A has, without the members that rot. Fewer places to keep in agreement. | The docs that point at experiment paths have to name the tag. |
| Contributors | `cargo test` in a fresh clone is the fast set. The SDK's version and its consistency test make the guest contract explicit. | Still one clone pinned by Servo. |
| Build and CI | Accidental Servo builds and `testbinding` unification go away. Adding engines no longer means editing exclusion lists. | A default-members list to keep current. |

Considered and not recommended: turning the 10 guests into `[[example]]` targets of the SDK. That
would remove 10 members, but only cosmetically: the guest artifacts move to `…/release/examples/`,
which the engine's tests, `headless.sh` and CI would have to follow, and the one-crate app template
that guest authors copy is gone.

### C. The guest side apart (SDK and examples)

**C1, its own workspace inside this repository** (with its own lockfile):

| Lens | For | Against |
|---|---|---|
| Maintenance | Guest changes never touch Servo's lockfile, and §5's promise that the SDK is independent becomes structural. Host functions still change in one commit. | A second lockfile and `target/` to keep. |
| Contributors | Guests stop inheriting the Servo-seeded lockfile and profile. | The engine's tests, `headless.sh` and CI need guest paths from the sibling workspace, or checked-in `.wasm` files with checksums as `bench/workloads/wasm` has. |
| Build and CI | Guest-only changes don't trigger `servo.yml`. The guests' own target is about 1.3 MB (build analysis). | One more build directory in each job that needs both. |

**C2, its own repository:**

| Lens | For | Against |
|---|---|---|
| Maintenance | The one promised contract gets its own versions and releases. | Most guest features become two pull requests: 42 of the 45 SDK commits before the rewrite also changed the host. The engine and the SDK can drift, and a consistency test would then span repositories. |
| Contributors | Guest authors don't clone a Servo-pinned repository. | Publishing `sighurt-sdk` to crates.io from this repository gives them most of that, without the split. |
| Build and CI | A small wasm32-only CI; the SDK's job is already separate. | Engine tests need pinned prebuilt guests. |

Little is gained now either way: the SDK and examples haven't changed during the rewrite, and
M5's compatibility sample relies on that.

### D. Engines in their own repositories, with api and wayland as a published "host kit"

| Lens | For | Against |
|---|---|---|
| Maintenance | Isolated history and CI per engine, a clearer MPL boundary for Servo, and room for outside maintainers. | The repositories depend on each other in a cycle: an engine needs api and wayland, and `sig` needs the engine. The api becomes a versioned cross-repository contract, which §5 rules out. Each queued change above becomes 3–4 pull requests that must land in order. |
| Contributors | An engine repository could have its own toolchain, profile and shell. | Adding an engine still means editing `sig`'s registry: there is deliberately no plugin mechanism (§1). WebKit's native build costs the same wherever the crate lives. |
| Build and CI | Path filters already separate the engines' CI (`servo.yml`). | No gain for `sig`, which still compiles 935 packages. An engine repository's lockfile never ships, so two lockfiles must agree on Servo's roughly 600 packages, or Servo rebuilds (572 s in release on louise-pc for the baseline). The single libwayland, the `links` check and the process-wide setup (decision 26) can only be verified in `sig`'s graph. |

This is the baseline's layout without the IPC that made it work: before the rewrite, Servo had
its own workspace and lockfile (836 packages, beside the root's 1 026) and was reached over
`sighurt-ipc`. Decisions 1, 4, 5 and 8 removed it. A split starts to pay when an engine runs in
a worker process behind a versioned protocol (§18, and "Engine processes" in §29): it then has
its own binary, graph and lockfile.

### E. The chrome (`sighurt-ui`) in its own repository

| Lens | For | Against |
|---|---|---|
| Maintenance | Its boundary is already the cleanest: api only (decision 10), tests without a compositor. | Every chrome decision so far also changed the core or `sig` (18, 19, 21, 40, 46, 47). The api would need versioning, as in D. Nothing else uses the chrome, and the tab strip and `:command` line are Sighurt's own. |
| Contributors | A chrome fork would be a small repository. | `sig` uses 14 concrete chrome items, so a fork means two repositories and a `[patch]` override. Documenting those 14 items gives a forker more. |
| Build and CI | None beyond what `-p sighurt-ui` already gives: 58 packages, tests in about 1 s, no Servo. | A second CI and release for a crate that builds in seconds. |

### Smaller changes looked at

- **The Wayland host as a reusable library.** It is the most reusable piece (target leases,
  layout transactions, bounded SHM buffers, xkb translation, text-input-v3). But its types are
  part of the engine contract and it uses the api's ids, geometry and input, so publishing it
  would mean a semver promise on both.
- **bench in its own repository.** It already treats `sig` as an artifact and has no cargo
  cost, and moving it would take 228 files and most of the diff noise out. But the diagnostics
  format and its parsers change together, STATUS links its evidence into `bench/results`, and
  the harness hash covers `tools/heavy` and `tools/headless-sway`.
- **The Wasm engine as a guest runtime crate and an adapter crate**, along the 7-of-35-files
  line above. This is the step that would come before any guest-side split, or before running
  guests without the browser. It isn't worth doing without such a reason, since decisions 42,
  43 and 45 changed both halves.
- **The Wasm engine's heavy subsystems behind features** (media, RTC, devices, WebSocket,
  dialogs), on by default in `sig`. That takes its graph from 549 to 317 packages for work that
  doesn't need them. With a feature off, its host functions stay registered and fail closed, so
  compiled guests still instantiate (§15: errors, not false success). The cost is a feature
  matrix in CI and stub functions.
- **Link and shell overheads**: lld saves about 4 s per Servo link (whether switching forces one
  full rebuild is unverified), and a cached nix-shell environment would save 2.2 s per call.
  These have nothing to do with the layout.

### Merging crates: none pays

| Merge | What it would lose |
|---|---|
| api into core | Engines and the chrome would see browser policy and pull in serde, toml and url, and every core edit would rebuild the engines. |
| ui into sig | Decision 10 (a chrome tested without a compositor). The chrome's tests would go from 58 packages to 206, and the clean place to fork it would be gone. |
| ui with wayland | libwayland in the chrome's tests (decision 10). |
| core into sig | The core's tests would compile 206 packages instead of 43, and §26's "core tests require no windowing or engine initialization" would rest on discipline. |
| demo into sig or wayland | The demo engine would no longer test the engine boundary, since it could reach `sig`'s internals, and it would stop being the template of an engine crate. Neutral for the build (0.7 s, build analysis). |
| wayland into sig | Not possible: engines need `TargetLease`. |
| sdk into the Wasm engine | Different target; the SDK promises zero dependencies. |

## What to decide now

Nothing about repositories. If something should go on record anyway, it would be two
principles:

- **Everything compiled against `sighurt-api` lives in this repository**: api, core, wayland, ui,
  `sig` and every engine adapter, the coming WPE adapter included. WebKit itself stays an
  external native dependency, as Servo's crates are.
- **A repository boundary goes only where the contract isn't Rust types**: the `"oxide"` ABI, a
  future engine worker protocol, or the diagnostics and configuration formats.

The cheap, reversible items of option B could be done by the owners of those files: the root
`Cargo.toml`, CI, CLAUDE.md and `shell.nix` all belong to the orchestrator or to other tracks.

- `default-members`, the most useful of these: it ends accidental Servo builds and, once CI
  and CLAUDE.md use it, the exclusion lists.
- The SDK–engine consistency test.
- The policy for the experiments.
- A note on which tools are machine-specific.
- The light shell, under the `PKG_CONFIG_PATH` condition in option B.

For WebKit specifically:

- No `build.rs` builds WebKit, and no per-push CI job builds it. The WebKit track's prefix
  outside the Nix store already works that way.
- Its adapter crate lives in this repository. Its build script needs the prefix (it reads
  `SIGHURT_WPE_PKG_CONFIG_PATH`, [`WPE.md`](WPE.md)), so the crate stays out of
  `default-members` and out of workspace-wide commands until the prefix exists wherever those
  commands run.

## Later, and what reopens the question

| Change | Revisit when |
|---|---|
| Experiments tagged and deleted | `sig` with Servo reproduces M1's checks; M7 at the latest |
| SDK and examples in their own workspace (C1) | A guest-side change touches Servo's lockfile, or the SDK gets published |
| SDK and examples in their own repository (C2) | The `"oxide"` ABI gets a frozen version and SDK-only commits come to outnumber SDK-and-host ones (today 3 of 45). Before that, publish the SDK from here when the first outside guest author needs it |
| Wasm engine split into runtime and adapter | Guests need to run without the browser, or syncing with upstream Oxide becomes a goal |
| Wasm subsystems behind features | The first CI run of the rewrite shows the Wasm graph dominating, or M6's service bridge reworks those host functions anyway |
| An engine in its own repository (D) | It runs in a worker process with a versioned protocol; or it can't share `sig`'s lockfile or `rust-version` (a Servo 0.6 upgrade, a conflict with WPE's system libraries); or an outside maintainer keeps it, and the api has gone two milestones without a breaking change |
| The chrome in its own repository (E) | A second chrome selected by a feature, another project wanting `sighurt-ui`, or chrome changes no longer touching the core or `sig` (fewer than 1 in 5 of the next 30) |
| The Wayland host published | An outside project wants to depend on it |
| bench, or only its results, elsewhere | M7 is done, or the tracked results grow uncomfortably large |
| lld, a cached shell | Whenever the link or the shell wait gets annoying |
| The whole question | M7: api changes stop being queued, `bench/build-cost` re-measures against the baseline's 16 minutes, and §23 already asks for README, DOCS, CLAUDE.md, CI and Nix to be updated then |

One measure to check at each milestone: the share of commits that touch `sighurt-api` or
`sighurt-wayland` together with an engine or `sig`. Once it falls under about 10 %, splitting
gets cheap. Today, every queued api change is such a commit.

## Recommended direction

Keep one repository, and inside it the nine crates of §5. Take option B as the direction: fence
the workspace so plain commands stay fast, delete the experiments once `sig` carries their
evidence, make the SDK's contract explicit with its own version and a consistency test, and
give outside contributors a light Nix shell that keeps the shared build inputs identical. Add the
WPE adapter as one more crate in this repository, with WebKit built outside cargo. Option C1 is
the first split worth making, and only when its trigger holds. Engines and the chrome stay here as long as they share `sig`'s process and
`sighurt-api` keeps changing. For the engines, the question changes with §18's worker process,
not with any build or repository setting.

This is a recommendation. The user asked to think about it, not to decide it, so nothing above
is in the decisions log.

## Where the analyses disagreed

| Point | The analyses said | The tree says |
|---|---|---|
| Packages per crate | Maintenance: wayland 47, ui 57, wasm 519, servo 608, `sig` 893. The others: 51, 58, 549, 645, 935 | Both are right. Maintenance counted normal edges only, the others normal and build edges; recounted above with both. |
| How often the SDK changes with the host | Maintenance: 39 of 53 SDK commits (74 %). Contributors: 42 of 45 | Of 149 non-merge commits up to 359cb36, 45 touched SDK sources and 42 of them the host side (41 counting only files now in `sighurt-engine-wasm`). The 74 % counted every commit touching the SDK's directory (54 here) against a narrower set of host files; its 39 wasn't reproduced. Either way, most SDK changes are host changes. |
| Host-function counts | Maintenance: SDK 211, engine 212 | Those are string counts. The engine registers 211: its 212th name is `api_ui_button`, in a test asserting it is *not* registered. The SDK imports 210: its 211th is `"api_name"`, in a doc comment of the `host_fns!` macro. The one registered function the SDK lacks is `api_video_set_pip`. |
| bench in the integration commit | Maintenance: "bench JSON is 123 of 192 files" | 123 files under `bench/`, 98 of them JSON. |
| CLAUDE.md is stale and its commands build Servo | Maintenance and build: yes | True of the committed CLAUDE.md. During this run the working copy was corrected (uncommitted): the experiments are excluded, and the Wasm engine is described as a library. |
| The Wasm engine's non-api half is "host functions tied to the SDK" | Maintenance | 7 of 35 files use the api or wayland, as stated, but the other 28 also hold the scheduler, the pools, the display list and text, not only host functions. |
| Rebuild time after an api edit | Maintenance: 2.0 s. Contributors: 6.2 s. Build: 3.8 s | Different commands: maintenance ran `check` of `sig` with the demo and Wasm engines (7 crates rechecked); the other two built `sig` with the demo engine, at different moments. All are seconds; none was re-measured here. |
| The nixpkgs pin in `shell.nix` | Contributors: a cost, since it "pins nixpkgs for everyone" | Pinning is what §21 asks for. Without the WPE prefix, the shell on this desktop is bit-identical to before. The real costs: louise-pc moved from Rust 1.95 to 1.98.1, so its Servo build is redone once, and nothing checks `rust-version` 1.95 any more ([`WPE.md`](WPE.md)). |
| Taking the experiments out | Build: now, as their own workspaces or at a tag. Maintenance and contributors: at M7, behind a tag | As their own workspaces they would have lockfiles of their own, and M0's "from the root lockfile" evidence would no longer apply as written. A tag once `sig` reproduces the evidence keeps it exact. |
