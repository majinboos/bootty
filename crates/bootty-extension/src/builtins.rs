const STATUS: &[(&str, &str)] = &[
    ("windows", include_str!("status_defaults/windows.luau")),
    ("clock", include_str!("status_defaults/clock.luau")),
    ("session", include_str!("status_defaults/session.luau")),
    ("sysinfo", include_str!("status_defaults/sysinfo.luau")),
];

const SIDEBAR: &[(&str, &str)] = &[
    ("sessions", include_str!("sidebar_defaults/sessions.luau")),
    ("codexbar", include_str!("sidebar_defaults/codexbar.luau")),
];

const SESSION: &[(&str, &str)] = &[
    ("diffs", include_str!("session_defaults/diffs.luau")),
    ("process", include_str!("session_defaults/process.luau")),
    ("directory", include_str!("session_defaults/directory.luau")),
    ("branch", include_str!("session_defaults/branch.luau")),
    ("ports", include_str!("session_defaults/ports.luau")),
    ("progress", include_str!("session_defaults/progress.luau")),
];

pub(super) struct BuiltinExtensionModule {
    pub identity: &'static str,
    pub placement: &'static str,
    pub source: &'static str,
}

pub(super) struct BuiltinModule {
    pub identity: &'static str,
    pub source: &'static str,
}

include!(concat!(env!("OUT_DIR"), "/builtin_modules.rs"));

pub(super) fn modules() -> Vec<BuiltinExtensionModule> {
    let mut modules = Vec::new();
    for (placement, builtins) in [
        ("status", STATUS),
        ("sidebar", SIDEBAR),
        ("session", SESSION),
    ] {
        modules.extend(
            builtins
                .iter()
                .map(|(identity, source)| BuiltinExtensionModule {
                    identity,
                    placement,
                    source,
                }),
        );
    }
    modules
}

pub(super) fn discovered_modules() -> impl Iterator<Item = &'static BuiltinModule> {
    DISCOVERED.iter()
}
