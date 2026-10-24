# Persistent logins / site data: proposed design (deep-design agent, 23 Oct 2026)

User request: "make sure to also look into getting logging in be persistent. Currently I have to log into github each time I search for it. See if that can be fixed in some nice way. The active engine fetches requests to store or access information and the browser decides what to do with those requests. Modularity is the key design we are going for. And make sure it is privacy first and totally safe."

## Root causes found
- WPE: cookies never persisted: engines/sighurt-engine-wpe/src/runtime.rs:1405-1419 creates `webkit_network_session_new(data, cache)`, no `webkit_cookie_manager_set_persistent_storage` (not bound in ffi.rs:453-457). WebKit keeps cookies in memory by default.
- Servo 0.7: cookies/HSTS/auth read at startup (servo-net-0.7.0/resource_thread.rs:211-214), written only at shutdown (Drop for ServoInner, servo-0.7.0/servo.rs:879 → Exit → resource_thread.rs:665-673; servo-base-0.7.0/lib.rs:60-74 File::create + write: non-atomic, panics on failure). Every abort (3 of the user's runs today: a WebGL panic), decision 54's leak + `_exit` (crates/sighurt-runtime/src/session.rs:693-716), SIGHUP, SIGKILL and a second `sig` (profile lock → no profile) lose all cookies of the run. Servo also serializes session cookies and never clears them (over-retention). Profile files are 0644/0755.
- The user's saved Servo jar has never held a GitHub login cookie (user_session / __Host-user_session_same_site, ~2 weeks).

## Verdict: shape C (hybrid). Engines keep their own jars, crash-safe inside the engine; the browser owns the policy through one small engine-neutral site-data API keyed by registrable domain. Reject B (browser-owned cookie store): plan §16 (line 615) says the core does not reimplement cookies; WebKit's cookies live in its network process with no per-cookie hook; Servo's HTTP Set-Cookie bypasses the resource thread loop (no hook; NewCookieListener is a TODO, resource_thread.rs:609-611); SiteDataManager snapshots are lossy (site_data gives eTLD+1 names only; cookies_for_url misses host-only cookies on subdomains and path-restricted ones). The user's "engine requests, browser decides" is honoured at site granularity, not per cookie.

## API
- sighurt-api engine.rs: `EngineCapabilities.site_data: bool`; runtime-level `fn site_data(&mut self, request: SiteDataRequest) -> Result<(), EngineError>` with a default `Unsupported` body (runtime-level: must run with no view open). `enum SiteDataRequest { List { request: RequestId }, Forget { site: String }, ForgetAll, EndSession }`, `site` = registrable domain; Forget covers cookies, storage, caches, credentials; EndSession drops session cookies/sessionStorage. `EngineEvent::SitesListed { request, sites: Vec<String> }` (runtime-level), names cut to the 16 KiB text bound, never logged.
- sighurt-core: `[privacy]` table: `keep = "all" | "kept"` (default "all"), `sites = [...]` (kept sites, host patterns as in [routing.*] hosts), `third-party-cookies = "block" | "allow"` (default block; WPE honours, Servo can't → notice). Live. Commands `site.keep [host]`, `site.forget [host]`, `site.forget-all` (no argument = the selected tab's site). site.keep/forget persist to `{state_dir}/sighurt/sites.toml` (Effect::SaveSites, atomic, 0600), merged with the config list; site.forget purges. `Effect::SiteData { engine: Option<String>, request }`. Pure logic: at Start, EndSession to all; in keep="kept", List at start, then Forget every site not kept, same at Quit best-effort.
- sighurt-runtime: fans Effect::SiteData out over the registry's runtimes (starting a lazily started engine for Forget, or recording it for its start); routes SitesListed as BrowserEvent::Engine; writes sites.toml; creates data/state dirs 0700 and fixes existing ones once.
- Adapters: Servo: List → site_data(Cookies|Local), Forget → clear_site_data, ForgetAll → clear_cookies + per-site storage clears, EndSession → clear_session_cookies (servo-0.7.0/site_data_manager.rs:134-279). WPE: webkit_website_data_manager_fetch/remove/clear + webkit_cookie_manager_* (new ffi). Demo engine: in-memory set for tests.

## Defaults
First-party persistent cookies and site data of every site survive runs and crashes; session cookies die with the process (RFC 6265 §5.3); third-party cookies blocked where the engine can; site.forget for regrets; keep="kept" = strict mode. Not defaulting to "kept": it logs the user out of everything not named (the complaint), no prompt UX exists, engines can't tell login cookies from consent cookies.

## Servo: vendored servo-net patch
Crash safety needs writes during the run; SiteDataManager can't enumerate all cookies. Vendor servo-net 0.7.0 under third_party/servo-net via [patch.crates-io] (precedent: surfman, Cargo.toml:99-100), ~60-line patch in resource_thread.rs: a persistence thread per public HttpState that every 5 s serialises cookie_jar (persistent cookies only, cookie.rs:38), hsts_list and auth_cache if changed, to {dir}/*.json.tmp mode 0600 then rename; the Exit arm reuses it; private HttpState never written. Cost: a Servo rebuild (script, constellation, libservo depend on net) and re-applying at each Servo bump; upstreamable. Fallback (not recommended): adapter journal via visited hosts × cookies_for_url_async (lossy, second copy of cookie values).

## Crash safety
Servo ≤5 s of cookies lost on abort/_exit/SIGKILL/SIGHUP; files always whole; auth_cache.json 0600. WPE: set_persistent_storage({profile}/cookies.sqlite, SQLITE) on persistent sessions only; WebKit writes on change.

## Security/privacy
No Sighurt code touches cookie values; policy acts on site names; only Servo's own serialisation, made earlier and atomic. 0700 dirs, 0600 files. No encryption at rest yet (no keyring; a key would sit beside the data; Firefox/Chromium on Linux the same) — later decision. Site lists never logged.

## Concurrent instances
Phase 1: second sig still gets no profile (decision 54) but a notice says so. Phase 3: follower profile (copy cookie files into scratch; logged in, writes discarded).

## Migration
EndSession at first start removes stored session cookies (_gh_sess); one-time chmod; no format changes.

## Tests
Vendored writer atomic/0600/no session cookies; Servo child-process test (persistent + session cookie, SIGKILL after 6 s, file holds only the persistent one, a second run sends it); WPE headless.sh kill -9 test; core [privacy]/site.* tests; runtime + demo fan-out; engines.sh "login survives kill -9" on both engines.

## Phases
1. Fix now, no API change: WPE persistent SQLite cookies; vendored servo-net incremental atomic writer; 0700/0600; EndSession at start via SiteDataManager.
2. site_data API, [privacy], site.* commands, sites.toml, demo tests.
3. Follower profile; keyring/encryption decision; upstream the patch.

## Uncertainties
WebKitCookieManager `changed` signal and WPE's default NO_THIRD_PARTY accept policy from docs memory; WebKit's file mode for cookies.sqlite; cookies_data_for_url subdomain behaviour; Servo rebuild time.

## Review (GPT-6 Astra, 24 October 2026)

Phase 1 below is revised to follow it.

**Verdict: shape C is right, but the design needs revision before implementation.** Keep the approved vendoring and `keep = "all"`. Engine-owned jars fit the architecture; browser-owned cookies and URL-snapshot journals would duplicate or lose cookie semantics. Phase 1 should establish reliable engine-native persistence. The later API can manage retention and deletion; it cannot provide per-access authorization with the inspected APIs.

1. **High — The new writer would outlive its profile lock after a crash.** Servo’s failure path leaks Servo, but the registry subsequently drops the adapter and its lock before `_exit` ([Servo runtime:1696](/home/gusahlg/repos/sighurt/engines/sighurt-engine-servo/src/runtime.rs:1696), [registry:557](/home/gusahlg/repos/sighurt/crates/sighurt-runtime/src/registry.rs:557)). A second process could acquire the profile while the leaked writer still writes. **Fix:** retain the lock until process termination on this path; on normal shutdown, stop and join the writer before releasing it. WPE’s fixed teardown iterations likewise do not prove its network process has stopped writing ([WPE runtime:1893](/home/gusahlg/repos/sighurt/engines/sighurt-engine-wpe/src/runtime.rs:1893)).

2. **High — “Atomic” and “≤5 seconds lost” are underspecified.** Servo’s existing Exit write truncates files; buffered flush errors can go unchecked ([resource_thread:665](/home/gusahlg/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/servo-net-0.7.0/resource_thread.rs:665), [servo-base:67](/home/gusahlg/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/servo-base-0.7.0/lib.rs:67)). Reusing a helper does not serialize competing writers. **Fix:** one writer owns periodic and final commits: exclusive same-directory temporary file, complete write/flush, file fsync, rename, directory fsync. Retain dirty state after failure, retry, and report degraded persistence without panicking. Five seconds is a target subject to scheduling/I/O, not a bound during disk failure.

3. **High — Filtering only `persistent` leaves expired/deleted secrets on disk.** Servo’s session/site deletion marks cookies expired rather than removing them ([cookie_storage:102](/home/gusahlg/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/servo-net-0.7.0/cookie_storage.rs:102)). **Fix:** snapshot only persistent, unexpired entries; preserve host-only, path, security flags and absolute expiry ([cookie:31](/home/gusahlg/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/servo-net-0.7.0/cookie.rs:31)). Never reconstruct cookies and restart Max-Age. Remove legacy session entries before navigation and durably sanitize the old file. Defining a session as one process lifetime is reasonable, but is Sighurt policy, not an [RFC-mandated boundary](https://www.rfc-editor.org/rfc/rfc6265.html#section-5.3).

4. **High — Servo’s privacy limitation is worse than unsupported third-party blocking.** SameSite enforcement and partitioning are explicitly TODO ([cookie:379](/home/gusahlg/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/servo-net-0.7.0/cookie.rs:379)); HTTP cookie handling bypasses the resource-message loop ([http_loader:2440](/home/gusahlg/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/servo-net-0.7.0/http_loader.rs:2440)). Persistence extends these existing risks across runs. **Fix:** expose unsupported protections honestly and resolve enforcement before claiming “privacy first.” Keep-all retention can coexist with actual cross-site blocking. In WPE, set policy explicitly and account for ITP overriding `NO_THIRD_PARTY` ([installed GIR:3323](/home/gusahlg/.local/share/sighurt/deps/wpewebkit-2.52.6/share/gir-1.0/WPEWebKit-2.0.gir:3323)).

5. **High — Credential handling contradicts the security rules.** Servo logs complete outgoing cookies at info level and cookie values at debug level ([cookie_storage:243](/home/gusahlg/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/servo-net-0.7.0/cookie_storage.rs:243), [cookie:338](/home/gusahlg/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/servo-net-0.7.0/cookie.rs:338)); HTTP debug logs also dump headers. Its auth cache serializes plaintext usernames/passwords ([resource_thread:689](/home/gusahlg/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/servo-net-0.7.0/resource_thread.rs:689)). WPE persistent credential storage defaults enabled ([GIR:10725](/home/gusahlg/.local/share/sighurt/deps/wpewebkit-2.52.6/share/gir-1.0/WPEWebKit-2.0.gir:10725)). **Fix:** remove secret logging and make HTTP auth memory-only by default. Treat HSTS separately: retain transport protection normally, define deletion of dynamic entries explicitly, preserve preloads.

6. **High — Permission migration needs a protocol.** I confirmed current profile directories are 0755 and Servo JSON files 0644. “One-time chmod” ([design:34](/home/gusahlg/repos/sighurt/docs/rewrite/site-data.md:34)) omits ordering, failures, temporaries and SQLite sidecars. **Fix:** secure owned profile paths before loading secrets or starting engine processes; migrate under the profile lock. Create files securely from the outset, reject unsafe existing temporary paths, and cover database journals/WAL, storage and cache directories. Do not recursively chmod shared XDG directories. Also replace startup read panics with bounded, fallible recovery ([servo-base:48](/home/gusahlg/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/servo-base-0.7.0/lib.rs:48)).

7. **High — `keep = "kept"` is cleanup, not strict non-retention.** Startup/quit sweeps ([design:14](/home/gusahlg/repos/sighurt/docs/rewrite/site-data.md:14)) allow unkept data onto disk throughout browsing and indefinitely after a crash. Lazy engines may never receive cleanup. **Fix:** describe that weaker guarantee accurately, or enforce exclusions when storing data/use ephemeral storage. WPE’s global persistence setting does not supply selective per-site persistence.

8. **High — `Forget` promises unsupported deletion and lacks completion semantics.** Servo’s manager covers cookies/localStorage/sessionStorage, excluding network state ([site_data_manager:20](/home/gusahlg/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/servo-0.7.0/site_data_manager.rs:20)). WPE deletion is asynchronous ([WebsiteDataManager.h:79](/home/gusahlg/.local/share/sighurt/deps/wpewebkit-2.52.6/include/wpe-webkit-2.0/wpe/WebKitWebsiteDataManager.h:79)); its `ALL` mask does not include HTTP credentials. **Fix:** capabilities by category, request IDs and completion/error results for every mutation. Quiesce live pages/workers, prevent stale snapshots restoring deleted cookies, and commit deletion before acknowledging it. Deferred lazy-engine deletion must survive restart and remain visibly pending. Clearing a lock-denied ephemeral instance does not clear the persistent profile.

9. **Medium — Host patterns and registrable-domain keys conflict.** Keeping `login.example.com` cannot match Servo’s `example.com` enumeration. WPE names may be hosts or “Local files” ([GIR:28513](/home/gusahlg/.local/share/sighurt/deps/wpewebkit-2.52.6/share/gir-1.0/WPEWebKit-2.0.gir:28513)). **Fix:** define canonical keys using public/private suffix rules, IDNA normalization and IP/localhost fallbacks; display deletion scope. Preserve native WPE records inside the adapter. Never truncate identity keys; bound total enumeration, not merely each string.

10. **Medium — Concurrent policy writes and follower profiles add unresolved complexity.** Atomic `sites.toml` replacement prevents torn files, not lost updates; union-merging config means `site.forget` cannot override a configured keep ([design:14](/home/gusahlg/repos/sighurt/docs/rewrite/site-data.md:14)). Locks are per engine, so persistence ownership can split between instances. **Fix:** define policy precedence and serialize updates; report persistence per engine. Defer follower copying: live SQLite requires a consistent snapshot, and discarded token-refresh writes can invalidate copied logins.

For **phase 1**, I would:

- Implement the single-owner Servo writer, lock-lifetime repair, persistent/unexpired snapshots, secure migration, recoverable I/O failures and secret-log removal. Keep auth memory-only. Snapshot briefly under locks; perform serialization and disk I/O outside them.
- Configure WPE’s actual session cookie manager before any view: SQLite persistence only for persistent sessions, explicit acceptance policy, explicit credential-storage policy ([NetworkSession.h:53](/home/gusahlg/.local/share/sighurt/deps/wpewebkit-2.52.6/include/wpe-webkit-2.0/wpe/WebKitNetworkSession.h:53)). WebKit already persists non-session cookies on changes; add no second writer ([GIR:3353](/home/gusahlg/.local/share/sighurt/deps/wpewebkit-2.52.6/share/gir-1.0/WPEWebKit-2.0.gir:3353)).
- Require tests for normal Exit, `_exit`, interrupted commits, ENOSPC/short writes/fsync failures, corrupt migration, second-instance locking, expiry/replacement/deletion and session exclusion. Kill WPE’s network process too. Replace the six-second sleep with observable commit evidence. Defer the generic API and follower profiles until these guarantees hold.
## Phase 1, as revised after the review

Servo (vendored `servo-net` 0.7.0 in `third_party/servo-net`, `[patch.crates-io]`):
- One writer owns every commit of cookies and HSTS (periodic, at most every 5 s when changed,
  and the final one at Exit; it is stopped and joined before the profile lock is released):
  a snapshot taken briefly under the locks, then outside them an exclusive temporary file in
  the same directory (0600), write, flush, fsync, rename, fsync of the directory. A failure
  keeps the state dirty, retries, logs once, never panics.
- Only persistent, unexpired cookies are written, with every attribute kept as stored
  (host-only, path, flags, absolute expiry). At load, session and expired entries are dropped
  and the file is rewritten clean.
- HTTP authentication stays in memory: `auth_cache.json` is no longer written, and an existing
  one is deleted.
- Servo's logging of cookie values and cookie headers is removed.
- A damaged file at load is set aside and logged, not a panic.

The Servo adapter keeps the profile lock until the process ends when Servo is leaked (decision
54's path), so no second instance writes while a leaked writer may still run.

WPE: before any view, on persistent sessions only, the network session's cookie manager gets
SQLite persistent storage in the profile, an explicit accept policy (no third party) and
persistent HTTP credential storage off.

Both: the profile directories (`{data_dir}/sighurt` and the engines' profiles, state dir) are
0700 and files written 0600, fixed under the profile lock before any engine reads them; no
recursive chmod outside Sighurt's own directories.

Not claimed: SameSite and partitioning in Servo 0.7 (TODO upstream); the later API's guarantees
(deletion with completion, canonical site keys, strict mode as cleanup) wait for phase 2.
