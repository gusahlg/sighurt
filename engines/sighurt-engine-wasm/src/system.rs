//! System information capabilities for Oxide guest modules.
//!
//! Read-only, low-sensitivity host facts: colour scheme (dark/light),
//! locale, timezone, and battery status. Nothing here identifies the user
//! or touches the filesystem, so no permission prompt is required.

use anyhow::Result;
use wasmtime::{Caller, Linker};

use crate::capabilities::{write_guest, HostState};

/// `api_system_theme` result: preference unknown.
const THEME_UNKNOWN: u32 = 2;

/// Writes `s` if it fits in `out_cap` bytes. Returns its length, or 0 if it doesn't fit.
fn write_string(caller: &mut Caller<'_, HostState>, s: &str, out_ptr: u32, out_cap: u32) -> u32 {
    if s.len() > out_cap as usize || !write_guest(caller, out_ptr, s.as_bytes()) {
        return 0;
    }
    s.len() as u32
}

fn battery_info() -> Option<(f32, starship_battery::State)> {
    let manager = starship_battery::Manager::new().ok()?;
    let battery = manager.batteries().ok()?.next()?.ok()?;
    Some((battery.state_of_charge().value, battery.state()))
}

/// Register `api_system_*` and `api_battery_*` host functions.
pub fn register_system_functions(linker: &mut Linker<HostState>) -> Result<()> {
    // api_system_theme() -> u32
    //   0 = light, 1 = dark, 2 = unknown. The browser's theme doesn't reach engine processes.
    linker.func_wrap("oxide", "api_system_theme", || THEME_UNKNOWN)?;

    // api_system_locale(out_ptr, out_cap) -> u32
    //   Writes the BCP 47 locale tag (e.g. "en-IN"). Returns bytes written,
    //   0 if unknown or the buffer is too small.
    linker.func_wrap(
        "oxide",
        "api_system_locale",
        |mut caller: Caller<'_, HostState>, out_ptr: u32, out_cap: u32| -> u32 {
            let locale = sys_locale::get_locale().unwrap_or_default();
            write_string(&mut caller, &locale, out_ptr, out_cap)
        },
    )?;

    // api_system_timezone(out_ptr, out_cap) -> u32
    //   Writes the IANA timezone name (e.g. "Asia/Kolkata"). Returns bytes
    //   written, 0 if unknown or the buffer is too small.
    linker.func_wrap(
        "oxide",
        "api_system_timezone",
        |mut caller: Caller<'_, HostState>, out_ptr: u32, out_cap: u32| -> u32 {
            let tz = iana_time_zone::get_timezone().unwrap_or_default();
            write_string(&mut caller, &tz, out_ptr, out_cap)
        },
    )?;

    // api_system_timezone_offset() -> i32
    //   Minutes east of UTC for the current local time (e.g. +330 for IST).
    linker.func_wrap("oxide", "api_system_timezone_offset", || -> i32 {
        use chrono::Offset;
        chrono::Local::now().offset().fix().local_minus_utc() / 60
    })?;

    // api_battery_level() -> i32
    //   Charge percentage 0–100, or -1 when no battery is present.
    linker.func_wrap("oxide", "api_battery_level", || -> i32 {
        match battery_info() {
            Some((charge, _)) => (charge * 100.0).round().clamp(0.0, 100.0) as i32,
            None => -1,
        }
    })?;

    // api_battery_charging() -> i32
    //   1 = charging or full (on AC), 0 = discharging, -1 = unknown/no battery.
    linker.func_wrap("oxide", "api_battery_charging", || -> i32 {
        use starship_battery::State;
        match battery_info() {
            Some((_, State::Charging)) | Some((_, State::Full)) => 1,
            Some((_, State::Discharging)) | Some((_, State::Empty)) => 0,
            _ => -1,
        }
    })?;

    Ok(())
}
