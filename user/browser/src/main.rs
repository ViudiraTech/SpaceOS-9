use space_browser::browser::run_browser;
use std::path::Path;

fn resolve_cli_target() -> Option<String> {
    let arg = std::env::args().nth(1)?;
    let trimmed = arg.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.contains("://") || trimmed.starts_with("about:") || trimmed.starts_with("data:") {
        return Some(trimmed.to_string());
    }
    let p = Path::new(trimmed);
    if p.exists() {
        let abs = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        return Some(format!("file://{}", abs.display()));
    }
    Some(trimmed.to_string())
}

fn main() {
    let target = resolve_cli_target();
    if let Err(err) = run_browser(target) {
        eprintln!("space-browser: {err}");
        std::process::exit(1);
    }
}
