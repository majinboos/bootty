use std::{
    env,
    fmt::Write,
    fs,
    path::{Path, PathBuf},
};

fn main() {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"))
        .join("src/builtin_modules");
    println!("cargo:rerun-if-changed={}", root.display());

    let mut paths = Vec::new();
    collect_luau(&root, &mut paths);
    paths.sort();

    let mut generated = String::from("const DISCOVERED: &[BuiltinModule] = &[\n");
    for path in paths {
        let identity = path
            .strip_prefix(&root)
            .expect("builtin module stays below its root")
            .to_string_lossy()
            .replace('\\', "/");
        writeln!(
            generated,
            "    BuiltinModule {{ identity: {identity:?}, source: include_str!({path:?}) }},",
            path = path.to_string_lossy(),
        )
        .expect("write builtin module entry");
    }
    generated.push_str("];\n");

    let output = PathBuf::from(env::var_os("OUT_DIR").expect("build output directory"))
        .join("builtin_modules.rs");
    fs::write(output, generated).expect("write builtin module manifest");
}

fn collect_luau(directory: &Path, paths: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => panic!("read {}: {error}", directory.display()),
    };
    for entry in entries {
        let path = entry.expect("read builtin module entry").path();
        if path.is_dir() {
            collect_luau(&path, paths);
        } else if path.extension().and_then(|value| value.to_str()) == Some("luau") {
            paths.push(path);
        }
    }
}
