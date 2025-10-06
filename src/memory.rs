/*
 * Digital Picture Frame - A fullscreen photo slideshow application
 * Copyright (C) 2025 Daniel Mikusa
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU Affero General Public License as published
 * by the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU Affero General Public License for more details.
 *
 * You should have received a copy of the GNU Affero General Public License
 * along with this program.  If not, see <https://www.gnu.org/licenses/>.
 */

use log::{debug, info};
use std::time::{Duration, Instant};
use sysinfo::{Pid, System};

pub struct MemoryMonitor {
    system: System,
    process_id: Pid,
    last_check: Instant,
    peak_memory: u64,
    initial_memory: u64,
}

impl MemoryMonitor {
    pub fn new() -> Self {
        let mut system = System::new_all();
        system.refresh_all();

        let process_id = Pid::from_u32(std::process::id());
        let initial_memory_bytes = system.process(process_id).map(|p| p.memory()).unwrap_or(0);
        let initial_memory = initial_memory_bytes / 1024; // Convert bytes to KB
        info!(
            "Memory Monitor initialized. Initial memory usage: {}",
            Self::format_memory_human(initial_memory)
        );

        Self {
            system,
            process_id,
            last_check: Instant::now(),
            peak_memory: initial_memory,
            initial_memory,
        }
    }

    pub fn check_memory(&mut self) -> MemoryStats {
        self.system.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::Some(&[self.process_id]),
            true,
            sysinfo::ProcessRefreshKind::nothing().with_memory(),
        );

        let current_memory_bytes = self
            .system
            .process(self.process_id)
            .map(|p| p.memory())
            .unwrap_or(0);
        let current_memory = current_memory_bytes / 1024; // Convert bytes to KB

        let virtual_memory_bytes = self
            .system
            .process(self.process_id)
            .map(|p| p.virtual_memory())
            .unwrap_or(0);
        let virtual_memory = virtual_memory_bytes / 1024; // Convert bytes to KB

        if current_memory > self.peak_memory {
            self.peak_memory = current_memory;
        }

        let stats = MemoryStats {
            current_memory_kb: current_memory,
            virtual_memory_kb: virtual_memory,
            peak_memory_kb: self.peak_memory,
            memory_growth_kb: current_memory.saturating_sub(self.initial_memory),
        };

        debug!(
            "Memory usage: {} (virtual: {}, peak: {}, growth: +{})",
            Self::format_memory_human(stats.current_memory_kb),
            Self::format_memory_human(stats.virtual_memory_kb),
            Self::format_memory_human(stats.peak_memory_kb),
            Self::format_memory_human(stats.memory_growth_kb)
        );

        self.last_check = Instant::now();
        stats
    }

    pub fn log_memory_periodically(&mut self, interval: Duration) {
        if self.last_check.elapsed() >= interval {
            let stats = self.check_memory();
            info!(
                "Memory check: Current: {}, Peak: {}, Growth: +{}",
                Self::format_memory_human(stats.current_memory_kb),
                Self::format_memory_human(stats.peak_memory_kb),
                Self::format_memory_human(stats.memory_growth_kb)
            );
        }
    }

    pub fn format_memory_mb(kb: u64) -> String {
        format!("{:.1} MB", kb as f64 / 1024.0)
    }

    pub fn format_memory_human(kb: u64) -> String {
        if kb >= 1024 * 1024 {
            format!("{:.2} GB", kb as f64 / (1024.0 * 1024.0))
        } else if kb >= 1024 {
            format!("{:.1} MB", kb as f64 / 1024.0)
        } else {
            format!("{} KB", kb)
        }
    }
}

#[derive(Debug, Clone)]
pub struct MemoryStats {
    pub current_memory_kb: u64,
    pub virtual_memory_kb: u64,
    pub peak_memory_kb: u64,
    pub memory_growth_kb: u64,
}

impl MemoryStats {
    pub fn current_memory_mb(&self) -> f64 {
        self.current_memory_kb as f64 / 1024.0
    }

    pub fn peak_memory_mb(&self) -> f64 {
        self.peak_memory_kb as f64 / 1024.0
    }

    pub fn memory_growth_mb(&self) -> f64 {
        self.memory_growth_kb as f64 / 1024.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_memory_human() {
        // Test KB values (less than 1024 KB)
        assert_eq!(MemoryMonitor::format_memory_human(0), "0 KB");
        assert_eq!(MemoryMonitor::format_memory_human(512), "512 KB");
        assert_eq!(MemoryMonitor::format_memory_human(1023), "1023 KB");

        // Test MB values (1024 KB and above, but less than 1024*1024 KB)
        assert_eq!(MemoryMonitor::format_memory_human(1024), "1.0 MB");
        assert_eq!(MemoryMonitor::format_memory_human(1536), "1.5 MB");
        assert_eq!(MemoryMonitor::format_memory_human(2048), "2.0 MB");
        assert_eq!(MemoryMonitor::format_memory_human(25000), "24.4 MB");
        assert_eq!(MemoryMonitor::format_memory_human(100000), "97.7 MB");
        assert_eq!(MemoryMonitor::format_memory_human(1048575), "1024.0 MB"); // Just under 1 GB

        // Test GB values (1024*1024 KB and above)
        assert_eq!(MemoryMonitor::format_memory_human(1048576), "1.00 GB"); // Exactly 1 GB
        assert_eq!(MemoryMonitor::format_memory_human(2097152), "2.00 GB"); // Exactly 2 GB
        assert_eq!(MemoryMonitor::format_memory_human(1572864), "1.50 GB"); // 1.5 GB
    }

    #[test]
    fn test_format_memory_mb() {
        assert_eq!(MemoryMonitor::format_memory_mb(1024), "1.0 MB");
        assert_eq!(MemoryMonitor::format_memory_mb(2048), "2.0 MB");
        assert_eq!(MemoryMonitor::format_memory_mb(1536), "1.5 MB");
    }
}
