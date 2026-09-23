const PLUGIN: sqlite_ext::Plugin = sqlite_ext::Plugin::new(
    "engine_iso_native_fixture",
    env!("CARGO_PKG_VERSION"),
    "warn",
    lab_20260923_0::register_native_fixture,
);

sqlite_ext::sqlite_extension!(sqlite3_engine_iso_init, PLUGIN);
