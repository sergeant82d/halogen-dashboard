//! Host hardware values from Linux sysfs and procfs: CPU, GPU and NPU use, temperatures, APU power, memory, disk
//! and network traffic.
//! Values the host does not offer stay None, so the dashboard shows a dash.

use crate::types::Sample;
use std::path::{Path, PathBuf};

pub struct Hardware {
    root: PathBuf,
    /// (total, idle) jiffies of the last read of /proc/stat.
    prev_cpu: Option<(u64, u64)>,
    /// Sample time (ms) and byte counters of the last disk and network read (see `io_counters`).
    prev_io: Option<(i64, [Option<u64>; 6])>,
    /// The host's /proc/net/dev. In a container /proc/net/dev is the container's own network, so the host's file is
    /// mounted in and named by HALOGEN_DASHBOARD_NET_DEV (the Quadlet mounts /proc/1/net/dev).
    net_dev: PathBuf,
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
        let root = root.into();
        let net_dev = std::env::var_os("HALOGEN_DASHBOARD_NET_DEV")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("proc/net/dev"));
        Self {
            root,
            prev_cpu: None,
            prev_io: None,
            net_dev,
        }
    }

    /// Bytes read and written since boot by SSDs and by spinning disks (whole disks with a device in /sys/block,
    /// split by queue/rotational), then bytes received and sent by the network interfaces other than loopback and
    /// virtual ones. A kind the host does not have stays None.
    fn io_counters(&self) -> [Option<u64>; 6] {
        let mut c = [None; 6];
        let mut add = |i: usize, v: u64| c[i] = Some(c[i].unwrap_or(0) + v);
        if let Some(stats) = read(&self.root.join("proc/diskstats")) {
            for line in stats.lines() {
                let f: Vec<&str> = line.split_whitespace().collect();
                let Some(name) = f.get(2) else { continue };
                let block = self.root.join("sys/block").join(name);
                if !block.join("device").exists() {
                    continue; // partitions, loop, device-mapper
                }
                let sectors = |i: usize| f.get(i).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0) * 512;
                let k = if read(&block.join("queue/rotational")).as_deref() == Some("1") { 2 } else { 0 };
                add(k, sectors(5));
                add(k + 1, sectors(9));
            }
        }
        if let Some(dev) = read(&self.net_dev) {
            for line in dev.lines().skip(2) {
                let Some((name, rest)) = line.split_once(':') else { continue };
                let name = name.trim();
                if name == "lo"
                    || ["veth", "br-", "docker", "podman", "virbr", "tap", "tun", "cni"]
                        .iter()
                        .any(|p| name.starts_with(p))
                {
                    continue;
                }
                let f: Vec<u64> = rest.split_whitespace().filter_map(|v| v.parse().ok()).collect();
                if f.len() >= 9 {
                    add(4, f[0]);
                    add(5, f[8]);
                }
            }
        }
        c
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
        // Bytes per second since the last Sample; None on the first call and when a counter went back (reboot).
        let io = self.io_counters();
        let rates: [Option<f64>; 6] = match self.prev_io.replace((s.t, io)) {
            Some((pt, prev)) if s.t > pt => std::array::from_fn(|i| {
                let d = io[i]?.checked_sub(prev[i]?)?;
                Some(d as f64 * 1000.0 / (s.t - pt) as f64)
            }),
            _ => [None; 6],
        };
        [s.ssd_read, s.ssd_write, s.hdd_read, s.hdd_write, s.net_in, s.net_out] = rates;
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

        // An SSD with a partition, a spinning disk, a loop device; loopback, a NIC and a container veth.
        write(&root, "sys/block/nvme0n1/device/x", "");
        write(&root, "sys/block/nvme0n1/queue/rotational", "0\n");
        write(&root, "sys/block/sda/device/x", "");
        write(&root, "sys/block/sda/queue/rotational", "1\n");
        write(&root, "sys/block/loop0/queue/rotational", "0\n");
        let diskstats = |ssd: u64, hdd: u64| {
            format!(
                " 259 0 nvme0n1 1 0 {ssd} 0 1 0 {} 0 0 0 0\n 259 1 nvme0n1p1 1 0 {ssd} 0 1 0 {ssd} 0 0 0 0\n   8 0 sda 1 0 {hdd} 0 1 0 0 0 0 0 0\n   7 0 loop0 1 0 999 0 1 0 999 0 0 0 0\n",
                ssd * 2
            )
        };
        let netdev = |rx: u64, tx: u64| {
            format!(
                "Inter-| head\n face | head\n    lo: 999 1 0 0 0 0 0 0 999 1 0 0 0 0 0 0\nenp98s0: {rx} 1 0 0 0 0 0 0 {tx} 1 0 0 0 0 0 0\nveth0: 999 1 0 0 0 0 0 0 999 1 0 0 0 0 0 0\n"
            )
        };
        write(&root, "proc/diskstats", &diskstats(1000, 2000));
        write(&root, "proc/net/dev", &netdev(5000, 6000));

        let mut hw = Hardware::new(&root);
        let mut s = Sample {
            t: 10_000,
            ..Default::default()
        };
        hw.apply(&mut s);
        assert_eq!(s.cpu_use, None);
        assert_eq!(s.ssd_read, None);
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
        // 2 s later: SSD +1000 sectors read, +2000 written; HDD +4000 read; 2000 bytes in, 8000 out.
        write(&root, "proc/diskstats", &diskstats(2000, 6000));
        write(&root, "proc/net/dev", &netdev(7000, 14000));
        s.t = 12_000;
        hw.apply(&mut s);
        assert_eq!(s.cpu_use, Some(0.75));
        assert_eq!(s.ssd_read, Some(1000.0 * 512.0 / 2.0));
        assert_eq!(s.ssd_write, Some(2000.0 * 512.0 / 2.0));
        assert_eq!(s.hdd_read, Some(4000.0 * 512.0 / 2.0));
        assert_eq!(s.hdd_write, Some(0.0));
        assert_eq!(s.net_in, Some(1000.0));
        assert_eq!(s.net_out, Some(4000.0));
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
        assert!(s.npu_use.is_none() && s.ssd_read.is_none() && s.net_in.is_none());
    }
}
