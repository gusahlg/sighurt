//! Builds NetSurf's core with this crate's frontend and links it statically.
//!
//! `SIGHURT_NETSURF_SRC` and `SIGHURT_NETSURF_PKG_CONFIG_PATH` come from shell.nix.
//! They are not `PKG_CONFIG_PATH`: Servo's build scripts track that and would rebuild
//! (docs/rewrite/NETSURF.md). This script sets `PKG_CONFIG_PATH` only in the make and
//! pkg-config processes it spawns.
//!
//! The unpacked tree and object files go under `$SIGHURT_NETSURF_BUILD`, else
//! `/data/sig-builds/netsurf` (the HDD, to keep the SSD free) when `/data/sig-builds` exists,
//! else in cargo's `OUT_DIR`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const STATIC_LIBS: &[&str] = &[
    "libcss",
    "libdom",
    "libhubbub",
    "libparserutils",
    "libwapcaplet",
    "libnsgif",
    "libnsbmp",
    "libsvgtiny",
    "libnsutils",
    "libnspsl",
    "libnslog",
    "libutf8proc",
];

const SHARED_LIBS: &[&str] = &[
    "libcurl",
    "libssl",
    "libcrypto",
    "freetype2",
    "libpng",
    "libjpeg",
    "libwebp",
    "expat",
    "zlib",
];

fn main() {
    for var in [
        "SIGHURT_NETSURF_SRC",
        "SIGHURT_NETSURF_PKG_CONFIG_PATH",
        "SIGHURT_NETSURF_PERL",
        "SIGHURT_NETSURF_XXD",
        "SIGHURT_NETSURF_GPERF",
    ] {
        println!("cargo:rerun-if-env-changed={var}");
    }
    println!("cargo:rerun-if-changed=csrc");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");

    // Record the fonts' store paths: `sig` runs outside the nix-shell too.
    for k in ["SANS", "SANS_BOLD", "SERIF", "MONO"] {
        let var = format!("SIGHURT_NETSURF_FONT_{k}");
        println!("cargo:rerun-if-env-changed={var}");
        if let Ok(v) = std::env::var(&var) {
            println!("cargo:rustc-env=SIGHURT_NETSURF_BUILT_FONT_{k}={v}");
        }
    }
    let src_tar = env("SIGHURT_NETSURF_SRC");
    let ns_pc = env("SIGHURT_NETSURF_PKG_CONFIG_PATH");
    let out_dir = PathBuf::from(env("OUT_DIR"));
    let tree = build_tree(&src_tar);
    let source = tree.join("netsurf-3.11");

    unpack(&src_tar, &tree, &source);
    install_frontend(&source);
    apply_patches(&source);
    let pc = prepend_pkg_config(&ns_pc);
    run_make(&source, &pc);
    let archive = archive_objects(&source, &out_dir);
    link(&archive, &pc, &out_dir);
    export_resources(&source, &out_dir);
}

/// The directory the NetSurf tree is unpacked and built in. It is named by the source tarball
/// (the hash in its nix store name) and by a hash of this crate's C sources and patches, so no
/// other checkout, worktree or edit shares objects with it: stale objects cannot be linked.
fn build_tree(tarball: &str) -> PathBuf {
    println!("cargo:rerun-if-env-changed=SIGHURT_NETSURF_BUILD");
    let base = if let Ok(dir) = std::env::var("SIGHURT_NETSURF_BUILD") {
        PathBuf::from(dir)
    } else if Path::new("/data/sig-builds").is_dir() {
        Path::new("/data/sig-builds").join("netsurf")
    } else {
        PathBuf::from(env("OUT_DIR")).join("netsurf-build")
    };
    let store_hash: String = Path::new(tarball)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("tarball")
        .chars()
        .take(12)
        .collect();
    base.join(format!(
        "{store_hash}-{:012x}",
        sources_hash() & 0xffff_ffff_ffff
    ))
}

/// FNV-1a over this crate's C sources and patches (names and bytes) and over this script.
fn sources_hash() -> u64 {
    fn feed(hash: &mut u64, bytes: &[u8]) {
        for &b in bytes {
            *hash ^= u64::from(b);
            *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    fn walk(dir: &Path, hash: &mut u64) {
        let mut entries: Vec<_> = fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(&path, hash);
            } else {
                feed(hash, path.to_string_lossy().as_bytes());
                feed(hash, &fs::read(&path).unwrap());
            }
        }
    }
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    walk(Path::new("csrc"), &mut hash);
    feed(&mut hash, &fs::read("build.rs").unwrap());
    hash
}

fn unpack(tar: &str, tree: &Path, source: &Path) {
    if source.join("Makefile").is_file() {
        return;
    }
    fs::create_dir_all(tree).unwrap_or_else(|e| panic!("mkdir {}: {e}", tree.display()));
    let status = Command::new("tar")
        .args(["xzf", tar, "-C"])
        .arg(tree)
        .status()
        .unwrap_or_else(|e| panic!("tar: {e}"));
    if !status.success() {
        panic!("unpacking {tar} failed");
    }
}

fn install_frontend(source: &Path) {
    let from = Path::new("csrc/frontend");
    let to = source.join("frontends/sighurt");
    let _ = fs::remove_dir_all(&to);
    copy_dir(from, &to);
    // The resources the message splitter writes into.
    fs::create_dir_all(to.join("res")).unwrap();
    let hts = source.join("frontends/Makefile.hts");
    let text = fs::read_to_string(&hts).unwrap();
    if !text.contains(" sighurt") {
        let next = text.replace(
            "VLDTARGET := amiga atari beos framebuffer gtk monkey riscos windows",
            "VLDTARGET := amiga atari beos framebuffer gtk monkey riscos windows sighurt",
        );
        if next == text {
            panic!("frontends/Makefile.hts has no VLDTARGET line to extend");
        }
        fs::write(&hts, next).unwrap();
    }
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &dest);
        } else {
            fs::copy(entry.path(), dest).unwrap();
        }
    }
}

/// Edits the unpacked tree. Idempotent: each patch leaves a marker.
fn apply_patches(source: &Path) {
    replace_once(
        &source.join("desktop/netsurf.c"),
        "SIGHURT PATCH: locale",
        "\tsetlocale(LC_ALL, \"\");\n",
        "\t/* SIGHURT PATCH: locale. sig shares this process; do not call setlocale. */\n",
    );
    replace_once(
        &source.join("utils/useragent.c"),
        "SIGHURT PATCH: user-agent",
        "static const char *core_user_agent_string = NULL;\n",
        "static const char *core_user_agent_string = NULL;\n\
/* SIGHURT PATCH: user-agent. [engines.netsurf] user-agent, if it set one. */\n\
static char *sighurt_user_agent = NULL;\n\
void user_agent_override(const char *ua)\n\
{\n\
\tfree(sighurt_user_agent);\n\
\tsighurt_user_agent = NULL;\n\
\tif (ua != NULL && ua[0] != '\\0')\n\
\t\tsighurt_user_agent = strdup(ua);\n\
}\n",
    );
    // One physical line: a `\` newline in a Rust string drops the next line's
    // leading spaces, and those spaces are the C function's indentation.
    replace_once(
        &source.join("utils/useragent.c"),
        "sighurt_user_agent != NULL",
        "const char *\nuser_agent_string(void)\n{\n        if (core_user_agent_string == NULL)\n                user_agent_build_string();\n\treturn core_user_agent_string;\n}",
        "const char *\nuser_agent_string(void)\n{\n\tif (sighurt_user_agent != NULL)\n\t\treturn sighurt_user_agent;\n        if (core_user_agent_string == NULL)\n                user_agent_build_string();\n\treturn core_user_agent_string;\n}",
    );
    // The override above already contains `free(sighurt_user_agent)`, so that
    // text cannot be this patch's skip-marker.
    replace_once(
        &source.join("utils/useragent.c"),
        "SIGHURT PATCH: free-user-agent",
        "\t\tcore_user_agent_string = NULL;\n\t}\n}",
        "\t\tcore_user_agent_string = NULL;\n\t}\n\t/* SIGHURT PATCH: free-user-agent. */\n\tfree(sighurt_user_agent);\n\tsighurt_user_agent = NULL;\n}",
    );
    // --- Security (docs/rewrite/NETSURF.md, "Security patches"). NetSurf lets a page, through
    // its own about:query forms, relax certificate checks and plant credentials; none of that
    // exists in sig. Verification is never relaxed, credentials never come from a page.
    let urldb = source.join("content/urldb.c");
    replace_once(
        &urldb,
        "SIGHURT PATCH: no-cert-permissions-set",
        "void urldb_set_cert_permissions(nsurl *url, bool permit)\n{\n",
        "void urldb_set_cert_permissions(nsurl *url, bool permit)\n{\n\t/* SIGHURT PATCH: no-cert-permissions-set. Verification is never relaxed. */\n\t(void)url;\n\t(void)permit;\n\treturn;\n",
    );
    replace_once(
        &urldb,
        "SIGHURT PATCH: no-cert-permissions-get",
        "bool urldb_get_cert_permissions(nsurl *url)\n{\n",
        "bool urldb_get_cert_permissions(nsurl *url)\n{\n\t/* SIGHURT PATCH: no-cert-permissions-get. */\n\t(void)url;\n\treturn false;\n",
    );
    replace_once(
        &urldb,
        "SIGHURT PATCH: no-auth-details",
        "void urldb_set_auth_details(nsurl *url, const char *realm, const char *auth)\n{\n",
        "void urldb_set_auth_details(nsurl *url, const char *realm, const char *auth)\n{\n\t/* SIGHURT PATCH: no-auth-details. A page never plants credentials. */\n\t(void)url;\n\t(void)realm;\n\t(void)auth;\n\treturn;\n",
    );
    let bw = source.join("desktop/browser_window.c");
    replace_once(
        &bw,
        "SIGHURT PATCH: internal-navigation",
        "\t\tif (post_multipart == NULL) {\n\t\t\treturn NSERROR_NEED_DATA;\n\t\t}\n\t\t/* It *is* internal, set it as such */",
        "\t\t/* SIGHURT PATCH: internal-navigation. Only the core navigates to about:query. */\n\t\treturn NSERROR_NEED_DATA;\n\t\t/* It *is* internal, set it as such */",
    );
    // Every failed load, a bad certificate and a login request included, is reported to the
    // frontend and fails; the core shows no error page, query page or login form.
    replace_once(
        &bw,
        "SIGHURT PATCH: failed-load",
        "\thlcache_handle_release(c);\n\n\tswitch (code) {\n\tcase NSERROR_BAD_AUTH:\n\t\tres = browser_window__handle_login(bw, message, url);\n\t\tbreak;\n\n\tcase NSERROR_BAD_CERTS:\n\t\tres = browser_window__handle_bad_certs(bw, url);\n\t\tbreak;\n\n\tcase NSERROR_TIMEOUT:\n\t\tres = browser_window__handle_timeout(bw, url);\n\t\tbreak;\n\n\tdefault:\n\t\tres = browser_window__handle_fetcherror(bw, message, url);\n\t\tbreak;\n\t}\n",
        "\t/* SIGHURT PATCH: failed-load. */\n\textern void nsh_load_failed(struct browser_window *, int, const char *, const char *);\n\tnsh_load_failed(bw, (int)code, message, nsurl_access(url));\n\thlcache_handle_release(c);\n\tif (bw->loading_content == NULL)\n\t\tbrowser_window_stop_throbber(bw);\n\tres = NSERROR_OK;\n",
    );
    // A GET form the user submits is asked about like a link (NavigationRequested).
    replace_once(
        &source.join("content/handlers/html/form.c"),
        "SIGHURT PATCH: form-get",
        "\t\t\t\tres = browser_window_navigate(target,\n\t\t\t\t\t\t\t      query_url,\n",
        "\t\t\t\t/* SIGHURT PATCH: form-get. */\n\t\t\t\textern int nsh_form_get(struct browser_window *, const char *);\n\t\t\t\tif (nsh_form_get(target, nsurl_access(query_url)))\n\t\t\t\t\tres = NSERROR_OK;\n\t\t\t\telse\n\t\t\t\tres = browser_window_navigate(target,\n\t\t\t\t\t\t\t      query_url,\n",
    );
    // A page's resources and redirects never reach file: from another scheme.
    replace_once(
        &source.join("content/llcache.c"),
        "SIGHURT PATCH: file-origin",
        "\tnsurl *hsts_url;\n\tbool hsts_in_use;\n\n\t/* Perform HSTS transform */",
        "\tnsurl *hsts_url;\n\tbool hsts_in_use;\n\n\t/* SIGHURT PATCH: file-origin. Only a file: page (or the browser itself, with no referrer) fetches file:. */\n\tif (referer != NULL && strncmp(nsurl_access(url), \"file:\", 5) == 0 &&\n\t    strncmp(nsurl_access(referer), \"file:\", 5) != 0)\n\t\treturn NSERROR_NO_FETCH_HANDLER;\n\n\t/* Perform HSTS transform */",
    );
}

fn replace_once(path: &Path, marker: &str, from: &str, to: &str) {
    let text =
        fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    if text.contains(marker) {
        return;
    }
    let count = text.matches(from).count();
    if count != 1 {
        panic!(
            "{}: expected one match for a patch ({marker}), found {count}",
            path.display()
        );
    }
    fs::write(path, text.replacen(from, to, 1)).unwrap();
}

fn prepend_pkg_config(ns: &str) -> String {
    match std::env::var("PKG_CONFIG_PATH") {
        Ok(existing) if !existing.is_empty() => format!("{ns}:{existing}"),
        _ => ns.to_string(),
    }
}

fn run_make(source: &Path, pkg_config_path: &str) {
    let jobs = std::env::var("NUM_JOBS")
        .or_else(|_| std::env::var("CARGO_BUILD_JOBS"))
        .unwrap_or_else(|_| "4".into());
    let perl = env("SIGHURT_NETSURF_PERL");
    let xxd_dir = Path::new(&env("SIGHURT_NETSURF_XXD"))
        .parent()
        .unwrap()
        .to_path_buf();
    let gperf_dir = Path::new(&env("SIGHURT_NETSURF_GPERF"))
        .parent()
        .unwrap()
        .to_path_buf();
    let path = format!(
        "{}:{}:{}",
        xxd_dir.display(),
        gperf_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let log_path = PathBuf::from(env("OUT_DIR")).join("netsurf-make.log");
    let log = fs::File::create(&log_path).unwrap();
    let status = Command::new("make")
        .current_dir(source)
        .env("CFLAGS", "-fcommon -fPIC -Wno-error")
        .env("PKG_CONFIG_PATH", pkg_config_path)
        .env("PERL", &perl)
        .env("PATH", &path)
        .args(["-j", &jobs, "TARGET=sighurt"])
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .status()
        .unwrap_or_else(|e| panic!("make: {e}"));
    if !status.success() {
        let output = fs::read_to_string(&log_path).unwrap_or_default();
        let tail: String = output
            .chars()
            .rev()
            .take(4000)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        panic!("NetSurf make failed (log {}):\n{tail}", log_path.display());
    }
}

fn archive_objects(source: &Path, out_dir: &Path) -> PathBuf {
    let obj = source.join("build/Linux-sighurt");
    let mut objects = Vec::new();
    for entry in fs::read_dir(&obj).unwrap_or_else(|e| panic!("{}: {e}", obj.display())) {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // The frontend's stub main only lets NetSurf's makefile finish its link.
        if name.ends_with(".o") && name != "frontends_sighurt_main.o" {
            objects.push(entry.path());
        }
    }
    if objects.is_empty() {
        panic!("no objects in {}", obj.display());
    }
    objects.sort();
    let archive = out_dir.join("libnscore.a");
    let _ = fs::remove_file(&archive);
    let status = Command::new("ar")
        .arg("rcs")
        .arg(&archive)
        .args(&objects)
        .status()
        .unwrap_or_else(|e| panic!("ar: {e}"));
    if !status.success() {
        panic!("ar failed");
    }
    archive
}

/// Tells cargo what to link. `cargo:rustc-link-arg` would apply to this crate's own tests only, so
/// everything goes through `rustc-link-lib`, which reaches every binary that depends on the
/// crate. The core and the static libraries are merged into one archive: the linker rescans one
/// archive until nothing more resolves, so their order does not matter.
fn link(archive: &Path, pkg_config_path: &str, out_dir: &Path) {
    let merged = out_dir.join("libnsall.a");
    let _ = fs::remove_file(&merged);
    let mut script = format!(
        "create {}\naddlib {}\n",
        merged.display(),
        archive.display()
    );
    for package in STATIC_LIBS {
        let lib = static_archive(package, pkg_config_path);
        script.push_str(&format!("addlib {}\n", lib.display()));
    }
    script.push_str("save\nend\n");
    let mut child = Command::new("ar")
        .arg("-M")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("ar: {e}"));
    std::io::Write::write_all(child.stdin.as_mut().unwrap(), script.as_bytes()).unwrap();
    if !child.wait().unwrap().success() {
        panic!("merging the static libraries failed");
    }
    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=nsall");
    for package in SHARED_LIBS {
        let (libname, libdir) = shared_lib(package, pkg_config_path);
        println!("cargo:rustc-link-search=native={libdir}");
        println!("cargo:rustc-link-lib=dylib={libname}");
    }
}

fn static_archive(package: &str, pkg_config_path: &str) -> PathBuf {
    let libdir = pkg_config_var(package, "libdir", pkg_config_path);
    let name = package.trim_start_matches("lib");
    let archive = Path::new(&libdir).join(format!("lib{name}.a"));
    if !archive.is_file() {
        panic!("no static library {}", archive.display());
    }
    archive
}

fn shared_lib(package: &str, pkg_config_path: &str) -> (String, String) {
    let libs = pkg_config_libs(package, pkg_config_path, false);
    let mut name = None;
    let mut dir = pkg_config_var(package, "libdir", pkg_config_path);
    for arg in libs.split_whitespace() {
        if let Some(rest) = arg.strip_prefix("-l") {
            if name.is_none() {
                name = Some(rest.to_string());
            }
        } else if let Some(rest) = arg.strip_prefix("-L") {
            dir = rest.to_string();
        }
    }
    let name = name.unwrap_or_else(|| package.trim_start_matches("lib").to_string());
    (name, dir)
}

fn pkg_config_var(package: &str, var: &str, pkg_config_path: &str) -> String {
    let output = Command::new("pkg-config")
        .env("PKG_CONFIG_PATH", pkg_config_path)
        .args(["--variable", var, package])
        .output()
        .unwrap_or_else(|e| panic!("pkg-config: {e}"));
    if !output.status.success() {
        panic!(
            "pkg-config --variable {var} {package}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn pkg_config_libs(package: &str, pkg_config_path: &str, static_libs: bool) -> String {
    let mut cmd = Command::new("pkg-config");
    cmd.env("PKG_CONFIG_PATH", pkg_config_path);
    if static_libs {
        cmd.arg("--static");
    }
    cmd.args(["--libs", package]);
    let output = cmd.output().unwrap_or_else(|e| panic!("pkg-config: {e}"));
    if !output.status.success() {
        panic!(
            "pkg-config --libs {package}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn export_resources(source: &Path, out_dir: &Path) {
    let dest = out_dir.join("res");
    fs::create_dir_all(&dest).unwrap();
    for name in ["default.css", "quirks.css", "internal.css", "adblock.css"] {
        let from = source.join("resources").join(name);
        if from.is_file() {
            fs::copy(&from, dest.join(name)).unwrap();
        }
    }
    let messages = source.join("frontends/sighurt/res/en/Messages");
    if !messages.is_file() {
        panic!(
            "NetSurf did not write {} (the message split). The frontend Makefile sets \
             MESSAGES_FILTER=any and MESSAGES_TARGET to the frontend res directory.",
            messages.display()
        );
    }
    fs::create_dir_all(dest.join("en")).unwrap();
    fs::copy(&messages, dest.join("en/Messages")).unwrap();
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        panic!(
            "{name} is not set. Build inside the repository nix-shell (tools/cargo does); \
             see docs/rewrite/NETSURF.md"
        )
    })
}
