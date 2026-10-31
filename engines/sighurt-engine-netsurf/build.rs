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
    let tree = build_tree();
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

/// The directory the NetSurf tree is unpacked into.
fn build_tree() -> PathBuf {
    println!("cargo:rerun-if-env-changed=SIGHURT_NETSURF_BUILD");
    if let Ok(dir) = std::env::var("SIGHURT_NETSURF_BUILD") {
        return PathBuf::from(dir);
    }
    let hdd = Path::new("/data/sig-builds");
    if hdd.is_dir() {
        hdd.join("netsurf")
    } else {
        PathBuf::from(env("OUT_DIR")).join("netsurf-build")
    }
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
}

fn replace_once(path: &Path, marker: &str, from: &str, to: &str) {
    let text = fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
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
        let tail: String = output.chars().rev().take(4000).collect::<String>().chars().rev().collect();
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
        if name.ends_with(".o") && !name.contains("main") {
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

fn link(archive: &Path, pkg_config_path: &str, out_dir: &Path) {
    println!("cargo:rustc-link-search=native={}", out_dir.display());
    // Full paths, in order: rustc does not interleave link-arg and link-lib.
    println!("cargo:rustc-link-arg=-Wl,--whole-archive");
    println!("cargo:rustc-link-arg={}", archive.display());
    println!("cargo:rustc-link-arg=-Wl,--no-whole-archive");
    println!("cargo:rustc-link-arg=-Wl,--start-group");
    let mut rpath = Vec::new();
    for package in STATIC_LIBS {
        let archive = static_archive(package, pkg_config_path);
        println!("cargo:rustc-link-arg={}", archive.display());
    }
    println!("cargo:rustc-link-arg=-Wl,--end-group");
    for package in SHARED_LIBS {
        let (libname, libdir) = shared_lib(package, pkg_config_path);
        if !rpath.contains(&libdir) {
            println!("cargo:rustc-link-arg=-L{libdir}");
            println!("cargo:rustc-link-arg=-Wl,-rpath,{libdir}");
            rpath.push(libdir);
        }
        println!("cargo:rustc-link-arg=-l{libname}");
    }
    for lib in ["m", "pthread", "dl", "c"] {
        println!("cargo:rustc-link-arg=-l{lib}");
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
