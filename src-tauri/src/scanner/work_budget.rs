//! process-wide scanner parallelism budget.
//!
//! jwalk and the duplicate hasher both use Rayon. Without an explicit global
//! pool they size themselves to the host, and nested scans can turn one UI
//! action into several full-width worker sets. Configure Rayon once at process
//! startup so filesystem-heavy work leaves CPU and IO headroom for the app.

use std::num::NonZeroUsize;

const ENV_SCANNER_THREADS: &str = "SAFAI_SCANNER_THREADS";
const MAX_DEFAULT_THREADS: usize = 4;
const MAX_OVERRIDE_THREADS: usize = 32;

pub fn configure_global_rayon_pool() {
    let threads = configured_threads();
    match rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .thread_name(|idx| format!("safai-scan-rayon-{idx}"))
        .build_global()
    {
        Ok(()) => {
            eprintln!("[safai] scanner rayon pool configured with {threads} workers");
        }
        Err(e) => {
            // Rayon can only be initialized once per process. Tests or a plugin
            // may have touched it first; in that case keep running with the
            // existing pool rather than failing app startup.
            eprintln!("[safai] scanner rayon pool already configured: {e}");
        }
    }
}

fn configured_threads() -> usize {
    std::env::var(ENV_SCANNER_THREADS)
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .map(|n| n.clamp(1, MAX_OVERRIDE_THREADS))
        .unwrap_or_else(default_threads)
}

fn default_threads() -> usize {
    let logical = std::thread::available_parallelism()
        .unwrap_or(NonZeroUsize::new(4).expect("non-zero fallback"))
        .get();
    if logical <= 2 {
        1
    } else {
        (logical - 1).min(MAX_DEFAULT_THREADS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_threads_are_bounded() {
        let threads = default_threads();
        assert!((1..=MAX_DEFAULT_THREADS).contains(&threads));
    }
}
