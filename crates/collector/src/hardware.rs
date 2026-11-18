//! Host hardware values from Linux sysfs and procfs: CPU and GPU use, temperatures, APU power and memory.
//! Values the host does not offer stay None, so the dashboard shows a dash.

use crate::types::Sample;
use std::path::{Path, PathBuf};

pub struct Hardware {
    root: PathBuf,
    /// (total, idle) jiffies of the last read of /proc/stat.
    prev_cpu: Option<(u64, u64)>,
}

impl Default for Hardware {
    fn default() -> Self {
        Self::new("/")
    }
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_owned())
}

fn read_f64(path: &Path) -> Option<f64> {
    read(path)?.parse().ok()
}

/// NPU use from the GPU card's gpu_metrics: the mean of `average_ipu_activity[8]` (per-column busy %, 0-100) in
/// format 3.0 (APUs with an XDNA NPU). The in-tree amdxdna driver gives no busy counter, the firmware does.
/// Columns the firmware does not fill (0xFFFF) are skipped; other formats give None.
fn npu_use(path: &Path) -> Option<f64> {
    let b = std::fs::read(path).ok()?;
    if b.len() < 62 || b[2] != 3 || b[3] != 0 {
        return None;
    }
    let cols: Vec<f64> = b[46..62]
        .chunks(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .filter(|v| *v <= 100)
        .map(f64::from)
        .collect();
    (!cols.is_empty()).then(|| cols.iter().sum::<f64>() / cols.len() as f64 / 100.0)
}

impl Hardware {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            prev_cpu: None,
        }
    }

    /// The first hwmon device with this name, such as k10temp, amdgpu or nvme.
    fn hwmon(&self, name: &str) -> Option<PathBuf> {
        let mut dirs: Vec<_> = std::fs::read_dir(self.root.join("sys/class/hwmon"))
            .ok()?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        dirs.sort();
        dirs.into_iter()
            .find(|d| read(&d.join("name")).as_deref() == Some(name))
    }

    /// The device folder of the first amdgpu card.
    fn gpu(&self) -> Option<PathBuf> {
        let mut dirs: Vec<_> = std::fs::read_dir(self.root.join("sys/class/drm"))
            .ok()?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("card") && !n.contains('-'))
            })
            .collect();
        dirs.sort();
        dirs.into_iter()
            .map(|d| d.join("device"))
            .find(|d| d.join("gpu_busy_percent").exists())
    }

    /// Share of CPU time not idle since the last call. None on the first call.
    fn cpu_use(&mut self) -> Option<f64> {
        let stat = read(&self.root.join("proc/stat"))?;
        let jiffies: Vec<u64> = stat
            .lines()
            .next()?
            .split_whitespace()
            .skip(1)
            .filter_map(|v| v.parse().ok())
            .collect();
        // idle + iowait
        let idle = jiffies.get(3)? + jiffies.get(4).copied().unwrap_or(0);
        let total: u64 = jiffies.iter().take(8).sum();
        let previous = self.prev_cpu.replace((total, idle));
        let (pt, pi) = previous?;
        let dt = total.checked_sub(pt).filter(|d| *d > 0)?;
        Some(1.0 - idle.saturating_sub(pi) as f64 / dt as f64)
    }

    /// Fills the hardware fields of a Sample.
    pub fn apply(&mut self, s: &mut Sample) {
        s.cpu_use = self.cpu_use();
        let celsius = |dir: &Option<PathBuf>| {
            dir.as_ref()
                .and_then(|d| read_f64(&d.join("temp1_input")))
                .map(|v| v / 1000.0)
        };
        s.cpu_temp = celsius(&self.hwmon("k10temp"));
        s.nvme_temp = celsius(&self.hwmon("nvme"));
        let amdgpu = self.hwmon("amdgpu");
        s.gpu_temp = celsius(&amdgpu);
        s.apu_power = amdgpu.as_ref().and_then(|d| {
            read_f64(&d.join("power1_average"))
                .or_else(|| read_f64(&d.join("power1_input")))
                .map(|v| v / 1e6)
        });
        s.gpu_clock = amdgpu
            .as_ref()
            .and_then(|d| read_f64(&d.join("freq1_input")))
            .map(|v| v / 1e6);
        let gpu = self.gpu();
        let gpu_value = |file: &str| gpu.as_ref().and_then(|d| read_f64(&d.join(file)));
        s.gpu_use = gpu_value("gpu_busy_percent").map(|v| v / 100.0);
        s.npu_use = gpu.as_ref().and_then(|d| npu_use(&d.join("gpu_metrics")));
        // Strix Halo keeps most GPU memory in GTT (system memory the GPU maps), so count VRAM and GTT together.
        let sum = |a: Option<f64>, b: Option<f64>| a.zip(b).map(|(a, b)| a + b);
        s.gpu_mem_used = sum(
            gpu_value("mem_info_vram_used"),
            gpu_value("mem_info_gtt_used"),
        );
        s.gpu_mem_total = sum(
            gpu_value("mem_info_vram_total"),
            gpu_value("mem_info_gtt_total"),
        );
        if let Some(meminfo) = read(&self.root.join("proc/meminfo")) {
            let kib = |key: &str| {
                meminfo
                    .lines()
                    .find(|l| l.starts_with(key))
                    .and_then(|l| l.split_whitespace().nth(1)?.parse::<f64>().ok())
                    .map(|v| v * 1024.0)
            };
            s.ram_total = kib("MemTotal:");
            s.ram_used = s.ram_total.zip(kib("MemAvailable:")).map(|(t, a)| t - a);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, path: &str, text: &str) {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn reads_strix_halo_sensors_and_cpu_use_from_deltas() {
        let root = std::env::temp_dir().join(format!("hd-hw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write(&root, "sys/class/hwmon/hwmon0/name", "r8169_0_6100:00\n");
        write(&root, "sys/class/hwmon/hwmon0/temp1_input", "50000\n");
        write(&root, "sys/class/hwmon/hwmon2/name", "nvme\n");
        write(&root, "sys/class/hwmon/hwmon2/temp1_input", "38850\n");
        write(&root, "sys/class/hwmon/hwmon3/name", "k10temp\n");
        write(&root, "sys/class/hwmon/hwmon3/temp1_input", "43750\n");
        write(&root, "sys/class/hwmon/hwmon5/name", "amdgpu\n");
        write(&root, "sys/class/hwmon/hwmon5/temp1_input", "42000\n");
        write(&root, "sys/class/hwmon/hwmon5/power1_average", "61000000\n");
        write(&root, "sys/class/hwmon/hwmon5/freq1_input", "2900000000\n");
        write(&root, "sys/class/drm/card0-DP-1/status", "disconnected\n");
        write(&root, "sys/class/drm/card0/device/gpu_busy_percent", "37\n");
        write(
            &root,
            "sys/class/drm/card0/device/mem_info_vram_used",
            "1000\n",
        );
        write(
            &root,
            "sys/class/drm/card0/device/mem_info_vram_total",
            "4000\n",
        );
        write(
            &root,
            "sys/class/drm/card0/device/mem_info_gtt_used",
            "39938920448\n",
        );
        write(
            &root,
            "sys/class/drm/card0/device/mem_info_gtt_total",
            "120000000000\n",
        );
        // gpu_metrics v3.0: 4-byte header, 19 temperatures, gfx and vcn busy, then the 8 NPU columns at byte 46.
        let mut metrics = vec![0u8; 264];
        metrics[..4].copy_from_slice(&[8, 1, 3, 0]);
        for (i, v) in [100u16, 50, 0, 0, 30, 0, 0, 0xFFFF].iter().enumerate() {
            metrics[46 + 2 * i..48 + 2 * i].copy_from_slice(&v.to_le_bytes());
        }
        std::fs::write(root.join("sys/class/drm/card0/device/gpu_metrics"), metrics).unwrap();
        write(
            &root,
            "proc/meminfo",
            "MemTotal:       128134844 kB\nMemFree: 1 kB\nMemAvailable:   73604468 kB\n",
        );
        write(
            &root,
            "proc/stat",
            "cpu  100 0 100 700 100 0 0 0 0 0\ncpu0 1 2 3\n",
        );

        let mut hw = Hardware::new(&root);
        let mut s = Sample::default();
        hw.apply(&mut s);
        assert_eq!(s.cpu_use, None);
        assert_eq!(s.cpu_temp, Some(43.75));
        assert_eq!(s.gpu_temp, Some(42.0));
        assert_eq!(s.nvme_temp, Some(38.85));
        assert_eq!(s.apu_power, Some(61.0));
        assert_eq!(s.gpu_clock, Some(2900.0));
        assert_eq!(s.gpu_use, Some(0.37));
        assert_eq!(s.npu_use, Some(180.0 / 7.0 / 100.0));
        assert_eq!(s.gpu_mem_used, Some(39938921448.0));
        assert_eq!(s.gpu_mem_total, Some(120000004000.0));
        assert_eq!(s.ram_total, Some(128134844.0 * 1024.0));
        assert_eq!(s.ram_used, Some((128134844.0 - 73604468.0) * 1024.0));

        // 100 more jiffies, 25 of them idle or iowait.
        write(&root, "proc/stat", "cpu  150 0 125 720 105 0 0 0 0 0\n");
        hw.apply(&mut s);
        assert_eq!(s.cpu_use, Some(0.75));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn missing_sensors_stay_none() {
        let mut hw = Hardware::new("/nonexistent-hd-root");
        let mut s = Sample::default();
        hw.apply(&mut s);
        hw.apply(&mut s);
        assert!(s.cpu_use.is_none() && s.cpu_temp.is_none() && s.gpu_use.is_none());
        assert!(s.apu_power.is_none() && s.ram_used.is_none() && s.gpu_mem_used.is_none());
        assert!(s.npu_use.is_none());
    }
}
