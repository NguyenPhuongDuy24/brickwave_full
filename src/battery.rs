//! Low-frequency battery sampling for the StockOS status indicator.
//!
//! Linux power-supply entries are discovered by their `type` file rather than
//! by a device-specific directory name. Windows preview intentionally reports
//! unavailable instead of fabricating a percentage.

use std::path::Path;
use std::time::{Duration, Instant};

const POWER_SUPPLY_ROOT: &str = "/sys/class/power_supply";
const REFRESH_INTERVAL: Duration = Duration::from_secs(15);

pub struct BatteryMonitor {
    percentage: Option<u8>,
    next_refresh: Instant,
}

impl BatteryMonitor {
    pub fn new() -> Self {
        Self {
            percentage: None,
            next_refresh: Instant::now(),
        }
    }

    pub fn poll(&mut self, enabled: bool) -> Option<u8> {
        if !enabled {
            return self.percentage;
        }
        let now = Instant::now();
        if now < self.next_refresh {
            return self.percentage;
        }
        let previous = self.percentage;
        self.percentage = read_capacity_at(Path::new(POWER_SUPPLY_ROOT));
        self.next_refresh = now + REFRESH_INTERVAL;
        if self.percentage != previous {
            match self.percentage {
                Some(value) => println!("BRICKWAVE_BATTERY percentage={value}"),
                None => println!("BRICKWAVE_BATTERY state=unavailable"),
            }
        }
        self.percentage
    }
}

fn read_capacity_at(root: &Path) -> Option<u8> {
    let mut supplies: Vec<_> = std::fs::read_dir(root).ok()?.flatten().collect();
    supplies.sort_by_key(|entry| entry.file_name());
    for supply in supplies {
        let path = supply.path();
        let Ok(supply_type) = std::fs::read_to_string(path.join("type")) else {
            continue;
        };
        if !supply_type.trim().eq_ignore_ascii_case("battery") {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(path.join("capacity")) else {
            continue;
        };
        if let Ok(value) = raw.trim().parse::<u8>()
            && value <= 100
        {
            return Some(value);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::read_capacity_at;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_root(name: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("brickwave-battery-{name}-{nonce}"));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn discovers_battery_by_type_and_validates_percentage() {
        let root = test_root("valid");
        let mains = root.join("usb");
        let battery = root.join("axp-battery");
        fs::create_dir_all(&mains).unwrap();
        fs::create_dir_all(&battery).unwrap();
        fs::write(mains.join("type"), "Mains\n").unwrap();
        fs::write(mains.join("capacity"), "99\n").unwrap();
        fs::write(battery.join("type"), "Battery\n").unwrap();
        fs::write(battery.join("capacity"), "73\n").unwrap();
        assert_eq!(read_capacity_at(&root), Some(73));
        fs::write(battery.join("capacity"), "101\n").unwrap();
        assert_eq!(read_capacity_at(&root), None);
        let _ = fs::remove_dir_all(root);
    }
}
