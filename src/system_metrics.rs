//! Best-effort Windows system telemetry for the optional live-status footer.
//!
//! These values are informational only. They are deliberately kept outside the
//! Herdr watcher and never participate in the shutdown decision.

use serde::Deserialize;
use std::collections::HashSet;
use std::io::{Read, Write};
use std::mem::zeroed;
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::ptr::null_mut;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const NVIDIA_REFRESH_INTERVAL: Duration = Duration::from_secs(10);
const CPU_TEMPERATURE_REFRESH_INTERVAL: Duration = Duration::from_secs(10);
const CPU_TEMPERATURE_RETRY_INTERVAL: Duration = Duration::from_secs(30);
const CPU_TEMPERATURE_MAX_RETRY_INTERVAL: Duration = Duration::from_secs(5 * 60);
const CPU_TEMPERATURE_STALE_AFTER: Duration = Duration::from_secs(30);
const CPU_TEMPERATURE_PROCESS_TIMEOUT: Duration = Duration::from_secs(10);
const CPU_TEMPERATURE_HTTP_TIMEOUT: Duration = Duration::from_secs(2);
const CPU_TEMPERATURE_PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(25);
const CPU_TEMPERATURE_MAX_ENDPOINT_OUTPUT_BYTES: usize = 64 * 1024;
const CPU_TEMPERATURE_MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const CPU_TEMPERATURE_MAX_RESPONSE_HEADERS_BYTES: usize = 16 * 1024;

fn cpu_temperature_refresh_interval(
    value_available: bool,
    retry_interval: &mut Duration,
) -> Duration {
    if value_available {
        *retry_interval = CPU_TEMPERATURE_RETRY_INTERVAL;
        CPU_TEMPERATURE_REFRESH_INTERVAL
    } else {
        let interval = *retry_interval;
        *retry_interval = interval
            .saturating_mul(2)
            .min(CPU_TEMPERATURE_MAX_RETRY_INTERVAL);
        interval
    }
}
const LHM_CPU_TEMPERATURE_ENDPOINT_QUERY: &str = r#"
$defaultPort = 8085
$endpoints = [System.Collections.Generic.List[object]]::new()
$endpointKeys = @{}
$localAddresses = @('127.0.0.1', '::1')
try {
    foreach ($networkInterface in [System.Net.NetworkInformation.NetworkInterface]::GetAllNetworkInterfaces()) {
        try {
            foreach ($unicastAddress in $networkInterface.GetIPProperties().UnicastAddresses) {
                $localAddresses += $unicastAddress.Address.ToString()
            }
        } catch { }
    }
} catch { }
$addEndpoint = {
    param([string]$Address, [int]$Port)
    $key = "${Address}|${Port}"
    if (-not $endpointKeys.ContainsKey($key)) {
        $endpointKeys[$key] = $true
        [void]$endpoints.Add([pscustomobject]@{ address = $Address; port = $Port })
    }
}
$processes = @(Get-Process -Name 'LibreHardwareMonitor' -ErrorAction SilentlyContinue)
if ($processes.Count -eq 0) { exit 1 }
try {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;

public static class HerdrNightWatchProcessImagePath {
    private const uint PROCESS_QUERY_LIMITED_INFORMATION = 0x1000;

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern IntPtr OpenProcess(uint desiredAccess, bool inheritHandle, uint processId);

    [DllImport("kernel32.dll", EntryPoint = "QueryFullProcessImageNameW", CharSet = CharSet.Unicode, ExactSpelling = true, SetLastError = true)]
    private static extern bool QueryFullProcessImageName(IntPtr process, uint flags, StringBuilder imageName, ref uint size);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool CloseHandle(IntPtr handle);

    public static string GetPath(uint processId) {
        IntPtr handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, processId);
        if (handle == IntPtr.Zero) return null;

        try {
            var imageName = new StringBuilder(32768);
            uint size = (uint)imageName.Capacity;
            return QueryFullProcessImageName(handle, 0, imageName, ref size) ? imageName.ToString() : null;
        } finally {
            CloseHandle(handle);
        }
    }
}
'@ -ErrorAction Stop
} catch { }

foreach ($process in $processes) {
    try {
        $processPath = [HerdrNightWatchProcessImagePath]::GetPath([uint32]$process.Id)
        if ([string]::IsNullOrWhiteSpace($processPath)) { continue }

        $configPath = [System.IO.Path]::ChangeExtension($processPath, '.config')
        if (-not (Test-Path -LiteralPath $configPath -PathType Leaf)) { continue }

        [xml]$config = [System.IO.File]::ReadAllText($configPath)
        $portSetting = $config.SelectSingleNode("/configuration/appSettings/add[@key='listenerPort']")
        $port = $defaultPort
        $configuredPort = 0
        if ($null -ne $portSetting -and [int]::TryParse($portSetting.GetAttribute('value'), [ref]$configuredPort) -and $configuredPort -gt 0 -and $configuredPort -le 65535) {
            $port = $configuredPort
        }

        $addresses = @('127.0.0.1')
        try {
            foreach ($localAddress in [System.Net.Dns]::GetHostAddresses('localhost')) {
                $addresses += $localAddress.ToString()
            }
        } catch { }
        $addressSetting = $config.SelectSingleNode("/configuration/appSettings/add[@key='listenerIp']")
        if ($null -ne $addressSetting) {
            $configuredAddress = $null
            $addressText = $addressSetting.GetAttribute('value')
            if ([System.Net.IPAddress]::TryParse($addressText, [ref]$configuredAddress) -and $configuredAddress.ToString() -notin @('0.0.0.0', '::')) {
                $addresses = @($configuredAddress.ToString()) + $addresses
            }
        }
        foreach ($address in $addresses) { & $addEndpoint $address $port }
    } catch { }
}
foreach ($address in @('127.0.0.1', '::1')) { & $addEndpoint $address $defaultPort }
[Console]::Out.Write((ConvertTo-Json -InputObject ([pscustomobject]@{
    endpoints = [object[]]$endpoints.ToArray()
    localAddresses = [string[]]$localAddresses
}) -Compress -Depth 3))
"#;

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
    pub vram_percent: Option<u8>,
    pub ram_percent: Option<u8>,
    pub cpu_temperature_c: Option<u8>,
    pub gpu_temperature_c: Option<u8>,
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
    nvidia_available: bool,
    nvidia_last_read: Option<Instant>,
    nvidia_cached: Option<NvidiaTelemetry>,
    cpu_temperature: CpuTemperatureSampler,
}

struct CpuTemperatureSampler {
    receiver: Receiver<CpuTemperatureUpdate>,
    cached: Option<u8>,
    last_update: Option<Instant>,
}

#[derive(Clone, Copy)]
struct CpuTemperatureUpdate {
    value: Option<u8>,
    sampled_at: Instant,
}

impl CpuTemperatureSampler {
    fn new() -> Self {
        let (sender, receiver) = mpsc::channel();
        let _ = thread::Builder::new()
            .name("herdr-cpu-temperature-sensor".to_owned())
            .spawn(move || {
                let mut endpoints = Vec::new();
                let mut retry_interval = CPU_TEMPERATURE_RETRY_INTERVAL;
                loop {
                    if endpoints.is_empty() {
                        endpoints = discover_lhm_endpoints().unwrap_or_default();
                    }
                    let sampled_at = Instant::now();
                    let value = if endpoints.is_empty() {
                        None
                    } else {
                        match read_cpu_temperature_from_lhm(&endpoints) {
                            Ok(value) => value,
                            Err(()) => {
                                endpoints.clear();
                                None
                            }
                        }
                    };
                    let refresh_interval =
                        cpu_temperature_refresh_interval(value.is_some(), &mut retry_interval);
                    if sender
                        .send(CpuTemperatureUpdate { value, sampled_at })
                        .is_err()
                    {
                        break;
                    }
                    thread::sleep(refresh_interval);
                }
            });
        Self::from_receiver(receiver)
    }

    fn from_receiver(receiver: Receiver<CpuTemperatureUpdate>) -> Self {
        Self {
            receiver,
            cached: None,
            last_update: None,
        }
    }

    fn sample(&mut self) -> Option<u8> {
        loop {
            match self.receiver.try_recv() {
                Ok(update) => {
                    self.cached = update.value;
                    self.last_update = Some(update.sampled_at);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.cached = None;
                    self.last_update = None;
                    return None;
                }
            }
        }

        if self
            .last_update
            .is_some_and(|last_update| last_update.elapsed() >= CPU_TEMPERATURE_STALE_AFTER)
        {
            self.cached = None;
        }
        self.cached
    }
}

impl Sampler {
    pub fn new() -> Self {
        Self {
            cpu_previous: None,
            query: PdhQuery::new(),
            nvidia_available: true,
            nvidia_last_read: None,
            nvidia_cached: None,
            cpu_temperature: CpuTemperatureSampler::new(),
        }
    }

    fn sample_nvidia(&mut self) -> Option<NvidiaTelemetry> {
        if !self.nvidia_available {
            return self.nvidia_cached;
        }
        if self
            .nvidia_last_read
            .is_some_and(|last| last.elapsed() < NVIDIA_REFRESH_INTERVAL)
        {
            return self.nvidia_cached;
        }
        self.nvidia_last_read = Some(Instant::now());
        match read_nvidia_gpu_telemetry() {
            Some(telemetry) => {
                self.nvidia_cached = Some(telemetry);
            }
            None => {
                self.nvidia_available = false;
                self.nvidia_cached = None;
            }
        }
        self.nvidia_cached
    }

    fn sample_cpu_temperature(&mut self) -> Option<u8> {
        self.cpu_temperature.sample()
    }

    pub fn sample(&mut self) -> SystemMetrics {
        let cpu_percent = sample_cpu(&mut self.cpu_previous);
        let ram_percent = sample_ram();
        let gpu_percent = self
            .query
            .as_mut()
            .map(PdhQuery::sample)
            .unwrap_or_default();
        let nvidia = self.sample_nvidia();
        let cpu_temperature_c = self.sample_cpu_temperature();
        let gpu_watts = nvidia.as_ref().and_then(|telemetry| telemetry.power_watts);
        let gpu_power_percent = nvidia
            .as_ref()
            .and_then(|telemetry| telemetry.power_percent);
        SystemMetrics {
            cpu_percent,
            gpu_percent,
            vram_percent: nvidia.as_ref().and_then(|telemetry| telemetry.vram_percent),
            ram_percent,
            cpu_temperature_c,
            gpu_temperature_c: nvidia
                .as_ref()
                .and_then(|telemetry| telemetry.temperature_c),
            gpu_watts,
            gpu_power_percent,
        }
    }
}

struct PdhQuery {
    handle: isize,
    gpu: Option<isize>,
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
        };
        if query.gpu.is_none() {
            unsafe { PdhCloseQuery(handle) };
            None
        } else {
            Some(query)
        }
    }

    fn sample(&mut self) -> Option<u8> {
        if unsafe { PdhCollectQueryData(self.handle) } != 0 {
            return None;
        }

        let gpu = self.gpu.and_then(read_max);
        gpu.map(percentage)
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
    temperature_c: Option<u8>,
    power_watts: Option<u16>,
    power_percent: Option<u8>,
    vram_percent: Option<u8>,
}

fn read_nvidia_gpu_telemetry() -> Option<NvidiaTelemetry> {
    let output = Command::new("nvidia-smi.exe")
        .creation_flags(CREATE_NO_WINDOW)
        .args([
            "--query-gpu=temperature.gpu,power.draw,power.limit,memory.used,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut power_watts = 0.0;
    let mut power_found = false;
    let mut power_limit_watts = 0.0;
    let mut temperature_c = None;
    let mut vram_used_mib = 0.0;
    let mut vram_total_mib = 0.0;
    let mut found = false;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut columns = line.split(',').map(str::trim);
        let temperature = columns.next().and_then(|value| value.parse::<f64>().ok());
        let power = columns.next().and_then(|value| value.parse::<f64>().ok());
        let power_limit = columns.next().and_then(|value| value.parse::<f64>().ok());
        let used = columns.next().and_then(|value| value.parse::<f64>().ok());
        let total = columns.next().and_then(|value| value.parse::<f64>().ok());
        if temperature.is_some()
            || power.is_some()
            || power_limit.is_some()
            || used.is_some()
            || total.is_some()
        {
            found = true;
        }
        if let Some(value) = temperature {
            let value = value.round().clamp(0.0, f64::from(u8::MAX)) as u8;
            temperature_c = Some(temperature_c.map_or(value, |current: u8| current.max(value)));
        }
        if let Some(value) = power {
            power_watts += value.max(0.0);
            power_found = true;
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
        temperature_c,
        power_watts: power_found
            .then(|| power_watts.round().clamp(0.0, f64::from(u16::MAX)) as u16),
        power_percent,
        vram_percent,
    })
}

fn discover_lhm_endpoints() -> Option<Vec<LhmEndpoint>> {
    let output = run_powershell_query(
        LHM_CPU_TEMPERATURE_ENDPOINT_QUERY,
        CPU_TEMPERATURE_PROCESS_TIMEOUT,
    )?;
    let discovery: LhmEndpointDiscovery = serde_json::from_slice(&output).ok()?;
    let local_addresses = discovery
        .local_addresses
        .into_iter()
        .filter_map(|address| address.parse::<IpAddr>().ok())
        .collect::<HashSet<_>>();
    Some(retain_local_lhm_endpoints(
        discovery.endpoints,
        &local_addresses,
    ))
}

#[derive(Deserialize)]
struct LhmEndpointDiscovery {
    endpoints: Vec<LhmEndpoint>,
    #[serde(rename = "localAddresses")]
    local_addresses: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct LhmEndpoint {
    address: String,
    port: u16,
}

fn retain_local_lhm_endpoints(
    endpoints: Vec<LhmEndpoint>,
    local_addresses: &HashSet<IpAddr>,
) -> Vec<LhmEndpoint> {
    endpoints
        .into_iter()
        .filter(|endpoint| {
            endpoint
                .address
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback() || local_addresses.contains(&address))
        })
        .collect()
}

fn run_powershell_query(script: &str, timeout: Duration) -> Option<Vec<u8>> {
    let mut child = Command::new("powershell.exe")
        .creation_flags(CREATE_NO_WINDOW)
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    read_child_stdout_with_timeout(
        &mut child,
        timeout,
        CPU_TEMPERATURE_MAX_ENDPOINT_OUTPUT_BYTES,
    )
}

fn read_child_stdout_with_timeout(
    child: &mut Child,
    timeout: Duration,
    maximum_output_bytes: usize,
) -> Option<Vec<u8>> {
    let Some(stdout) = child.stdout.take() else {
        stop_child(child);
        return None;
    };
    let (output_tx, output_rx) = mpsc::sync_channel(1);
    let stdout_reader = thread::spawn(move || {
        let mut output = Vec::new();
        let result = stdout
            .take(maximum_output_bytes.saturating_add(1) as u64)
            .read_to_end(&mut output);
        let _ = output_tx.send(result.map(|_| output));
    });
    let mut captured_output = None;
    let started = Instant::now();
    loop {
        match output_rx.try_recv() {
            Ok(Ok(output)) if output.len() > maximum_output_bytes => {
                stop_child(child);
                let _ = stdout_reader.join();
                return None;
            }
            Ok(Ok(output)) => captured_output = Some(output),
            Ok(Err(_)) => {
                stop_child(child);
                let _ = stdout_reader.join();
                return None;
            }
            Err(TryRecvError::Disconnected) if captured_output.is_some() => {}
            Err(TryRecvError::Disconnected) => {
                stop_child(child);
                let _ = stdout_reader.join();
                return None;
            }
            Err(TryRecvError::Empty) => {}
        }
        if started.elapsed() >= timeout {
            stop_child(child);
            let _ = stdout_reader.join();
            return None;
        }
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                stdout_reader.join().ok()?;
                let output = match captured_output {
                    Some(output) => output,
                    None => output_rx.try_recv().ok()?.ok()?,
                };
                return (output.len() <= maximum_output_bytes).then_some(output);
            }
            Ok(Some(_)) => {
                let _ = stdout_reader.join();
                return None;
            }
            Ok(None) => {
                let remaining = timeout.saturating_sub(started.elapsed());
                thread::sleep(CPU_TEMPERATURE_PROCESS_POLL_INTERVAL.min(remaining));
            }
            Err(_) => {
                stop_child(child);
                let _ = stdout_reader.join();
                return None;
            }
        }
    }
}

fn stop_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn read_cpu_temperature_from_lhm(endpoints: &[LhmEndpoint]) -> Result<Option<u8>, ()> {
    let mut endpoint_responded = false;
    for endpoint in endpoints {
        let payload = match read_lhm_payload(endpoint) {
            Ok(payload) => {
                endpoint_responded = true;
                payload
            }
            Err(_) => continue,
        };
        if let Some(value) = cpu_temperature_from_lhm_json(&payload) {
            return Ok(Some(value));
        }
    }
    if endpoint_responded {
        Ok(None)
    } else {
        Err(())
    }
}

fn read_lhm_payload(endpoint: &LhmEndpoint) -> Result<serde_json::Value, String> {
    let address = endpoint
        .address
        .parse::<IpAddr>()
        .map_err(|error| format!("invalid sensor address: {error}"))?;
    let socket_address = SocketAddr::new(address, endpoint.port);
    let deadline = Instant::now() + CPU_TEMPERATURE_HTTP_TIMEOUT;
    let connect_timeout = deadline.saturating_duration_since(Instant::now());
    if connect_timeout.is_zero() {
        return Err("sensor connection deadline expired".to_owned());
    }
    let mut stream = TcpStream::connect_timeout(&socket_address, connect_timeout)
        .map_err(|error| format!("sensor connection failed: {error}"))?;
    let host = match address {
        IpAddr::V4(value) => value.to_string(),
        IpAddr::V6(value) => format!("[{value}]"),
    };
    let write_timeout = deadline.saturating_duration_since(Instant::now());
    if write_timeout.is_zero() {
        return Err("sensor request deadline expired".to_owned());
    }
    stream
        .set_write_timeout(Some(write_timeout))
        .map_err(|error| format!("sensor write timeout setup failed: {error}"))?;
    write!(
        stream,
        "GET /data.json HTTP/1.0\r\nHost: {host}:{}\r\nAccept: application/json\r\nConnection: close\r\n\r\n",
        endpoint.port
    )
    .map_err(|error| format!("sensor request write failed: {error}"))?;

    let mut response = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("sensor response deadline expired".to_owned());
        }
        stream
            .set_read_timeout(Some(remaining))
            .map_err(|error| format!("sensor read timeout setup failed: {error}"))?;
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(bytes_read) => {
                if response.len().saturating_add(bytes_read) > CPU_TEMPERATURE_MAX_RESPONSE_BYTES {
                    return Err("sensor response exceeded size limit".to_owned());
                }
                response.extend_from_slice(&buffer[..bytes_read]);
                if let Some(headers) = parse_lhm_http_headers(&response)
                    .map_err(|_| "invalid sensor response headers".to_owned())?
                {
                    if !headers.chunked
                        && headers.content_length.is_some_and(|length| {
                            response.len() >= headers.body_start.saturating_add(length)
                        })
                    {
                        break;
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(format!("sensor response read failed: {error}")),
        }
    }
    let body =
        parse_lhm_http_body(&response).map_err(|_| "invalid sensor response body".to_owned())?;
    serde_json::from_slice(&body)
        .map_err(|error| format!("sensor response contained invalid JSON: {error}"))
}

#[derive(Clone, Copy)]
struct LhmHttpHeaders {
    body_start: usize,
    content_length: Option<usize>,
    chunked: bool,
}

fn parse_lhm_http_headers(response: &[u8]) -> Result<Option<LhmHttpHeaders>, ()> {
    let Some(header_end) = response.windows(4).position(|window| window == b"\r\n\r\n") else {
        if response.len() > CPU_TEMPERATURE_MAX_RESPONSE_HEADERS_BYTES {
            return Err(());
        }
        return Ok(None);
    };
    if header_end > CPU_TEMPERATURE_MAX_RESPONSE_HEADERS_BYTES {
        return Err(());
    }
    let header_text = std::str::from_utf8(&response[..header_end]).map_err(|_| ())?;
    let mut lines = header_text.lines();
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or(())?;
    if status != 200 {
        return Err(());
    }
    let mut content_length = None;
    let mut chunked = false;
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let (name, value) = line.split_once(':').ok_or(())?;
        if name.eq_ignore_ascii_case("content-length") {
            let length = value.trim().parse::<usize>().map_err(|_| ())?;
            if length > CPU_TEMPERATURE_MAX_RESPONSE_BYTES {
                return Err(());
            }
            content_length = Some(length);
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            chunked = value
                .split(',')
                .any(|encoding| encoding.trim().eq_ignore_ascii_case("chunked"));
        }
    }
    Ok(Some(LhmHttpHeaders {
        body_start: header_end + 4,
        content_length,
        chunked,
    }))
}

fn parse_lhm_http_body(response: &[u8]) -> Result<Vec<u8>, ()> {
    let headers = parse_lhm_http_headers(response)?.ok_or(())?;
    let body = response.get(headers.body_start..).ok_or(())?;
    if headers.chunked {
        return decode_lhm_chunked_body(body);
    }
    if let Some(length) = headers.content_length {
        return Ok(body.get(..length).ok_or(())?.to_vec());
    }
    Ok(body.to_vec())
}

fn decode_lhm_chunked_body(mut body: &[u8]) -> Result<Vec<u8>, ()> {
    let mut decoded = Vec::new();
    loop {
        let line_end = body
            .windows(2)
            .position(|window| window == b"\r\n")
            .ok_or(())?;
        let size = std::str::from_utf8(&body[..line_end])
            .map_err(|_| ())?
            .split(';')
            .next()
            .ok_or(())?
            .trim();
        let size = usize::from_str_radix(size, 16).map_err(|_| ())?;
        body = body.get(line_end + 2..).ok_or(())?;
        if size == 0 {
            return Ok(decoded);
        }
        let chunk = body.get(..size).ok_or(())?;
        if decoded.len().saturating_add(size) > CPU_TEMPERATURE_MAX_RESPONSE_BYTES {
            return Err(());
        }
        decoded.extend_from_slice(chunk);
        body = body.get(size..).ok_or(())?;
        if !body.starts_with(b"\r\n") {
            return Err(());
        }
        body = body.get(2..).ok_or(())?;
    }
}

fn cpu_temperature_from_lhm_json(payload: &serde_json::Value) -> Option<u8> {
    let mut pending: Vec<&serde_json::Value> = payload
        .get("Children")
        .and_then(serde_json::Value::as_array)
        .map(|children| children.iter().collect())
        .unwrap_or_default();
    let mut readings = Vec::new();
    while let Some(node) = pending.pop() {
        let is_temperature =
            node.get("Type").and_then(serde_json::Value::as_str) == Some("Temperature");
        let sensor_id = node
            .get("SensorId")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase();
        if is_temperature
            && (sensor_id.starts_with("/amdcpu/0/temperature/")
                || sensor_id.starts_with("/intelcpu/0/temperature/"))
        {
            let value = node
                .get("RawValue")
                .and_then(parse_lhm_temperature_value)
                .or_else(|| node.get("Value").and_then(parse_lhm_temperature_value));
            if let Some(value) = value {
                let name = node
                    .get("Text")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                readings.push((name, value));
            }
        }
        if let Some(children) = node.get("Children").and_then(serde_json::Value::as_array) {
            pending.extend(children.iter());
        }
    }

    let preferred = readings.iter().find(|(name, _)| {
        let name = name.to_ascii_lowercase();
        name.contains("tctl/tdie") || name.contains("cpu package")
    });
    let selected = preferred.or_else(|| {
        readings
            .iter()
            .max_by(|left, right| left.1.total_cmp(&right.1))
    });
    selected.map(|(_, value)| value.round().clamp(0.0, f64::from(u8::MAX)) as u8)
}

fn parse_lhm_temperature_value(raw_value: &serde_json::Value) -> Option<f64> {
    let value = if let Some(value) = raw_value.as_f64() {
        value
    } else {
        let raw_text = raw_value.as_str()?.trim();
        let suffix_start = raw_text.len().checked_sub("°C".len());
        let raw_text = suffix_start
            .and_then(|index| raw_text.get(index..).map(|suffix| (index, suffix)))
            .filter(|(_, suffix)| suffix.eq_ignore_ascii_case("°C"))
            .map_or(raw_text, |(index, _)| raw_text[..index].trim_end());
        raw_text.replace(',', ".").parse::<f64>().ok()?
    };
    value.is_finite().then_some(value)
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn blocked_temperature_worker_does_not_block_metric_sampling() {
        let (temperature_tx, temperature_rx) = mpsc::channel();
        let worker_temperature_tx = temperature_tx.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            worker_temperature_tx
                .send(CpuTemperatureUpdate {
                    value: Some(72),
                    sampled_at: Instant::now(),
                })
                .unwrap();
        });
        let mut sampler = Sampler {
            cpu_previous: None,
            query: None,
            nvidia_available: false,
            nvidia_last_read: None,
            nvidia_cached: None,
            cpu_temperature: CpuTemperatureSampler::from_receiver(temperature_rx),
        };

        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let started = Instant::now();
        assert_eq!(sampler.sample().cpu_temperature_c, None);
        assert!(started.elapsed() < Duration::from_secs(1));

        release_tx.send(()).unwrap();
        worker.join().unwrap();
        assert_eq!(sampler.sample().cpu_temperature_c, Some(72));
        drop(temperature_tx);
    }

    #[test]
    fn unavailable_and_stale_temperatures_are_not_reported() {
        let (temperature_tx, temperature_rx) = mpsc::channel();
        let mut sampler = CpuTemperatureSampler::from_receiver(temperature_rx);

        temperature_tx
            .send(CpuTemperatureUpdate {
                value: Some(68),
                sampled_at: Instant::now(),
            })
            .unwrap();
        assert_eq!(sampler.sample(), Some(68));

        temperature_tx
            .send(CpuTemperatureUpdate {
                value: None,
                sampled_at: Instant::now(),
            })
            .unwrap();
        assert_eq!(sampler.sample(), None);

        temperature_tx
            .send(CpuTemperatureUpdate {
                value: Some(69),
                sampled_at: Instant::now() - CPU_TEMPERATURE_STALE_AFTER - Duration::from_millis(1),
            })
            .unwrap();
        assert_eq!(sampler.sample(), None);
    }

    #[test]
    fn cpu_temperature_parser_prefers_cpu_package_ignores_gpu_and_parses_decimal_comma() {
        let mut payload = serde_json::json!({
            "Children": [
                {
                    "Type": "Temperature",
                    "SensorId": "/amdcpu/0/temperature/0",
                    "Text": "CPU Core",
                    "RawValue": 88.0,
                    "Children": []
                },
                {
                    "Type": "Temperature",
                    "SensorId": "/gpu/0/temperature/0",
                    "Text": "GPU",
                    "RawValue": 96.0,
                    "Children": []
                },
                {
                    "Type": "Temperature",
                    "SensorId": "/amdcpu/0/temperature/1",
                    "Text": "Tctl/Tdie",
                    "RawValue": "46.9 °C",
                    "Children": []
                }
            ]
        });

        assert_eq!(cpu_temperature_from_lhm_json(&payload), Some(47));

        payload["Children"][2]["RawValue"] = serde_json::json!("46,9 °C");
        assert_eq!(cpu_temperature_from_lhm_json(&payload), Some(47));
    }

    #[test]
    fn lhm_endpoint_discovery_rejects_addresses_outside_local_interfaces() {
        let local_address = IpAddr::from([192, 168, 1, 24]);
        let discovery: LhmEndpointDiscovery = serde_json::from_value(serde_json::json!({
            "endpoints": [
                { "address": "127.0.0.1", "port": 8085 },
                { "address": "::1", "port": 8085 },
                { "address": local_address.to_string(), "port": 8085 },
                { "address": "192.0.2.44", "port": 8085 },
                { "address": "0.0.0.0", "port": 8085 },
                { "address": "::", "port": 8085 }
            ],
            "localAddresses": [local_address.to_string()]
        }))
        .unwrap();
        let local_addresses = discovery
            .local_addresses
            .into_iter()
            .filter_map(|address| address.parse::<IpAddr>().ok())
            .collect::<HashSet<_>>();

        let retained = retain_local_lhm_endpoints(discovery.endpoints, &local_addresses);

        assert_eq!(
            retained
                .iter()
                .map(|endpoint| endpoint.address.as_str())
                .collect::<Vec<_>>(),
            ["127.0.0.1", "::1", "192.168.1.24"]
        );
    }

    #[test]
    fn cpu_temperature_parser_falls_back_to_display_value() {
        let mut payload = serde_json::json!({
            "Children": [{
                "Type": "Temperature",
                "SensorId": "/intelcpu/0/temperature/0",
                "Text": "CPU Package",
                "RawValue": "not available",
                "Value": "51,6 °C",
                "Children": []
            }]
        });

        assert_eq!(cpu_temperature_from_lhm_json(&payload), Some(52));

        payload["Children"][0]
            .as_object_mut()
            .unwrap()
            .remove("RawValue");
        assert_eq!(cpu_temperature_from_lhm_json(&payload), Some(52));
    }

    #[test]
    fn cpu_temperature_retry_interval_backs_off_caps_and_resets_after_success() {
        let mut retry_interval = CPU_TEMPERATURE_RETRY_INTERVAL;

        assert_eq!(
            cpu_temperature_refresh_interval(false, &mut retry_interval),
            Duration::from_secs(30)
        );
        assert_eq!(
            cpu_temperature_refresh_interval(false, &mut retry_interval),
            Duration::from_secs(60)
        );
        assert_eq!(
            cpu_temperature_refresh_interval(false, &mut retry_interval),
            Duration::from_secs(120)
        );
        assert_eq!(
            cpu_temperature_refresh_interval(false, &mut retry_interval),
            Duration::from_secs(240)
        );
        assert_eq!(
            cpu_temperature_refresh_interval(false, &mut retry_interval),
            CPU_TEMPERATURE_MAX_RETRY_INTERVAL
        );
        assert_eq!(
            cpu_temperature_refresh_interval(false, &mut retry_interval),
            CPU_TEMPERATURE_MAX_RETRY_INTERVAL
        );
        assert_eq!(
            cpu_temperature_refresh_interval(true, &mut retry_interval),
            CPU_TEMPERATURE_REFRESH_INTERVAL
        );
        assert_eq!(
            cpu_temperature_refresh_interval(false, &mut retry_interval),
            CPU_TEMPERATURE_RETRY_INTERVAL
        );
    }

    #[test]
    fn direct_http_query_reads_lhm_temperature_json() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let payload = serde_json::json!({
            "Children": [{
                "Type": "Temperature",
                "SensorId": "/intelcpu/0/temperature/0",
                "Text": "CPU Package",
                "RawValue": 62.4,
                "Children": []
            }]
        });
        let (stop_tx, stop_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let mut stream = loop {
                if stop_rx.try_recv().is_ok() {
                    return;
                }
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("local HTTP fixture accept failed: {error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 256];
            loop {
                let bytes_read = stream.read(&mut buffer).unwrap();
                assert_ne!(bytes_read, 0, "local HTTP request ended before its headers");
                request.extend_from_slice(&buffer[..bytes_read]);
                assert!(
                    request.len() <= 1024,
                    "local HTTP request exceeded its limit"
                );
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            assert!(request.starts_with(b"GET /data.json HTTP/1.0\r\n"));
            let body = serde_json::to_vec(&payload).unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
        });

        let endpoint = LhmEndpoint {
            address: "127.0.0.1".to_owned(),
            port,
        };
        let result = read_lhm_payload(&endpoint);
        let _ = stop_tx.send(());
        server.join().unwrap();
        let payload = result.unwrap_or_else(|error| panic!("direct HTTP query failed: {error}"));
        assert_eq!(cpu_temperature_from_lhm_json(&payload), Some(62));
    }

    #[test]
    fn powershell_query_timeout_kills_and_reaps_the_child() {
        let availability = Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "$PSVersionTable.PSVersion.Major",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        assert!(
            availability.is_ok(),
            "Windows PowerShell is required for the sensor query"
        );

        let started = Instant::now();
        let result = run_powershell_query("Start-Sleep -Seconds 30", Duration::from_millis(100));
        assert!(result.is_none());
        assert!(started.elapsed() < Duration::from_secs(15));
    }

    #[test]
    fn powershell_query_drains_large_stdout_before_waiting_for_exit() {
        let output = run_powershell_query("Write-Output ('x' * 60000)", Duration::from_secs(10))
            .expect("large PowerShell stdout should be drained while the child runs");

        assert!(output.len() > 32 * 1024);
        assert!(output.len() <= CPU_TEMPERATURE_MAX_ENDPOINT_OUTPUT_BYTES);
    }

    #[test]
    fn powershell_query_rejects_stdout_over_the_configured_limit() {
        let started = Instant::now();
        let result = run_powershell_query("Write-Output ('x' * 262144)", Duration::from_secs(10));

        assert!(result.is_none());
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
