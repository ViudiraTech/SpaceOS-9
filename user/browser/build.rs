use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn collect_files(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", dir.display(), e))
        .filter_map(|res| res.ok().map(|e| e.path()))
        .collect();
    entries.sort();
    for p in entries {
        if p.is_file() && p.extension().and_then(|s| s.to_str()) == Some(ext) {
            out.push(p);
        }
    }
}

fn find_single_subdir(parent: &Path) -> PathBuf {
    let mut dirs: Vec<PathBuf> = fs::read_dir(parent)
        .unwrap_or_else(|e| panic!("missing c++ headers dir {}: {}", parent.display(), e))
        .filter_map(|r| r.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs.into_iter()
        .next()
        .unwrap_or_else(|| panic!("no versioned libstdc++ dir under {}", parent.display()))
}

fn configure_cross(build: &mut cc::Build, target: &str, sysroot: &Path, is_cpp: bool) {
    if !target.contains("musl") {
        return;
    }
    build.compiler(if is_cpp { "clang++" } else { "clang" });
    build.archiver("ar");
    build.flag("--target=x86_64-linux-musl");
    build.flag(&format!("--sysroot={}", sysroot.display()));
    build.flag("-fPIC");
    if is_cpp {
        let cxx_root = find_single_subdir(&sysroot.join("usr/include/c++"));
        let cxx_arch = cxx_root.join("x86_64-alpine-linux-musl");
        build.flag("-nostdinc++");
        build.flag("-isystem").flag(&cxx_root.display().to_string());
        build.flag("-isystem").flag(&cxx_arch.display().to_string());
    }
    build
        .flag("-isystem")
        .flag(&sysroot.join("usr/include").display().to_string());
}

fn ensure_sysroot(repo_root: &Path) -> PathBuf {
    let sysroot = repo_root.join("build/musl-sysroot");
    if !sysroot.join("usr/include/stdio.h").exists() || !sysroot.join("usr/lib/libstdc++.a").exists() {
        let script = repo_root.join("tools/ensure_musl_sysroot.py");
        let status = Command::new("python3")
            .arg(&script)
            .status()
            .expect("failed to execute ensure_musl_sysroot.py");
        assert!(status.success(), "ensure_musl_sysroot.py exited with {status}");
    }
    sysroot
}

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let repo_root = manifest_dir.parent().unwrap().parent().unwrap().to_path_buf();
    let target = env::var("TARGET").unwrap_or_default();
    let sysroot = if target.contains("musl") {
        ensure_sysroot(&repo_root)
    } else {
        PathBuf::new()
    };

    let third_party = repo_root.join("user/third_party");
    let litehtml_dir = third_party.join("litehtml");
    let gumbo_src = litehtml_dir.join("src/gumbo/src");
    let qjs_dir = third_party.join("quickjs-ng");
    let engine_dir = manifest_dir.join("engine");

    for dir in [&engine_dir, &litehtml_dir.join("src"), &litehtml_dir.join("include"), &qjs_dir] {
        println!("cargo:rerun-if-changed={}", dir.display());
    }

    // 1. Gumbo HTML5 parser (C11)
    let mut gumbo_files = Vec::new();
    collect_files(&gumbo_src, "c", &mut gumbo_files);
    let mut gumbo = cc::Build::new();
    configure_cross(&mut gumbo, &target, &sysroot, false);
    gumbo
        .std("c11")
        .opt_level(2)
        .flag("-fPIC")
        .warnings(false)
        .include(&gumbo_src);
    for f in &gumbo_files {
        gumbo.file(f);
    }
    gumbo.compile("space_gumbo");

    // 2. QuickJS-NG core (GNU11)
    let qjs_files = ["quickjs.c", "dtoa.c", "libregexp.c", "libunicode.c"];
    let mut qjs = cc::Build::new();
    configure_cross(&mut qjs, &target, &sysroot, false);
    qjs.std("gnu11")
        .opt_level(2)
        .flag("-fPIC")
        .flag("-funsigned-char")
        .define("QUICKJS_NG_BUILD", None)
        .define("_GNU_SOURCE", None)
        .warnings(false)
        .include(&qjs_dir);
    for f in qjs_files {
        qjs.file(qjs_dir.join(f));
    }
    qjs.compile("space_quickjs");

    // 3. LiteHTML core (C++17)
    let mut litehtml_files = Vec::new();
    collect_files(&litehtml_dir.join("src"), "cpp", &mut litehtml_files);
    let mut litehtml = cc::Build::new();
    configure_cross(&mut litehtml, &target, &sysroot, true);
    litehtml
        .cpp(true)
        .flag("-std=c++17")
        .opt_level(2)
        .flag("-fPIC")
        .warnings(false)
        .include(litehtml_dir.join("include"))
        .include(litehtml_dir.join("include/litehtml"))
        .include(&gumbo_src);
    for f in &litehtml_files {
        litehtml.file(f);
    }
    litehtml.compile("space_litehtml");

    // 4. Space Browser C++ DOM/Widget/Canvas2D engine (C++17)
    let engine_files = [
        "canvas2d.cpp",
        "widgets.cpp",
        "container.cpp",
        "dom_bridge.cpp",
        "engine.cpp",
    ];
    let mut engine = cc::Build::new();
    configure_cross(&mut engine, &target, &sysroot, true);
    engine
        .cpp(true)
        .flag("-std=c++17")
        .opt_level(2)
        .flag("-fPIC")
        .flag("-Wall")
        .flag("-Wextra")
        .flag("-Wno-unused-parameter")
        .include(&engine_dir)
        .include(litehtml_dir.join("include"))
        .include(litehtml_dir.join("include/litehtml"))
        .include(&gumbo_src)
        .include(&qjs_dir);
    for f in engine_files {
        let p = engine_dir.join(f);
        println!("cargo:rerun-if-changed={}", p.display());
        engine.file(p);
    }
    engine.compile("space_engine");

    // Link ordering: engine -> litehtml + quickjs + gumbo -> libstdc++
    println!("cargo:rustc-link-lib=static=space_engine");
    println!("cargo:rustc-link-lib=static=space_litehtml");
    println!("cargo:rustc-link-lib=static=space_quickjs");
    println!("cargo:rustc-link-lib=static=space_gumbo");

    if target.contains("musl") {
        println!(
            "cargo:rustc-link-search=native={}",
            sysroot.join("usr/lib").display()
        );
        println!("cargo:rustc-link-lib=static=stdc++");
    } else {
        println!("cargo:rustc-link-lib=stdc++");
    }
}
