//! Read-only observation of a drained WAL; no tools, LLM or production writes.
fn main() {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--spool")) {
        eprintln!("usage: harness-observer --spool PATH");
        std::process::exit(2);
    }
    let Some(path) = args.next() else { std::process::exit(2); };
    if let Err(error) = harness_runtime::monitoring::observer::collect(std::path::Path::new(&path)) {
        eprintln!("observer: {error}");
        std::process::exit(1);
    }
}
