//! wire shapes for the memory + activity screens.
//!
//! every numeric field is a plain scalar, UI does its own unit formatting
//! (MiB/GiB/%) via `src/lib/format.ts` so rust stays dimensionless.
//! `#[serde(rename_all = "camelCase")]` matches `src/lib/activity.ts`

use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProcessRow {
    pub pid: u32,
    pub parent_pid: Option<u32>,
    /// short display name e.g. `zsh`, `Google Chrome Helper`
    pub name: String,
    /// full joined cmdline when available. safe to be empty
    pub command: String,
    /// None when sysinfo can't resolve
    pub user: Option<String>,
    /// percent across *all* cores, 0.0..=(cores * 100.0). UI normalises to
    /// per-core on display, we keep the raw summed value so numbers match
    /// `top` / Activity Monitor
    pub cpu_percent: f32,
    pub memory_bytes: u64,
    /// unix seconds since start. 0 when unknown
    pub start_time: u64,
    pub threads: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MemorySnapshot {
    pub total_bytes: u64,
    pub used_bytes: u64,
    /// free = total - used. "free" vs "available" matters on linux (page
    /// cache counts as free from the kernel's POV), we ship both so UI
    /// picks the right one per-OS
    pub free_bytes: u64,
    pub available_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_used_bytes: u64,
    /// used/total * 100, clamped [0, 100]. drives the hero gauge
    pub pressure_percent: f32,
}

impl MemorySnapshot {
    pub fn compute_pressure(used: u64, total: u64) -> f32 {
        if total == 0 {
            return 0.0;
        }
        let pct = (used as f64 / total as f64) * 100.0;
        if pct < 0.0 {
            0.0
        } else if pct > 100.0 {
            100.0
        } else {
            pct as f32
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CpuSnapshot {
    pub core_count: usize,
    /// raw `cpu_usage()` from sysinfo, one per logical core. already clamped
    /// to [0, 100] against pathological readings (macOS briefly reports 101%
    /// for procs alive <1 tick)
    pub per_core_percent: Vec<f32>,
    /// mean of `per_core_percent`, clamped [0, 100]
    pub average_percent: f32,
}

impl CpuSnapshot {
    /// clamp each reading [0, 100] and compute mean. empty input (shouldn't
    /// happen on real host but does mid-startup in sysinfo) yields 0.0 / 0
    pub fn from_per_core(per_core: Vec<f32>) -> Self {
        let cleaned: Vec<f32> = per_core
            .into_iter()
            .map(|v| {
                if v < 0.0 {
                    0.0
                } else if v > 100.0 {
                    100.0
                } else {
                    v
                }
            })
            .collect();
        let core_count = cleaned.len();
        let avg = if core_count == 0 {
            0.0
        } else {
            let sum: f32 = cleaned.iter().copied().sum();
            let raw = sum / core_count as f32;
            if raw < 0.0 {
                0.0
            } else if raw > 100.0 {
                100.0
            } else {
                raw
            }
        };
        Self {
            core_count,
            per_core_percent: cleaned,
            average_percent: avg,
        }
    }
}

/// emitted on `activity://snapshot`. carries enough for both the hero gauge
/// and the activity monitor table so UI only needs one subscription
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ActivitySnapshot {
    /// unix millis at sample time. drives the sparkline x-axis
    pub timestamp_ms: u64,
    pub memory: MemorySnapshot,
    pub cpu: CpuSnapshot,
    /// full list, sorted (memory desc, pid asc) for stable row ordering.
    /// UI keys off pid, determinism avoids flicker on sort
    pub processes: Vec<ProcessRow>,
    /// top-N by memory, precomputed so the memory screen renders w/o
    /// post-processing. same pattern as dupes "top groups"
    pub top_by_memory: Vec<ProcessRow>,
    /// top-N by CPU. partial_cmp on f32 so NaNs sort last, never first.
    /// NaN at top would confuse the "kill runaway" UX
    pub top_by_cpu: Vec<ProcessRow>,
    pub process_count: usize,
    /// monotonic since stream start. lets UI detect dropped events,
    /// tauri's event bus coalesces rapid fires and we want the sparkline
    /// to draw a gap instead of a straight line
    pub tick: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pressure_is_zero_when_total_is_zero() {
        assert_eq!(MemorySnapshot::compute_pressure(0, 0), 0.0);
        assert_eq!(MemorySnapshot::compute_pressure(100, 0), 0.0);
    }

    #[test]
    fn pressure_clamps_above_one_hundred() {
        // used > total can happen on linux when "used" includes kernel buffers
        // the walker double-counts for a single tick
        assert_eq!(MemorySnapshot::compute_pressure(200, 100), 100.0);
    }

    #[test]
    fn pressure_clamps_below_zero() {
        assert_eq!(MemorySnapshot::compute_pressure(0, 100), 0.0);
    }

    #[test]
    fn pressure_midpoint() {
        let p = MemorySnapshot::compute_pressure(50, 100);
        assert!((p - 50.0).abs() < 0.01);
    }

    #[test]
    fn cpu_average_is_mean_of_cores() {
        let c = CpuSnapshot::from_per_core(vec![10.0, 20.0, 30.0, 40.0]);
        assert_eq!(c.core_count, 4);
        assert!((c.average_percent - 25.0).abs() < 0.01);
    }

    #[test]
    fn cpu_empty_input_is_zero_zero() {
        let c = CpuSnapshot::from_per_core(vec![]);
        assert_eq!(c.core_count, 0);
        assert_eq!(c.average_percent, 0.0);
    }

    #[test]
    fn cpu_clamps_out_of_range_readings() {
        let c = CpuSnapshot::from_per_core(vec![-5.0, 101.0, 50.0]);
        assert_eq!(c.per_core_percent, vec![0.0, 100.0, 50.0]);
        assert!((c.average_percent - 50.0).abs() < 0.01);
    }

    #[test]
    fn cpu_single_core_average_equals_value() {
        let c = CpuSnapshot::from_per_core(vec![73.5]);
        assert_eq!(c.core_count, 1);
        assert!((c.average_percent - 73.5).abs() < 0.01);
    }

    #[test]
    fn process_row_camelcase() {
        let row = ProcessRow {
            pid: 42,
            parent_pid: Some(1),
            name: "firefox".into(),
            command: "firefox --private".into(),
            user: Some("ish".into()),
            cpu_percent: 5.5,
            memory_bytes: 1024,
            start_time: 1_700_000_000,
            threads: Some(64),
        };
        let v = serde_json::to_value(&row).unwrap();
        assert!(v.get("parentPid").is_some());
        assert!(v.get("cpuPercent").is_some());
        assert!(v.get("memoryBytes").is_some());
        assert!(v.get("startTime").is_some());
    }

    #[test]
    fn memory_snapshot_camelcase() {
        let s = MemorySnapshot {
            total_bytes: 100,
            used_bytes: 40,
            free_bytes: 60,
            available_bytes: 55,
            swap_total_bytes: 10,
            swap_used_bytes: 2,
            pressure_percent: 40.0,
        };
        let v = serde_json::to_value(s).unwrap();
        for key in [
            "totalBytes",
            "usedBytes",
            "freeBytes",
            "availableBytes",
            "swapTotalBytes",
            "swapUsedBytes",
            "pressurePercent",
        ] {
            assert!(v.get(key).is_some(), "missing {key}");
        }
    }

    #[test]
    fn cpu_snapshot_camelcase() {
        let s = CpuSnapshot::from_per_core(vec![1.0, 2.0]);
        let v = serde_json::to_value(s).unwrap();
        assert!(v.get("coreCount").is_some());
        assert!(v.get("perCorePercent").is_some());
        assert!(v.get("averagePercent").is_some());
    }

    #[test]
    fn activity_snapshot_camelcase() {
        let snap = ActivitySnapshot {
            timestamp_ms: 1,
            memory: MemorySnapshot {
                total_bytes: 0,
                used_bytes: 0,
                free_bytes: 0,
                available_bytes: 0,
                swap_total_bytes: 0,
                swap_used_bytes: 0,
                pressure_percent: 0.0,
            },
            cpu: CpuSnapshot::from_per_core(vec![]),
            processes: vec![],
            top_by_memory: vec![],
            top_by_cpu: vec![],
            process_count: 0,
            tick: 0,
        };
        let v = serde_json::to_value(&snap).unwrap();
        for key in [
            "timestampMs",
            "memory",
            "cpu",
            "processes",
            "topByMemory",
            "topByCpu",
            "processCount",
            "tick",
        ] {
            assert!(v.get(key).is_some(), "missing {key}");
        }
    }
}
