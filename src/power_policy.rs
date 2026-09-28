//! StockOS-aware idle policy.
//!
//! The policy itself is platform neutral so its state transitions can be
//! tested on Windows. `StockPowerController` is only constructed by the
//! TrimUI SDL host and talks to the small, documented-by-use StockOS IPC
//! surface: `shmvar`, `/tmp/stay_awake`, `/tmp/stay_alive`,
//! `/tmp/system/set_brightness`, and the LED state files also used by StockOS
//! `keymon`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

pub const EFFICIENT_AFTER: Duration = Duration::from_secs(2);
const STOCK_LED_ROOT: &str = "/sys/class/led_anim";
const LED_SNAPSHOT_DIRECTORY: &str = "led-state-snapshot";
const LED_VALUE_MAX: u32 = 255;
const LED_EFFECT_ATTRIBUTES: [&str; 5] = [
    "effect_lr",
    "effect_m",
    "effect_f1",
    "effect_f2",
    "effect_rear",
];
const LED_ATTRIBUTES: [&str; 11] = [
    "max_scale",
    "max_scale_lr",
    "max_scale_f1f2",
    "max_scale_rear",
    "effect_lr",
    "effect_m",
    "effect_f1",
    "effect_f2",
    "effect_rear",
    "anim_frames_enable",
    "effect_enable",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeHost {
    StockOs,
    NextUi,
}

impl RuntimeHost {
    pub fn detect() -> Self {
        Self::from_value(std::env::var("BRICKWAVE_HOST").ok().as_deref())
    }

    fn from_value(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            Some(value) if value.eq_ignore_ascii_case("nextui") => Self::NextUi,
            _ => Self::StockOs,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdleStage {
    Active,
    Efficient,
    Timeout,
}

#[derive(Debug)]
pub struct IdlePolicy {
    always_keep_screen_on: bool,
    stock_timeout: Option<Duration>,
    last_activity: Instant,
}

impl IdlePolicy {
    pub fn new(always_keep_screen_on: bool, stock_timeout: Option<Duration>, now: Instant) -> Self {
        Self {
            always_keep_screen_on,
            stock_timeout,
            last_activity: now,
        }
    }

    pub fn note_activity(&mut self, now: Instant) {
        self.last_activity = now;
    }

    pub fn set_always_keep_screen_on(&mut self, enabled: bool, now: Instant) {
        if self.always_keep_screen_on != enabled {
            self.always_keep_screen_on = enabled;
            // Changing the policy is itself an explicit interaction. Grant a
            // complete StockOS timeout instead of dimming immediately.
            self.note_activity(now);
        }
    }

    pub const fn always_keep_screen_on(&self) -> bool {
        self.always_keep_screen_on
    }

    pub fn stage(&self, now: Instant) -> IdleStage {
        let idle_for = now.saturating_duration_since(self.last_activity);
        if idle_for < EFFICIENT_AFTER {
            return IdleStage::Active;
        }
        if self
            .stock_timeout
            .is_some_and(|timeout| idle_for >= timeout)
        {
            IdleStage::Timeout
        } else {
            IdleStage::Efficient
        }
    }
}

pub struct StockPowerController {
    host: RuntimeHost,
    stock_timeout: Option<Duration>,
    original_brightness: Option<u8>,
    dimmed: bool,
    stay_awake_owned: bool,
    stay_awake_marker: PathBuf,
    stay_alive_owned: bool,
    stay_alive_marker: PathBuf,
    brightness_marker: PathBuf,
    led_snapshot: Option<LedSnapshot>,
}

#[derive(Debug)]
struct LedSnapshot {
    root: PathBuf,
    marker_dir: PathBuf,
    values: Vec<(&'static str, u32)>,
}

impl LedSnapshot {
    fn capture(root: &Path, marker_dir: PathBuf) -> Option<Self> {
        let values = LED_ATTRIBUTES
            .iter()
            .filter_map(|name| {
                let value = fs::read_to_string(root.join(name)).ok()?;
                parse_led_value(&value).ok().map(|value| (*name, value))
            })
            .collect::<Vec<_>>();
        if values.is_empty() {
            return None;
        }

        let _ = fs::remove_dir_all(&marker_dir);
        let marker_ready = fs::create_dir_all(&marker_dir).is_ok()
            && values.iter().all(|(name, value)| {
                fs::write(marker_dir.join(name), format!("{value}\n")).is_ok()
            });
        if !marker_ready {
            let _ = fs::remove_dir_all(&marker_dir);
            println!("BRICKWAVE_LED_STATE_ERROR action=snapshot-marker");
        }

        let summary = values
            .iter()
            .map(|(name, value)| format!("{name}:{value}"))
            .collect::<Vec<_>>()
            .join(",");
        println!(
            "BRICKWAVE_LED_STATE capture=ready attributes={} crash_marker={marker_ready} values={summary}",
            values.len()
        );
        Some(Self {
            root: root.to_owned(),
            marker_dir,
            values,
        })
    }

    fn restore(&self, reason: &str) {
        let mut restored = 0;
        for (name, value) in &self.values {
            if fs::write(self.root.join(name), format!("{value}\n")).is_ok() {
                restored += 1;
            }
        }
        if restored == self.values.len() {
            println!("BRICKWAVE_LED_STATE restore=ok reason={reason} attributes={restored}");
        } else {
            println!(
                "BRICKWAVE_LED_STATE_ERROR action=restore reason={reason} restored={restored} expected={}",
                self.values.len()
            );
        }
    }

    fn remove_marker(&self) {
        let _ = fs::remove_dir_all(&self.marker_dir);
    }
}

impl StockPowerController {
    pub fn new() -> Self {
        let host = RuntimeHost::detect();
        let runtime_dir = std::env::var_os("BRICKWAVE_RUNTIME_DIR")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp/brickwave"));
        if host == RuntimeHost::NextUi {
            println!(
                "BRICKWAVE_POWER_POLICY host=nextui stockos_brightness=false stockos_led=false stockos_guards=false"
            );
            return Self {
                host,
                stock_timeout: None,
                original_brightness: None,
                dimmed: false,
                stay_awake_owned: false,
                stay_awake_marker: runtime_dir.join("stay-awake-owned"),
                stay_alive_owned: false,
                stay_alive_marker: runtime_dir.join("stay-alive-owned"),
                brightness_marker: runtime_dir.join("screen-brightness"),
                led_snapshot: None,
            };
        }
        let stock_timeout =
            query_stock_value("dimtime").and_then(|value| parse_stock_timeout(&value).ok());
        let original_brightness =
            query_stock_value("brightness").and_then(|value| parse_brightness(&value).ok());
        let led_snapshot = LedSnapshot::capture(
            Path::new(STOCK_LED_ROOT),
            runtime_dir.join(LED_SNAPSHOT_DIRECTORY),
        );
        // StockOS keymon leaves effect 1 active after its wake path. MainUI
        // normally clears that effect, but it is not running behind an SDL
        // application. Brickwave keeps the user's original values in the
        // snapshot and uses the ordinary idle/off state while it owns the UI.
        set_led_effects_off(Path::new(STOCK_LED_ROOT), "app-start");

        match stock_timeout {
            Some(timeout) => println!(
                "BRICKWAVE_POWER_POLICY timeout_source=stockos seconds={}",
                timeout.as_secs()
            ),
            None => println!("BRICKWAVE_POWER_POLICY timeout_source=stockos unavailable=true"),
        }
        match original_brightness {
            Some(value) => println!("BRICKWAVE_DISPLAY brightness_source=stockos value={value}"),
            None => println!("BRICKWAVE_DISPLAY brightness_source=stockos unavailable=true"),
        }

        Self {
            host,
            stock_timeout,
            original_brightness,
            dimmed: false,
            stay_awake_owned: false,
            stay_awake_marker: runtime_dir.join("stay-awake-owned"),
            stay_alive_owned: false,
            stay_alive_marker: runtime_dir.join("stay-alive-owned"),
            brightness_marker: runtime_dir.join("screen-brightness"),
            led_snapshot,
        }
    }

    pub const fn stock_timeout(&self) -> Option<Duration> {
        self.stock_timeout
    }

    pub const fn is_nextui(&self) -> bool {
        matches!(self.host, RuntimeHost::NextUi)
    }

    pub fn set_stay_awake(&mut self, enabled: bool) {
        if self.is_nextui() {
            return;
        }
        if enabled == self.stay_awake_owned {
            return;
        }
        if enabled {
            if let Some(parent) = self.stay_awake_marker.parent()
                && fs::create_dir_all(parent).is_err()
            {
                println!("BRICKWAVE_POWER_GUARD_ERROR action=marker-directory");
                return;
            }
            match fs::write("/tmp/stay_awake", b"1\n") {
                Ok(()) => {
                    if fs::write(&self.stay_awake_marker, b"brickwave\n").is_err() {
                        let _ = fs::remove_file("/tmp/stay_awake");
                        println!("BRICKWAVE_POWER_GUARD_ERROR action=marker-write");
                        return;
                    }
                    self.stay_awake_owned = true;
                    println!("BRICKWAVE_POWER_GUARD stay_awake=true");
                }
                Err(_) => println!("BRICKWAVE_POWER_GUARD_ERROR action=create-stay-awake"),
            }
        } else if self.stay_awake_owned {
            match fs::remove_file("/tmp/stay_awake") {
                Ok(()) => println!("BRICKWAVE_POWER_GUARD stay_awake=false"),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    println!("BRICKWAVE_POWER_GUARD stay_awake=false")
                }
                Err(_) => println!("BRICKWAVE_POWER_GUARD_ERROR action=remove-stay-awake"),
            }
            let _ = fs::remove_file(&self.stay_awake_marker);
            self.stay_awake_owned = false;
        }
    }

    /// Keep the Brickwave process alive while still allowing StockOS to own
    /// LCD off/on. `keymon` checks this marker separately from `stay_awake`:
    /// the former skips deep suspend, while the latter prevents the normal
    /// display timeout entirely.
    pub fn set_stay_alive(&mut self, enabled: bool) {
        if self.is_nextui() {
            return;
        }
        if enabled == self.stay_alive_owned {
            return;
        }
        if enabled {
            if let Some(parent) = self.stay_alive_marker.parent()
                && fs::create_dir_all(parent).is_err()
            {
                println!("BRICKWAVE_POWER_GUARD_ERROR action=stay-alive-marker-directory");
                return;
            }
            match fs::write("/tmp/stay_alive", b"1\n") {
                Ok(()) => {
                    if fs::write(&self.stay_alive_marker, b"brickwave\n").is_err() {
                        let _ = fs::remove_file("/tmp/stay_alive");
                        println!("BRICKWAVE_POWER_GUARD_ERROR action=stay-alive-marker-write");
                        return;
                    }
                    self.stay_alive_owned = true;
                    println!("BRICKWAVE_POWER_GUARD stay_alive=true");
                }
                Err(_) => {
                    println!("BRICKWAVE_POWER_GUARD_ERROR action=create-stay-alive")
                }
            }
        } else if self.stay_alive_owned {
            match fs::remove_file("/tmp/stay_alive") {
                Ok(()) => println!("BRICKWAVE_POWER_GUARD stay_alive=false"),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    println!("BRICKWAVE_POWER_GUARD stay_alive=false")
                }
                Err(_) => println!("BRICKWAVE_POWER_GUARD_ERROR action=remove-stay-alive"),
            }
            let _ = fs::remove_file(&self.stay_alive_marker);
            self.stay_alive_owned = false;
        }
    }

    pub fn dim(&mut self) {
        if self.is_nextui() {
            return;
        }
        if self.dimmed {
            return;
        }
        let Some(original) = self.original_brightness else {
            println!("BRICKWAVE_DISPLAY_DIM_ERROR reason=brightness-unavailable");
            return;
        };
        if let Some(parent) = self.brightness_marker.parent()
            && fs::create_dir_all(parent).is_err()
        {
            println!("BRICKWAVE_DISPLAY_DIM_ERROR reason=marker-directory");
            return;
        }
        if fs::write(&self.brightness_marker, original.to_string()).is_err() {
            println!("BRICKWAVE_DISPLAY_DIM_ERROR reason=marker-write");
            return;
        }
        if write_stock_brightness(0).is_err() {
            let _ = fs::remove_file(&self.brightness_marker);
            println!("BRICKWAVE_DISPLAY_DIM_ERROR reason=stockos-ipc-write");
            return;
        }
        self.dimmed = true;
        println!("BRICKWAVE_DISPLAY_DIM brightness=0 saved={original}");
    }

    pub fn restore_brightness(&mut self) {
        if self.is_nextui() {
            return;
        }
        if !self.dimmed {
            return;
        }
        let Some(original) = self.original_brightness else {
            return;
        };
        match write_stock_brightness(original) {
            Ok(()) => {
                self.dimmed = false;
                let _ = fs::remove_file(&self.brightness_marker);
                println!("BRICKWAVE_DISPLAY_RESTORE brightness={original}");
            }
            Err(()) => println!("BRICKWAVE_DISPLAY_RESTORE_ERROR reason=stockos-ipc-write"),
        }
    }

    /// StockOS `keymon` writes effect 1 to every LED group when it wakes the
    /// display. MainUI normally clears the temporary wake effect, but it is
    /// not present while an external SDL application owns the screen. Apply
    /// the normal off effect after keymon has completed; the pre-app values
    /// remain in `led_snapshot` and are restored when Brickwave exits.
    pub fn turn_leds_off_after_wake(&self) {
        if self.is_nextui() {
            return;
        }
        set_led_effects_off(Path::new(STOCK_LED_ROOT), "stockos-wake");
    }

    pub fn cleanup(&mut self) {
        if self.is_nextui() {
            return;
        }
        self.restore_brightness();
        self.set_stay_awake(false);
        self.set_stay_alive(false);
        if let Some(snapshot) = self.led_snapshot.take() {
            snapshot.restore("cleanup");
            // The stock MainUI idle state requested for this device has no
            // button illumination. Keep the saved scale/enable settings, but
            // do not leave a stale wake effect active while returning control
            // to MainUI.
            set_led_effects_off(Path::new(STOCK_LED_ROOT), "cleanup");
            snapshot.remove_marker();
        }
    }
}

impl Drop for StockPowerController {
    fn drop(&mut self) {
        self.cleanup();
    }
}

fn query_stock_value(name: &str) -> Option<String> {
    for executable in ["/usr/trimui/bin/shmvar", "/usr/trimui/bin/systemval"] {
        let Ok(output) = Command::new(executable).arg(name).output() else {
            continue;
        };
        if output.status.success() {
            let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}

fn parse_stock_timeout(value: &str) -> Result<Duration, ()> {
    let seconds = value.trim().parse::<u64>().map_err(|_| ())?;
    if !(1..=86_400).contains(&seconds) {
        return Err(());
    }
    Ok(Duration::from_secs(seconds))
}

fn parse_brightness(value: &str) -> Result<u8, ()> {
    let brightness = value.trim().parse::<u8>().map_err(|_| ())?;
    if brightness > 10 {
        return Err(());
    }
    Ok(brightness)
}

fn parse_led_value(value: &str) -> Result<u32, ()> {
    let value = value.trim().parse::<u32>().map_err(|_| ())?;
    if value > LED_VALUE_MAX {
        return Err(());
    }
    Ok(value)
}

fn set_led_effects_off(root: &Path, reason: &str) {
    let mut updated = 0;
    for name in LED_EFFECT_ATTRIBUTES {
        if fs::write(root.join(name), b"0\n").is_ok() {
            updated += 1;
        }
    }
    if updated == LED_EFFECT_ATTRIBUTES.len() {
        println!("BRICKWAVE_LED_STATE wake_effects=off reason={reason} attributes={updated}");
    } else {
        println!(
            "BRICKWAVE_LED_STATE_ERROR action=wake-effects-off reason={reason} updated={updated} expected={}",
            LED_EFFECT_ATTRIBUTES.len()
        );
    }
}

fn write_stock_brightness(value: u8) -> Result<(), ()> {
    if value > 10 {
        return Err(());
    }
    fs::write("/tmp/system/set_brightness", value.to_string()).map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::{
        EFFICIENT_AFTER, IdlePolicy, IdleStage, LedSnapshot, RuntimeHost, parse_brightness,
        parse_led_value, parse_stock_timeout, set_led_effects_off,
    };
    use std::fs;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    #[test]
    fn idle_policy_has_active_efficient_and_timeout_stages() {
        let now = Instant::now();
        let policy = IdlePolicy::new(false, Some(Duration::from_secs(30)), now);
        assert_eq!(policy.stage(now), IdleStage::Active);
        assert_eq!(policy.stage(now + EFFICIENT_AFTER), IdleStage::Efficient);
        assert_eq!(
            policy.stage(now + Duration::from_secs(30)),
            IdleStage::Timeout
        );
    }

    #[test]
    fn missing_stock_timeout_stays_efficient() {
        let now = Instant::now();
        let policy = IdlePolicy::new(false, None, now);
        assert_eq!(
            policy.stage(now + Duration::from_secs(86_400)),
            IdleStage::Efficient
        );
    }

    #[test]
    fn changing_mode_grants_a_fresh_timeout() {
        let now = Instant::now();
        let mut policy = IdlePolicy::new(false, Some(Duration::from_secs(30)), now);
        let later = now + Duration::from_secs(40);
        assert_eq!(policy.stage(later), IdleStage::Timeout);
        policy.set_always_keep_screen_on(true, later);
        assert!(policy.always_keep_screen_on());
        assert_eq!(policy.stage(later), IdleStage::Active);
    }

    #[test]
    fn validates_stock_values_without_guessing() {
        assert_eq!(parse_stock_timeout("60\n").unwrap().as_secs(), 60);
        assert!(parse_stock_timeout("0").is_err());
        assert!(parse_stock_timeout("86401").is_err());
        assert_eq!(parse_brightness("0").unwrap(), 0);
        assert_eq!(parse_brightness("10").unwrap(), 10);
        assert!(parse_brightness("11").is_err());
        assert_eq!(parse_led_value("5\n").unwrap(), 5);
        assert_eq!(parse_led_value("255").unwrap(), 255);
        assert!(parse_led_value("256").is_err());
        assert!(parse_led_value("on").is_err());
    }

    #[test]
    fn runtime_host_requires_explicit_nextui_value() {
        assert_eq!(RuntimeHost::from_value(Some("nextui")), RuntimeHost::NextUi);
        assert_eq!(
            RuntimeHost::from_value(Some(" NEXTUI ")),
            RuntimeHost::NextUi
        );
        assert_eq!(RuntimeHost::from_value(None), RuntimeHost::StockOs);
        assert_eq!(
            RuntimeHost::from_value(Some("stockos")),
            RuntimeHost::StockOs
        );
        assert_eq!(
            RuntimeHost::from_value(Some("unknown")),
            RuntimeHost::StockOs
        );
    }

    #[test]
    fn led_snapshot_restores_only_valid_captured_attributes() {
        let unique = format!(
            "brickwave-led-snapshot-{}-{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        );
        let base = std::env::temp_dir().join(unique);
        let root = base.join("led_anim");
        let marker = base.join("run").join("led-state-snapshot");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("effect_lr"), "0\n").unwrap();
        fs::write(root.join("effect_m"), "7\n").unwrap();
        fs::write(root.join("effect_f1"), "invalid\n").unwrap();

        let snapshot = LedSnapshot::capture(&root, marker.clone()).unwrap();
        assert_eq!(snapshot.values.len(), 2);
        assert_eq!(fs::read_to_string(marker.join("effect_lr")).unwrap(), "0\n");
        assert!(!marker.join("effect_f1").exists());

        fs::write(root.join("effect_lr"), "1\n").unwrap();
        fs::write(root.join("effect_m"), "1\n").unwrap();
        snapshot.restore("test");
        assert_eq!(fs::read_to_string(root.join("effect_lr")).unwrap(), "0\n");
        assert_eq!(fs::read_to_string(root.join("effect_m")).unwrap(), "7\n");

        snapshot.remove_marker();
        assert!(!marker.exists());
        let _ = fs::remove_dir_all(PathBuf::from(base));
    }

    #[test]
    fn wake_led_cleanup_turns_effects_off_without_changing_other_attributes() {
        let unique = format!(
            "brickwave-led-off-{}-{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        fs::create_dir_all(&root).unwrap();
        for name in super::LED_EFFECT_ATTRIBUTES {
            fs::write(root.join(name), "1\n").unwrap();
        }
        fs::write(root.join("max_scale"), "5\n").unwrap();

        set_led_effects_off(&root, "test");

        for name in super::LED_EFFECT_ATTRIBUTES {
            assert_eq!(fs::read_to_string(root.join(name)).unwrap(), "0\n");
        }
        assert_eq!(fs::read_to_string(root.join("max_scale")).unwrap(), "5\n");
        let _ = fs::remove_dir_all(root);
    }
}
