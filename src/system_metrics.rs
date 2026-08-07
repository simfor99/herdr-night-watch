//! Best-effort Windows system telemetry for the optional live-status footer.
//!
//! These values are informational only. They are deliberately kept outside the
//! Herdr watcher and never participate in the shutdown decision.

use std::mem::zeroed;
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::ptr::null_mut;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

use windows_sys::Win32::Foundation::{BOOL, FILETIME};
use windows_sys::Win32::System::Performance::{
    PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE,
    PDH_MORE_DATA, PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData,
    PdhGetFormattedCounterArrayW, PdhOpenQueryW,
};
use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows_sys::Win32::System::Threading::GetSystemTimes;

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemMetrics {
    pub cpu_percent: Option<u8>,
    pub gpu_percent: Option<u8>,
    pub vram_used_bytes: Option<u64>,
    pub vram_percent: Option<u8>,
    pub ram_percent: Option<u8>,
    pub gpu_watts: Option<u16>,
    pub gpu_power_percent: Option<u8>,
}

#[derive(Default)]
struct CpuSample {
    idle: u64,
    kernel: u64,
    user: u64,
}

pub struct Sampler {
    cpu_previous: Option<CpuSample>,
    query: Option<PdhQuery>,
}

impl Sampler {
    pub fn new() -> Self {
        Self {
            cpu_previous: None,
            query: PdhQuery::new(),
        }
    }

    pub fn sample(&mut self) -> SystemMetrics {
        let cpu_percent = sample_cpu(&mut self.cpu_previous);
        let ram_percent = sample_ram();
        let (gpu_percent, pdh_vram_used_bytes) = self
            .query
            .as_mut()
            .map(PdhQuery::sample)
            .unwrap_or_default();
        let nvidia = read_nvidia_gpu_telemetry();
        let gpu_watts = nvidia.as_ref().and_then(|telemetry| telemetry.power_watts);
        let gpu_power_percent = nvidia
            .as_ref()
            .and_then(|telemetry| telemetry.power_percent);
        let vram_used_bytes = nvidia
            .as_ref()
            .and_then(|telemetry| telemetry.vram_used_bytes)
            .or(pdh_vram_used_bytes);
        SystemMetrics {
            cpu_percent,
            gpu_percent,
            vram_used_bytes,
            vram_percent: nvidia.as_ref().and_then(|telemetry| telemetry.vram_percent),
            ram_percent,
            gpu_watts,
            gpu_power_percent,
        }
    }
}

struct PdhQuery {
    handle: isize,
    gpu: Option<isize>,
    vram_used: Option<isize>,
}

impl PdhQuery {
    fn new() -> Option<Self> {
        let mut handle = 0isize;
        let status = unsafe { PdhOpenQueryW(std::ptr::null(), 0, &mut handle) };
        if status != 0 {
            return None;
        }

        let query = Self {
            handle,
            gpu: add_counter(handle, r"\GPU Engine(*)\Utilization Percentage"),
            vram_used: add_counter(handle, r"\GPU Adapter Memory(*)\Dedicated Usage"),
        };
        if query.gpu.is_none() && query.vram_used.is_none() {
            unsafe { PdhCloseQuery(handle) };
            None
        } else {
            Some(query)
        }
    }

    fn sample(&mut self) -> (Option<u8>, Option<u64>) {
        if unsafe { PdhCollectQueryData(self.handle) } != 0 {
            return (None, None);
        }

        let gpu = self.gpu.and_then(read_max);
        let vram = self
            .vram_used
            .and_then(read_sum)
            .map(|value| value.round().clamp(0.0, u64::MAX as f64) as u64);
        (gpu.map(percentage), vram)
    }
}

impl Drop for PdhQuery {
    fn drop(&mut self) {
        if self.handle != 0 {
            unsafe { PdhCloseQuery(self.handle) };
        }
    }
}

fn add_counter(query: isize, path: &str) -> Option<isize> {
    let path = wide(path);
    let mut counter = 0isize;
    let status = unsafe { PdhAddEnglishCounterW(query, path.as_ptr(), 0, &mut counter) };
    (status == 0).then_some(counter)
}

fn read_max(counter: isize) -> Option<f64> {
    formatted_values(counter)
        .into_iter()
        .map(|value| value.max(0.0))
        .max_by(|left, right| left.total_cmp(right))
}

fn read_sum(counter: isize) -> Option<f64> {
    let values = formatted_values(counter);
    (!values.is_empty()).then(|| values.into_iter().map(|value| value.max(0.0)).sum())
}

fn formatted_values(counter: isize) -> Vec<f64> {
    let mut buffer_size = 0u32;
    let mut item_count = 0u32;
    let status = unsafe {
        PdhGetFormattedCounterArrayW(
            counter,
            PDH_FMT_DOUBLE,
            &mut buffer_size,
            &mut item_count,
            null_mut(),
        )
    };
    if status != PDH_MORE_DATA || buffer_size == 0 || item_count == 0 {
        return Vec::new();
    }

    let item_size = std::mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>();
    let item_capacity = (buffer_size as usize).div_ceil(item_size);
    let mut items = Vec::with_capacity(item_capacity);
    for _ in 0..item_capacity {
        items.push(unsafe { zeroed::<PDH_FMT_COUNTERVALUE_ITEM_W>() });
    }
    let mut actual_size = buffer_size;
    let mut actual_count = item_count;
    let status = unsafe {
        PdhGetFormattedCounterArrayW(
            counter,
            PDH_FMT_DOUBLE,
            &mut actual_size,
            &mut actual_count,
            items.as_mut_ptr(),
        )
    };
    if status != 0 {
        return Vec::new();
    }

    items
        .into_iter()
        .take(actual_count as usize)
        .filter_map(|item| {
            if item.FmtValue.CStatus != PDH_CSTATUS_VALID_DATA
                && item.FmtValue.CStatus != PDH_CSTATUS_NEW_DATA
            {
                return None;
            }
            Some(unsafe { item.FmtValue.Anonymous.doubleValue })
        })
        .filter(|value| value.is_finite())
        .collect()
}

fn sample_cpu(previous: &mut Option<CpuSample>) -> Option<u8> {
    let mut idle: FILETIME = unsafe { zeroed() };
    let mut kernel: FILETIME = unsafe { zeroed() };
    let mut user: FILETIME = unsafe { zeroed() };
    let success: BOOL = unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) };
    if success == 0 {
        return None;
    }
    let current = CpuSample {
        idle: filetime_value(idle),
        kernel: filetime_value(kernel),
        user: filetime_value(user),
    };
    let result = previous.as_ref().and_then(|old| {
        let total_delta = current
            .kernel
            .saturating_sub(old.kernel)
            .saturating_add(current.user.saturating_sub(old.user));
        let idle_delta = current.idle.saturating_sub(old.idle);
        (total_delta > 0).then(|| {
            percentage((total_delta.saturating_sub(idle_delta) as f64 / total_delta as f64) * 100.0)
        })
    });
    *previous = Some(current);
    result
}

fn sample_ram() -> Option<u8> {
    let mut memory = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..unsafe { zeroed() }
    };
    let success = unsafe { GlobalMemoryStatusEx(&mut memory) };
    (success != 0).then_some(memory.dwMemoryLoad.min(100) as u8)
}

fn filetime_value(value: FILETIME) -> u64 {
    (u64::from(value.dwHighDateTime) << 32) | u64::from(value.dwLowDateTime)
}

fn percentage(value: f64) -> u8 {
    value.round().clamp(0.0, 100.0) as u8
}

#[derive(Clone, Copy)]
struct NvidiaTelemetry {
    power_watts: Option<u16>,
    power_percent: Option<u8>,
    vram_used_bytes: Option<u64>,
    vram_percent: Option<u8>,
}

fn read_nvidia_gpu_telemetry() -> Option<NvidiaTelemetry> {
    let output = Command::new("nvidia-smi.exe")
        .creation_flags(CREATE_NO_WINDOW)
        .args([
            "--query-gpu=power.draw,power.limit,memory.used,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut power_watts = 0.0;
    let mut power_limit_watts = 0.0;
    let mut vram_used_mib = 0.0;
    let mut vram_total_mib = 0.0;
    let mut found = false;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut columns = line.split(',').map(str::trim);
        let power = columns.next().and_then(|value| value.parse::<f64>().ok());
        let power_limit = columns.next().and_then(|value| value.parse::<f64>().ok());
        let used = columns.next().and_then(|value| value.parse::<f64>().ok());
        let total = columns.next().and_then(|value| value.parse::<f64>().ok());
        if power.is_some() || power_limit.is_some() || used.is_some() || total.is_some() {
            found = true;
        }
        if let Some(value) = power {
            power_watts += value.max(0.0);
        }
        if let Some(value) = power_limit {
            power_limit_watts += value.max(0.0);
        }
        if let Some(value) = used {
            vram_used_mib += value.max(0.0);
        }
        if let Some(value) = total {
            vram_total_mib += value.max(0.0);
        }
    }
    if !found {
        return None;
    }
    let vram_percent =
        (vram_total_mib > 0.0).then(|| percentage(vram_used_mib / vram_total_mib * 100.0));
    let power_percent =
        (power_limit_watts > 0.0).then(|| percentage(power_watts / power_limit_watts * 100.0));
    Some(NvidiaTelemetry {
        power_watts: Some(power_watts.round().clamp(0.0, f64::from(u16::MAX)) as u16),
        power_percent,
        vram_used_bytes: Some((vram_used_mib * 1024.0 * 1024.0).round() as u64),
        vram_percent,
    })
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}
