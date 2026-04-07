//! pure sampling logic.
//!
//! takes an injected [`SystemProbe`] so unit tests can feed synthetic
//! mem/cpu/process data without spinning up sysinfo. real OS-touching impl
//! is [`SysinfoProbe`], exercised by one smoke test behind cfg(test)+ignore
//! (sysinfo warm-up is flaky on CI).
//!
//! deterministic output:
//!
//! * `processes` sorted by (memory desc, pid asc). pid tiebreaker keeps row
//!   order stable across ticks, the UI's virtualised table keys off pid so
//!   flicker here is user-visible
//! * `top_by_cpu` sorted (cpu desc, pid asc) with partial_cmp so NaN readings
//!   (sysinfo briefly returns NaN for <1 tick old procs) sink to the bottom
//! * `top_by_memory` / `top_by_cpu` truncate to `top_n`. top_n==0 yields
//!   empty lists (activity screen only cares about the full process list)

use std::cmp::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use super::types::{ActivitySnapshot, CpuSnapshot, MemorySnapshot, ProcessRow};

/// implementations refresh internal caches on the sampling thread. stream
/// driver calls [`SystemProbe::refresh`] once per tick before reading
pub trait SystemProbe: Send {
    fn refresh(&mut self);
    fn memory_total(&self) -> u64;
    fn memory_used(&self) -> u64;
    fn memory_free(&self) -> u64;
    fn memory_available(&self) -> u64;
    fn swap_total(&self) -> u64;
    fn swap_used(&self) -> u64;
    fn cpu_per_core(&self) -> Vec<f32>;
    fn processes(&self) -> Vec<ProcessRow>;
}

/// refreshes probe then reads every surface. top_n==0 disables the top lists
pub fn sample(probe: &mut dyn SystemProbe, top_n: usize, tick: u64) -> ActivitySnapshot {
    probe.refresh();
    let total = probe.memory_total();
    let used = probe.memory_used();
    let mem = MemorySnapshot {
        total_bytes: total,
        used_bytes: used,
        free_bytes: probe.memory_free(),
        available_bytes: probe.memory_available(),
        swap_total_bytes: probe.swap_total(),
        swap_used_bytes: probe.swap_used(),
        pressure_percent: MemorySnapshot::compute_pressure(used, total),
    };
    let cpu = CpuSnapshot::from_per_core(probe.cpu_per_core());
    let mut processes = probe.processes();
    sort_by_memory(&mut processes);
    let process_count = processes.len();
    let top_by_memory = take_top(&processes, top_n);
    let mut by_cpu = processes.clone();
    sort_by_cpu(&mut by_cpu);
    let top_by_cpu = take_top(&by_cpu, top_n);

    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    ActivitySnapshot {
        timestamp_ms,
        memory: mem,
        cpu,
        processes,
        top_by_memory,
        top_by_cpu,
        process_count,
        tick,
    }
}

fn sort_by_memory(rows: &mut [ProcessRow]) {
    rows.sort_by(|a, b| {
        b.memory_bytes
            .cmp(&a.memory_bytes)
            .then_with(|| a.pid.cmp(&b.pid))
    });
}

fn sort_by_cpu(rows: &mut [ProcessRow]) {
    rows.sort_by(|a, b| {
        // NaN never compares Greater, so NaN cpu_percent sinks. without this
        // the top-CPU card could briefly show "NaN% - firefox" while sysinfo
        // warms up
        match b.cpu_percent.partial_cmp(&a.cpu_percent) {
            Some(o) => o.then_with(|| a.pid.cmp(&b.pid)),
            None => {
                let a_nan = a.cpu_percent.is_nan();
                let b_nan = b.cpu_percent.is_nan();
                match (a_nan, b_nan) {
                    (true, false) => Ordering::Greater,
                    (false, true) => Ordering::Less,
                    _ => a.pid.cmp(&b.pid),
                }
            }
        }
    });
}

fn take_top(rows: &[ProcessRow], n: usize) -> Vec<ProcessRow> {
    if n == 0 {
        return Vec::new();
    }
    rows.iter().take(n).cloned().collect()
}

// ------------------- sysinfo-backed implementation -------------------

/// real-host probe. only refreshes the signals we sample, refresh_all()
/// would also pull disk / network / component / user which we don't need
pub struct SysinfoProbe {
    sys: sysinfo::System,
}

impl SysinfoProbe {
    pub fn new() -> Self {
        let sys = sysinfo::System::new();
        Self { sys }
    }
}

impl Default for SysinfoProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemProbe for SysinfoProbe {
    fn refresh(&mut self) {
        self.sys.refresh_memory();
        self.sys.refresh_cpu_usage();
        self.sys
            .refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    }

    fn memory_total(&self) -> u64 {
        self.sys.total_memory()
    }

    fn memory_used(&self) -> u64 {
        self.sys.used_memory()
    }

    fn memory_free(&self) -> u64 {
        self.sys.free_memory()
    }

    fn memory_available(&self) -> u64 {
        self.sys.available_memory()
    }

    fn swap_total(&self) -> u64 {
        self.sys.total_swap()
    }

    fn swap_used(&self) -> u64 {
        self.sys.used_swap()
    }

    fn cpu_per_core(&self) -> Vec<f32> {
        self.sys.cpus().iter().map(|c| c.cpu_usage()).collect()
    }

    fn processes(&self) -> Vec<ProcessRow> {
        self.sys
            .processes()
            .iter()
            // filter out threads. on linux refresh_processes enumerates every
            // /proc/<pid>/task/<tid> and reports each thread's memory as the
            // owning process's RSS, so firefox w/ 12 threads floods Top Memory
            // with 12 identical rows. thread_kind() is Some(_) for a thread,
            // None for a real process, only platform-agnostic classifier
            // sysinfo exposes
            .filter(|(_pid, p)| p.thread_kind().is_none())
            .map(|(pid, p)| {
                let cmd_parts = p.cmd();
                let command = cmd_parts
                    .iter()
                    .map(|s| s.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join(" ");
                ProcessRow {
                    pid: pid.as_u32(),
                    parent_pid: p.parent().map(|pp| pp.as_u32()),
                    name: p.name().to_string_lossy().into_owned(),
                    command,
                    user: resolve_user(&self.sys, p),
                    cpu_percent: p.cpu_usage(),
                    memory_bytes: p.memory(),
                    start_time: p.start_time(),
                    threads: None,
                }
            })
            .collect()
    }
}

/// we don't ship sysinfo's `user` feature flag (adds a platform user DB
/// refresh we don't need) so user_id -> name mapping is unavailable.
/// returning None keeps the wire format stable
fn resolve_user(_sys: &sysinfo::System, _p: &sysinfo::Process) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    /// mock probe driven by plain vecs. every accessor clones, fields are
    /// pub so tests can mutate between ticks
    pub struct MockProbe {
        pub total: u64,
        pub used: u64,
        pub free: u64,
        pub available: u64,
        pub swap_total: u64,
        pub swap_used: u64,
        pub cpu: Vec<f32>,
        pub procs: Vec<ProcessRow>,
        pub refreshes: Arc<AtomicU64>,
    }

    impl MockProbe {
        fn new() -> Self {
            Self {
                total: 16 * 1024 * 1024 * 1024,
                used: 8 * 1024 * 1024 * 1024,
                free: 8 * 1024 * 1024 * 1024,
                available: 8 * 1024 * 1024 * 1024,
                swap_total: 0,
                swap_used: 0,
                cpu: vec![10.0, 20.0],
                procs: vec![],
                refreshes: Arc::new(AtomicU64::new(0)),
            }
        }
    }

    impl SystemProbe for MockProbe {
        fn refresh(&mut self) {
            self.refreshes.fetch_add(1, Ordering::Relaxed);
        }
        fn memory_total(&self) -> u64 {
            self.total
        }
        fn memory_used(&self) -> u64 {
            self.used
        }
        fn memory_free(&self) -> u64 {
            self.free
        }
        fn memory_available(&self) -> u64 {
            self.available
        }
        fn swap_total(&self) -> u64 {
            self.swap_total
        }
        fn swap_used(&self) -> u64 {
            self.swap_used
        }
        fn cpu_per_core(&self) -> Vec<f32> {
            self.cpu.clone()
        }
        fn processes(&self) -> Vec<ProcessRow> {
            self.procs.clone()
        }
    }

    pub fn row(pid: u32, name: &str, mem: u64, cpu: f32) -> ProcessRow {
        ProcessRow {
            pid,
            parent_pid: None,
            name: name.into(),
            command: name.into(),
            user: None,
            cpu_percent: cpu,
            memory_bytes: mem,
            start_time: 0,
            threads: None,
        }
    }

    #[test]
    fn sample_refreshes_probe_exactly_once() {
        let mut probe = MockProbe::new();
        let counter = probe.refreshes.clone();
        let _ = sample(&mut probe, 5, 0);
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn sample_sorts_processes_by_memory_desc() {
        let mut probe = MockProbe::new();
        probe.procs = vec![
            row(1, "a", 100, 0.0),
            row(2, "b", 500, 0.0),
            row(3, "c", 300, 0.0),
        ];
        let snap = sample(&mut probe, 10, 0);
        let pids: Vec<u32> = snap.processes.iter().map(|r| r.pid).collect();
        assert_eq!(pids, vec![2, 3, 1]);
    }

    #[test]
    fn sample_memory_tiebreak_is_pid_asc() {
        let mut probe = MockProbe::new();
        probe.procs = vec![
            row(9, "same", 100, 0.0),
            row(1, "same", 100, 0.0),
            row(5, "same", 100, 0.0),
        ];
        let snap = sample(&mut probe, 10, 0);
        let pids: Vec<u32> = snap.processes.iter().map(|r| r.pid).collect();
        assert_eq!(pids, vec![1, 5, 9]);
    }

    #[test]
    fn sample_top_by_memory_truncates() {
        let mut probe = MockProbe::new();
        probe.procs = (1..=20)
            .map(|i| row(i, "p", i as u64 * 10, 0.0))
            .collect();
        let snap = sample(&mut probe, 5, 0);
        assert_eq!(snap.top_by_memory.len(), 5);
        // largest first: pid 20 down to 16
        assert_eq!(snap.top_by_memory[0].pid, 20);
        assert_eq!(snap.top_by_memory[4].pid, 16);
    }

    #[test]
    fn sample_top_n_zero_yields_empty_top_lists() {
        let mut probe = MockProbe::new();
        probe.procs = vec![row(1, "a", 10, 0.0), row(2, "b", 20, 0.0)];
        let snap = sample(&mut probe, 0, 0);
        assert!(snap.top_by_memory.is_empty());
        assert!(snap.top_by_cpu.is_empty());
        // full list still populated
        assert_eq!(snap.processes.len(), 2);
    }

    #[test]
    fn sample_top_by_cpu_desc() {
        let mut probe = MockProbe::new();
        probe.procs = vec![
            row(1, "low", 0, 1.0),
            row(2, "high", 0, 95.0),
            row(3, "mid", 0, 40.0),
        ];
        let snap = sample(&mut probe, 3, 0);
        let pids: Vec<u32> = snap.top_by_cpu.iter().map(|r| r.pid).collect();
        assert_eq!(pids, vec![2, 3, 1]);
    }

    #[test]
    fn sample_nan_cpu_sinks_to_bottom() {
        let mut probe = MockProbe::new();
        probe.procs = vec![
            row(1, "runaway", 0, f32::NAN),
            row(2, "real", 0, 80.0),
            row(3, "small", 0, 10.0),
        ];
        let snap = sample(&mut probe, 3, 0);
        let pids: Vec<u32> = snap.top_by_cpu.iter().map(|r| r.pid).collect();
        // NaN must not be first, "NaN%" as #1 would look broken
        assert_eq!(pids.first().copied(), Some(2));
        assert_eq!(pids.last().copied(), Some(1));
    }

    #[test]
    fn sample_populates_memory_snapshot_fields() {
        let mut probe = MockProbe::new();
        probe.total = 1000;
        probe.used = 400;
        probe.free = 600;
        probe.available = 550;
        probe.swap_total = 100;
        probe.swap_used = 20;
        let snap = sample(&mut probe, 0, 0);
        assert_eq!(snap.memory.total_bytes, 1000);
        assert_eq!(snap.memory.used_bytes, 400);
        assert_eq!(snap.memory.free_bytes, 600);
        assert_eq!(snap.memory.available_bytes, 550);
        assert_eq!(snap.memory.swap_total_bytes, 100);
        assert_eq!(snap.memory.swap_used_bytes, 20);
        assert!((snap.memory.pressure_percent - 40.0).abs() < 0.01);
    }

    #[test]
    fn sample_populates_cpu_snapshot() {
        let mut probe = MockProbe::new();
        probe.cpu = vec![50.0, 50.0, 50.0, 50.0];
        let snap = sample(&mut probe, 0, 0);
        assert_eq!(snap.cpu.core_count, 4);
        assert!((snap.cpu.average_percent - 50.0).abs() < 0.01);
    }

    #[test]
    fn sample_tick_is_pass_through() {
        let mut probe = MockProbe::new();
        let snap = sample(&mut probe, 0, 42);
        assert_eq!(snap.tick, 42);
    }

    #[test]
    fn sample_timestamp_is_monotonic_ish() {
        // back-to-back samples must not produce t2 < t1. millis resolution,
        // equal is fine, less-than = clock issue
        let mut probe = MockProbe::new();
        let a = sample(&mut probe, 0, 0);
        let b = sample(&mut probe, 0, 1);
        assert!(b.timestamp_ms >= a.timestamp_ms);
    }

    #[test]
    fn sample_process_count_matches_processes_len() {
        let mut probe = MockProbe::new();
        probe.procs = (1..=7).map(|i| row(i, "x", 0, 0.0)).collect();
        let snap = sample(&mut probe, 3, 0);
        assert_eq!(snap.process_count, 7);
        assert_eq!(snap.processes.len(), 7);
    }

    #[test]
    fn sample_on_empty_probe_yields_zeroes() {
        let mut probe = MockProbe::new();
        probe.procs = vec![];
        probe.cpu = vec![];
        probe.total = 0;
        probe.used = 0;
        let snap = sample(&mut probe, 5, 0);
        assert_eq!(snap.process_count, 0);
        assert!(snap.processes.is_empty());
        assert!(snap.top_by_memory.is_empty());
        assert!(snap.top_by_cpu.is_empty());
        assert_eq!(snap.memory.pressure_percent, 0.0);
        assert_eq!(snap.cpu.core_count, 0);
    }

    /// smoke test: real sysinfo probe should report at least one process
    /// (this test proc) and positive total memory. ignored because sysinfo's
    /// first tick reports total_memory()=0 during startup on some platforms,
    /// still want the path compiled
    #[test]
    #[ignore]
    fn sysinfo_probe_real_smoke() {
        let mut p = SysinfoProbe::new();
        let snap = sample(&mut p, 5, 0);
        assert!(snap.memory.total_bytes > 0, "expected real RAM reading");
        assert!(snap.process_count > 0, "expected at least one process");
    }
}
