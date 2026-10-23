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
