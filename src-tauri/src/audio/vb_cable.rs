//! 小米语音环境：检测 VB-CABLE，并使用 VB-Audio 官方图形安装器修复。
//!
//! 内嵌 ZIP 仍须通过固定 SHA-256 校验；PowerShell 仅用于在驱动端点就绪后
//! 设定默认麦克风，绝不再承担正常应用流程中的驱动注册或安装。

use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::copy;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};
use std::time::{Duration, Instant};

pub const DRIVER_ZIP_NAME: &str = "VBCABLE_Driver_Pack45.zip";
pub const DRIVER_ZIP_SHA256: &str =
    "b950e39f01af1d04ea623c8f6d8eb9b6ea5c477c637295fabf20631c85116bfb";
pub const CONFIGURE_SCRIPT_NAME: &str = "configure-xiaomi-audio.ps1";
pub const DOWNLOAD_PAGE_URL: &str = "https://vb-audio.com/Cable/";
pub const DOWNLOAD_ZIP_URL: &str =
    "https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack45.zip";

type CableEndpoints = (bool, bool);
const VOICE_ENV_STATUS_CACHE_TTL: Duration = Duration::from_secs(30);

#[derive(Default)]
struct VoiceEnvStatusCache {
    entry: Option<(Instant, CableEndpoints)>,
}

impl VoiceEnvStatusCache {
    const fn new() -> Self {
        Self { entry: None }
    }

    fn get_fresh(&self, now: Instant, ttl: Duration) -> Option<CableEndpoints> {
        self.entry.and_then(|(at, endpoints)| {
            (now.saturating_duration_since(at) < ttl).then_some(endpoints)
        })
    }

    fn latest(&self) -> Option<CableEndpoints> {
        self.entry.map(|(_, endpoints)| endpoints)
    }

    fn store(&mut self, now: Instant, endpoints: CableEndpoints) {
        self.entry = Some((now, endpoints));
    }

    fn invalidate(&mut self) {
        self.entry = None;
    }
}

static VOICE_ENV_STATUS_CACHE: Mutex<VoiceEnvStatusCache> = Mutex::new(VoiceEnvStatusCache::new());
static DRIVER_ZIP_CACHE: Mutex<Option<PathBuf>> = Mutex::new(None);
static OFFICIAL_INSTALLER_RUNNING: AtomicBool = AtomicBool::new(false);

const OFFICIAL_STAGE_DIR_NAME: &str = "vb-cable-official-stage";
const OFFICIAL_STAGE_MARKER: &str = ".zip.sha256";

struct InstallerRunGuard;

impl Drop for InstallerRunGuard {
    fn drop(&mut self) {
        OFFICIAL_INSTALLER_RUNNING.store(false, Ordering::Release);
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceEnvStatus {
    pub ready: bool,
    pub cable_input: bool,
    pub cable_output: bool,
    pub embedded_available: bool,
    pub embedded_zip_path: Option<String>,
    pub download_page_url: String,
    pub download_zip_url: String,
    pub message: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceEnvActionResult {
    pub ok: bool,
    pub ready: bool,
    pub needs_choice: bool,
    pub needs_reboot: bool,
    /// Stable UI outcome code; message remains the diagnostic/log payload.
    pub result_code: String,
    pub message: String,
    pub report_path: Option<String>,
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let file = File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut reader = std::io::BufReader::new(file);
    let mut hasher = Sha256::new();
    copy(&mut reader, &mut hasher).map_err(|e| format!("hash {}: {e}", path.display()))?;
    Ok(format!("{:x}", hasher.finalize()))
}

fn asset_candidates(file_name: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(p) = std::env::var("REMOTE_BRIDGE_XIAOMI_VB_CABLE_ZIP") {
        if file_name == DRIVER_ZIP_NAME {
            out.push(PathBuf::from(p));
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            out.push(dir.join("assets").join("xiaomi").join(file_name));
            out.push(
                dir.join("resources")
                    .join("assets")
                    .join("xiaomi")
                    .join(file_name),
            );
            out.push(
                dir.join("_up_")
                    .join("resources")
                    .join("assets")
                    .join("xiaomi")
                    .join(file_name),
            );
            if let Some(parent) = dir.parent() {
                out.push(
                    parent
                        .join("resources")
                        .join("assets")
                        .join("xiaomi")
                        .join(file_name),
                );
            }
        }
    }
    if let Some(manifest) = option_env!("CARGO_MANIFEST_DIR") {
        out.push(
            PathBuf::from(manifest)
                .join("assets")
                .join("xiaomi")
                .join(file_name),
        );
    }
    out
}

pub fn find_driver_zip() -> Option<PathBuf> {
    // 内嵌资产通常不变：仅缓存验证成功的路径。首次缺失或瞬时不可读时
    // 不缓存 None，这样文件恢复后无需重启应用即可重新发现。
    cached_driver_zip_with(&DRIVER_ZIP_CACHE, find_driver_zip_uncached)
}

fn cached_driver_zip_with<F>(cache: &Mutex<Option<PathBuf>>, find: F) -> Option<PathBuf>
where
    F: FnOnce() -> Option<PathBuf>,
{
    let mut cached = cache.lock().unwrap_or_else(|error| error.into_inner());
    if let Some(path) = cached.clone() {
        if path.is_file() {
            return Some(path);
        }
        log::warn!("cached VB-CABLE zip disappeared path={}", path.display());
        *cached = None;
    }

    let path = find()?;
    *cached = Some(path.clone());
    Some(path)
}

fn find_driver_zip_uncached() -> Option<PathBuf> {
    for path in asset_candidates(DRIVER_ZIP_NAME) {
        if !path.is_file() {
            continue;
        }
        match sha256_file(&path) {
            Ok(hash) if hash.eq_ignore_ascii_case(DRIVER_ZIP_SHA256) => return Some(path),
            Ok(hash) => log::warn!(
                "VB-CABLE zip hash mismatch path={} got={hash}",
                path.display()
            ),
            Err(e) => log::warn!("VB-CABLE zip unreadable: {e}"),
        }
    }
    None
}

pub fn find_configure_script() -> Option<PathBuf> {
    asset_candidates(CONFIGURE_SCRIPT_NAME)
        .into_iter()
        .find(|p| p.is_file())
}

fn official_setup_exe_name() -> &'static str {
    if cfg!(target_pointer_width = "64") {
        "VBCABLE_Setup_x64.exe"
    } else {
        "VBCABLE_Setup.exe"
    }
}

fn official_setup_fallback_exe_name() -> &'static str {
    if cfg!(target_pointer_width = "64") {
        "VBCABLE_Setup.exe"
    } else {
        "VBCABLE_Setup_x64.exe"
    }
}

fn app_data_root() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Nexus Prime")
        .join("VB-CABLE")
}

fn find_named_file(root: &Path, name: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file()
            && path
                .file_name()
                .is_some_and(|candidate| candidate.eq_ignore_ascii_case(name))
        {
            return Some(path);
        }
        if path.is_dir() {
            if let Some(found) = find_named_file(&path, name) {
                return Some(found);
            }
        }
    }
    None
}

fn staged_setup_if_valid(stage: &Path, zip_hash: &str) -> Option<PathBuf> {
    let marker = stage.join(OFFICIAL_STAGE_MARKER);
    let marker_hash = std::fs::read_to_string(marker).ok()?;
    if !marker_hash.trim().eq_ignore_ascii_case(zip_hash) {
        return None;
    }
    find_named_file(stage, official_setup_exe_name())
        .or_else(|| find_named_file(stage, official_setup_fallback_exe_name()))
}

fn extract_official_driver_zip(zip: &Path, destination: &Path) -> Result<(), String> {
    let tar_status = Command::new("tar")
        .args([
            "-xf",
            &zip.display().to_string(),
            "-C",
            &destination.display().to_string(),
        ])
        .status();
    if matches!(tar_status, Ok(status) if status.success()) {
        return Ok(());
    }

    // 仅用于解压已校验的本地 ZIP；不是驱动安装路径，且没有可见控制台窗口。
    let script = format!(
        "Expand-Archive -LiteralPath '{}' -DestinationPath '{}' -Force",
        zip.display().to_string().replace('\'', "''"),
        destination.display().to_string().replace('\'', "''")
    );
    let output = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            &script,
        ])
        .output()
        .map_err(|error| format!("解压 VB-CABLE 官方包失败: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "解压 VB-CABLE 官方包失败: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// 将已验证的官方包解压至项目专属暂存目录。新内容始终先进入独立临时目录，
/// 验证完整安装器后才原子替换旧暂存，任何失败都不会破坏可复用的有效内容。
fn stage_official_setup(zip: &Path) -> Result<PathBuf, String> {
    let zip_hash = sha256_file(zip)?;
    if !zip_hash.eq_ignore_ascii_case(DRIVER_ZIP_SHA256) {
        return Err("VB-CABLE 内嵌驱动包 SHA-256 校验失败".into());
    }

    let root = app_data_root();
    std::fs::create_dir_all(&root)
        .map_err(|error| format!("创建 VB-CABLE 暂存目录失败: {error}"))?;
    let stage = root.join(OFFICIAL_STAGE_DIR_NAME);
    if let Some(setup) = staged_setup_if_valid(&stage, &zip_hash) {
        log::info!(
            "reuse verified VB-CABLE official setup path={}",
            setup.display()
        );
        return Ok(setup);
    }

    let pending = root.join(format!(
        "{OFFICIAL_STAGE_DIR_NAME}.pending-{}",
        std::process::id()
    ));
    if pending.exists() {
        std::fs::remove_dir_all(&pending)
            .map_err(|error| format!("清理旧的 VB-CABLE 临时暂存失败: {error}"))?;
    }
    std::fs::create_dir(&pending)
        .map_err(|error| format!("创建 VB-CABLE 临时暂存失败: {error}"))?;

    let staged_setup = (|| {
        extract_official_driver_zip(zip, &pending)?;
        let setup = find_named_file(&pending, official_setup_exe_name())
            .or_else(|| find_named_file(&pending, official_setup_fallback_exe_name()))
            .ok_or_else(|| "官方 VB-CABLE 包中未找到 Setup 安装器".to_string())?;
        std::fs::write(pending.join(OFFICIAL_STAGE_MARKER), &zip_hash)
            .map_err(|error| format!("写入 VB-CABLE 暂存校验标记失败: {error}"))?;
        Ok::<PathBuf, String>(setup)
    })();
    if let Err(error) = staged_setup {
        let _ = std::fs::remove_dir_all(&pending);
        return Err(error);
    }

    if stage.exists() {
        std::fs::remove_dir_all(&stage)
            .map_err(|error| format!("替换过期 VB-CABLE 暂存失败: {error}"))?;
    }
    std::fs::rename(&pending, &stage)
        .map_err(|error| format!("完成 VB-CABLE 安全暂存失败: {error}"))?;
    staged_setup_if_valid(&stage, &zip_hash)
        .ok_or_else(|| "VB-CABLE 暂存完成后无法验证官方安装器".to_string())
}

#[cfg(target_os = "windows")]
fn launch_gui_exe_and_wait(exe: &Path) -> Result<u32, String> {
    use windows::core::HSTRING;
    use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE};
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let file = HSTRING::from(exe.as_os_str());
    let directory = HSTRING::from(exe.parent().unwrap_or_else(|| Path::new(".")).as_os_str());
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpFile: windows::core::PCWSTR(file.as_ptr()),
        lpDirectory: windows::core::PCWSTR(directory.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe {
        ShellExecuteExW(&mut info)
            .map_err(|error| format!("启动 VB-CABLE 官方安装器失败: {error}"))?;
        let process = info.hProcess;
        if process.is_invalid() {
            return Err("VB-CABLE 官方安装器未返回进程句柄".into());
        }
        let waited = WaitForSingleObject(process, INFINITE);
        if waited != WAIT_OBJECT_0 {
            let _ = CloseHandle(process);
            return Err(format!("等待 VB-CABLE 官方安装器失败: {waited:?}"));
        }
        let mut exit_code = 0;
        GetExitCodeProcess(process, &mut exit_code)
            .map_err(|error| format!("读取 VB-CABLE 安装器退出码失败: {error}"))?;
        let _ = CloseHandle(process);
        Ok(exit_code)
    }
}

#[cfg(not(target_os = "windows"))]
fn launch_gui_exe_and_wait(_exe: &Path) -> Result<u32, String> {
    Err("仅 Windows 支持 VB-CABLE 官方安装器".into())
}

fn installer_result_code(exit_code: u32, ready: bool) -> &'static str {
    if exit_code == 3010 || exit_code == 1641 {
        "voice_env.reboot_required"
    } else if exit_code != 0 {
        "voice_env.installer_failed"
    } else if ready {
        "voice_env.ready"
    } else {
        "voice_env.incomplete"
    }
}

#[cfg(target_os = "windows")]
fn probe_cable_endpoints() -> Result<CableEndpoints, String> {
    use cpal::traits::{DeviceTrait, HostTrait};
    let host = cpal::default_host();
    let mut cable_input = false;
    let mut cable_output = false;
    let mut output_name_error = None;
    let devices = host
        .output_devices()
        .map_err(|e| format!("枚举输出设备失败: {e}"))?;
    for d in devices {
        match d.name() {
            Ok(name) if name.to_ascii_lowercase().contains("cable input") => {
                cable_input = true;
            }
            Ok(_) => {}
            Err(e) => {
                output_name_error.get_or_insert_with(|| e.to_string());
            }
        }
    }
    if !cable_input {
        if let Some(error) = output_name_error {
            return Err(format!("读取输出设备名称失败: {error}"));
        }
    }

    let mut input_name_error = None;
    let devices = host
        .input_devices()
        .map_err(|e| format!("枚举输入设备失败: {e}"))?;
    for d in devices {
        match d.name() {
            Ok(name) if name.to_ascii_lowercase().contains("cable output") => {
                cable_output = true;
            }
            Ok(_) => {}
            Err(e) => {
                input_name_error.get_or_insert_with(|| e.to_string());
            }
        }
    }
    if !cable_output {
        if let Some(error) = input_name_error {
            return Err(format!("读取输入设备名称失败: {error}"));
        }
    }

    Ok((cable_input, cable_output))
}

#[cfg(not(target_os = "windows"))]
fn probe_cable_endpoints() -> Result<CableEndpoints, String> {
    Ok((false, false))
}

fn lock_voice_env_status_cache() -> std::sync::MutexGuard<'static, VoiceEnvStatusCache> {
    VOICE_ENV_STATUS_CACHE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
}

fn invalidate_voice_env_status_cache() {
    lock_voice_env_status_cache().invalidate();
}

fn cached_probe_with<F>(
    cache: &mut VoiceEnvStatusCache,
    now: Instant,
    ttl: Duration,
    probe: F,
) -> Result<CableEndpoints, String>
where
    F: FnOnce() -> Result<CableEndpoints, String>,
{
    if let Some(endpoints) = cache.get_fresh(now, ttl) {
        return Ok(endpoints);
    }
    let endpoints = probe()?;
    cache.store(Instant::now(), endpoints);
    Ok(endpoints)
}

fn probe_and_store_with<F>(
    cache: &Mutex<VoiceEnvStatusCache>,
    probe: F,
) -> Result<CableEndpoints, String>
where
    F: FnOnce() -> Result<CableEndpoints, String>,
{
    // 从探测开始到写入全程持锁，避免更早启动的慢探测在修复成功后
    // 把旧结果反向覆盖回缓存。
    let mut cached = cache.lock().unwrap_or_else(|error| error.into_inner());
    let endpoints = probe()?;
    cached.store(Instant::now(), endpoints);
    Ok(endpoints)
}

fn probe_and_store_voice_env_status_cache() -> Result<CableEndpoints, String> {
    probe_and_store_with(&VOICE_ENV_STATUS_CACHE, probe_cable_endpoints)
}

fn build_voice_env_status(cable_input: bool, cable_output: bool) -> VoiceEnvStatus {
    let ready = cable_input && cable_output;
    let zip = find_driver_zip();
    let embedded_available = zip.is_some() && find_configure_script().is_some();
    let message = if ready {
        "VB-CABLE 已就绪。可点「虚拟声卡检测与修复」将默认麦克风设为 CABLE Output。".into()
    } else if embedded_available {
        "未检测到 VB-CABLE。可使用内嵌驱动安装，或打开官网下载最新版。".into()
    } else {
        "未检测到 VB-CABLE，且内嵌驱动包不可用。请从官网下载安装。".into()
    };
    VoiceEnvStatus {
        ready,
        cable_input,
        cable_output,
        embedded_available,
        embedded_zip_path: zip.map(|p| p.display().to_string()),
        download_page_url: DOWNLOAD_PAGE_URL.into(),
        download_zip_url: DOWNLOAD_ZIP_URL.into(),
        message,
    }
}

pub fn voice_env_status() -> VoiceEnvStatus {
    let endpoints = match probe_and_store_voice_env_status_cache() {
        Ok(endpoints) => endpoints,
        Err(error) => {
            log::warn!("VB-CABLE endpoint probe failed: {error}");
            lock_voice_env_status_cache().latest().unwrap_or_default()
        }
    };
    let (cable_input, cable_output) = endpoints;
    build_voice_env_status(cable_input, cable_output)
}

/// 带缓存的探测：cpal 全量枚举音频设备走 COM/RPC，前端状态页每秒轮询一次，
/// 不缓存时实测占 ~3-4% 单核。修复/安装流程请用无缓存的 `voice_env_status()`。
pub fn voice_env_status_cached() -> VoiceEnvStatus {
    // 锁在探测期间保持，使并发的修复结果不会被更早启动的旧探测覆盖。
    let mut cache = lock_voice_env_status_cache();
    let endpoints = match cached_probe_with(
        &mut cache,
        Instant::now(),
        VOICE_ENV_STATUS_CACHE_TTL,
        probe_cable_endpoints,
    ) {
        Ok(endpoints) => endpoints,
        Err(error) => {
            // 瞬时 COM/RPC 枚举失败不写入 30s 缓存；保留上次成功状态并在
            // 下一次轮询立即重试。
            log::warn!("VB-CABLE cached endpoint probe failed: {error}");
            cache.latest().unwrap_or_default()
        }
    };
    drop(cache);
    let (cable_input, cable_output) = endpoints;
    build_voice_env_status(cable_input, cable_output)
}

fn desktop_report_path() -> PathBuf {
    let desktop = std::env::var("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("Desktop");
    desktop.join("XiaomiRemoteBridge-audio-check.txt")
}

fn app_path_for_script() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn run_configure_script(mode: &str, zip: &Path) -> Result<VoiceEnvActionResult, String> {
    invalidate_voice_env_status_cache();
    let script =
        find_configure_script().ok_or_else(|| "未找到 configure-xiaomi-audio.ps1".to_string())?;
    let app_path = app_path_for_script();
    log::info!(
        "XIAOMI VOICE ENV: run script mode={mode} zip={} app={}",
        zip.display(),
        app_path.display()
    );

    let output = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            &script.display().to_string(),
            "-Mode",
            mode,
            "-AppPath",
            &app_path.display().to_string(),
            "-DriverZipPath",
            &zip.display().to_string(),
        ])
        .output()
        .map_err(|e| format!("启动语音环境脚本失败: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !stdout.is_empty() {
        log::info!("XIAOMI VOICE ENV stdout: {stdout}");
    }
    if !stderr.is_empty() {
        log::warn!("XIAOMI VOICE ENV stderr: {stderr}");
    }

    // 脚本多数情况 exit 0，结果写在桌面报告里
    let report = desktop_report_path();
    let report_text = std::fs::read_to_string(&report).unwrap_or_default();
    let needs_reboot = report_text
        .to_ascii_lowercase()
        .contains("restart required")
        || report_text.contains("需要重启")
        || output.status.code() == Some(3010);
    let warning = report_text
        .lines()
        .find(|l| l.starts_with("Result: WARNING"))
        .map(|l| l.trim_start_matches("Result: ").to_string());

    // 稍等端点出现
    for _ in 0..15 {
        match probe_cable_endpoints() {
            Ok((i, o)) if i && o => break,
            Ok(_) | Err(_) => {
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        }
    }
    let (cable_input, cable_output) = match probe_and_store_voice_env_status_cache() {
        Ok(endpoints) => endpoints,
        Err(error) => {
            invalidate_voice_env_status_cache();
            log::warn!("VB-CABLE final endpoint probe failed: {error}");
            (false, false)
        }
    };
    let ready = cable_input && cable_output;

    let message = if let Some(w) = warning {
        w
    } else if needs_reboot {
        "驱动已安装，但可能需要重启 Windows 后端点才会出现。重启后再点一次「虚拟声卡检测与修复」。"
            .into()
    } else if ready {
        "语音环境已就绪：VB-CABLE 可用，默认麦克风已尝试设为 CABLE Output。".into()
    } else if !output.status.success() {
        format!(
            "脚本执行失败 (code={:?})。{}",
            output.status.code(),
            if stderr.is_empty() { stdout } else { stderr }
        )
    } else {
        "脚本已执行，但尚未检测到 CABLE Input/Output。若刚装驱动请重启后再试，或改用官网最新包。"
            .into()
    };

    Ok(VoiceEnvActionResult {
        ok: ready || needs_reboot,
        ready,
        needs_choice: false,
        needs_reboot,
        result_code: if ready {
            "voice_env.ready"
        } else if needs_reboot {
            "voice_env.reboot_required"
        } else {
            "voice_env.incomplete"
        }
        .into(),
        message,
        report_path: if report.is_file() {
            Some(report.display().to_string())
        } else {
            None
        },
    })
}

/// 启动 VB-Audio 官方完整安装器并在结束后重新探测端点。
fn run_official_setup_installer() -> Result<VoiceEnvActionResult, String> {
    if OFFICIAL_INSTALLER_RUNNING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Ok(VoiceEnvActionResult {
            ok: false,
            ready: false,
            needs_choice: false,
            needs_reboot: false,
            result_code: "voice_env.repair_in_progress".into(),
            message: "VB-CABLE 官方安装器正在运行，请完成或关闭后再试。".into(),
            report_path: None,
        });
    }
    let _running_guard = InstallerRunGuard;
    let zip = find_driver_zip().ok_or_else(|| "内嵌 VB-CABLE 驱动包不可用".to_string())?;
    let setup = stage_official_setup(&zip)?;
    log::info!("launch VB-CABLE official setup path={}", setup.display());
    let exit_code = match launch_gui_exe_and_wait(&setup) {
        Ok(code) => code,
        Err(error)
            if error.contains("1223")
                || error.to_ascii_lowercase().contains("704c7")
                || error.to_ascii_lowercase().contains("cancelled") =>
        {
            return Ok(VoiceEnvActionResult {
                ok: false,
                ready: false,
                needs_choice: false,
                needs_reboot: false,
                result_code: "voice_env.install_cancelled".into(),
                message: "已取消 Windows 管理员确认；未执行驱动安装，可随时重新尝试。".into(),
                report_path: None,
            });
        }
        Err(error) => return Err(error),
    };

    invalidate_voice_env_status_cache();
    let mut endpoints = (false, false);
    for _ in 0..20 {
        match probe_cable_endpoints() {
            Ok(found) => {
                endpoints = found;
                if found.0 && found.1 {
                    break;
                }
            }
            Err(error) => log::warn!("VB-CABLE post-install endpoint probe failed: {error}"),
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    // 将最终结果写回缓存，避免安装前的缺失状态覆盖刚安装的端点。
    lock_voice_env_status_cache().store(Instant::now(), endpoints);
    let ready = endpoints.0 && endpoints.1;
    let needs_reboot = exit_code == 3010 || exit_code == 1641 || !ready;

    let mic_message = if ready {
        match run_configure_script("EnsureMic", &zip) {
            Ok(result) if result.ok || result.ready => String::new(),
            Ok(result) => format!("；默认麦克风未能自动校正：{}", result.message),
            Err(error) => format!("；默认麦克风未能自动校正：{error}"),
        }
    } else {
        String::new()
    };
    let message = if exit_code == 3010 || exit_code == 1641 {
        format!(
            "VB-CABLE 官方安装器已完成（退出码 {exit_code}），请先重启 Windows；重启后重新打开 Nexus Prime 并点击「自动修复」。{mic_message}"
        )
    } else if exit_code != 0 {
        format!("VB-CABLE 官方安装器以退出码 {exit_code} 结束。请重试安装，或查看安装器提示。")
    } else if ready {
        format!("VB-CABLE 官方安装器已完成，已检测到 CABLE Input/Output，并已尝试设为 CABLE Output 默认麦克风。{mic_message}")
    } else {
        "VB-CABLE 官方安装器已关闭，但 10 秒内未检测到 CABLE Input/Output。请重启 Windows 后重新打开 Nexus Prime 并点击「自动修复」。".into()
    };
    Ok(VoiceEnvActionResult {
        ok: exit_code == 0 && ready,
        ready,
        needs_choice: false,
        needs_reboot,
        result_code: installer_result_code(exit_code, ready).into(),
        message,
        report_path: None,
    })
}

/// 检测；已就绪时只校正默认麦克风，未就绪时由界面展示修复选择。
pub fn check_or_prompt() -> VoiceEnvActionResult {
    let status = voice_env_status();
    if status.ready {
        match find_driver_zip() {
            Some(zip) => match run_configure_script("EnsureMic", &zip) {
                Ok(mut r) => {
                    r.needs_choice = false;
                    r
                }
                Err(e) => VoiceEnvActionResult {
                    ok: false,
                    ready: true,
                    needs_choice: false,
                    needs_reboot: false,
                    result_code: "voice_env.repair_failed".into(),
                    message: format!("VB-CABLE 已在，但修复默认麦克风失败: {e}"),
                    report_path: None,
                },
            },
            None => VoiceEnvActionResult {
                ok: true,
                ready: true,
                needs_choice: false,
                needs_reboot: false,
                result_code: "voice_env.ready".into(),
                message: "VB-CABLE 已就绪（无内嵌包，跳过默认麦克风修复脚本）。".into(),
                report_path: None,
            },
        }
    } else {
        VoiceEnvActionResult {
            ok: false,
            ready: false,
            needs_choice: true,
            needs_reboot: false,
            result_code: "voice_env.choice_required".into(),
            message: status.message,
            report_path: None,
        }
    }
}

pub fn install_embedded() -> Result<VoiceEnvActionResult, String> {
    run_official_setup_installer()
}

pub fn open_download_page() -> Result<VoiceEnvActionResult, String> {
    #[cfg(target_os = "windows")]
    {
        Command::new("cmd")
            .args(["/C", "start", "", DOWNLOAD_PAGE_URL])
            .spawn()
            .map_err(|e| format!("打开下载页失败: {e}"))?;
    }
    #[cfg(not(target_os = "windows"))]
    {
        return Err("仅 Windows 支持".into());
    }
    Ok(VoiceEnvActionResult {
        ok: true,
        ready: false,
        needs_choice: false,
        needs_reboot: false,
        result_code: "voice_env.download_page_opened".into(),
        message: "已打开 VB-Audio 官网。安装完成后请再点「虚拟声卡检测与修复」。".into(),
        report_path: None,
    })
}

pub fn open_download_zip() -> Result<VoiceEnvActionResult, String> {
    #[cfg(target_os = "windows")]
    {
        Command::new("cmd")
            .args(["/C", "start", "", DOWNLOAD_ZIP_URL])
            .spawn()
            .map_err(|e| format!("打开下载链接失败: {e}"))?;
    }
    #[cfg(not(target_os = "windows"))]
    {
        return Err("仅 Windows 支持".into());
    }
    Ok(VoiceEnvActionResult {
        ok: true,
        ready: false,
        needs_choice: false,
        needs_reboot: false,
        result_code: "voice_env.download_started".into(),
        message: "已开始下载官方驱动包。安装完成后请再点「虚拟声卡检测与修复」。".into(),
        report_path: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::{mpsc, Arc};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn successful_fresh_probe_replaces_an_unexpired_cached_result() {
        let started = Instant::now();
        let mut cache = VoiceEnvStatusCache::new();
        cache.store(started, (false, false));
        assert_eq!(
            cache.get_fresh(started + Duration::from_secs(1), VOICE_ENV_STATUS_CACHE_TTL),
            Some((false, false))
        );

        cache.store(started + Duration::from_secs(2), (true, true));
        assert_eq!(
            cache.get_fresh(started + Duration::from_secs(3), VOICE_ENV_STATUS_CACHE_TTL),
            Some((true, true))
        );
    }

    #[test]
    fn probe_errors_are_not_cached_as_missing_devices() {
        let started = Instant::now();
        let mut cache = VoiceEnvStatusCache::new();
        let calls = Cell::new(0);

        let first = cached_probe_with(&mut cache, started, VOICE_ENV_STATUS_CACHE_TTL, || {
            calls.set(calls.get() + 1);
            Err("temporary COM error".into())
        });
        assert!(first.is_err());
        assert_eq!(cache.latest(), None);

        let second = cached_probe_with(
            &mut cache,
            started + Duration::from_millis(1),
            VOICE_ENV_STATUS_CACHE_TTL,
            || {
                calls.set(calls.get() + 1);
                Ok((true, true))
            },
        );
        assert_eq!(second.unwrap(), (true, true));
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn official_installer_prefers_architecture_matching_setup_name() {
        let expected = if cfg!(target_pointer_width = "64") {
            "VBCABLE_Setup_x64.exe"
        } else {
            "VBCABLE_Setup.exe"
        };
        assert_eq!(official_setup_exe_name(), expected);
        assert_ne!(
            official_setup_exe_name(),
            official_setup_fallback_exe_name()
        );
    }

    #[test]
    fn installer_exit_codes_keep_reboot_and_failure_distinct() {
        assert_eq!(installer_result_code(0, true), "voice_env.ready");
        assert_eq!(installer_result_code(0, false), "voice_env.incomplete");
        assert_eq!(
            installer_result_code(3010, false),
            "voice_env.reboot_required"
        );
        assert_eq!(
            installer_result_code(1641, false),
            "voice_env.reboot_required"
        );
        assert_eq!(
            installer_result_code(1, false),
            "voice_env.installer_failed"
        );
    }

    #[test]
    fn successful_missing_result_is_cached_until_ttl_expires() {
        let started = Instant::now();
        let mut cache = VoiceEnvStatusCache::new();
        let calls = Cell::new(0);

        let first = cached_probe_with(&mut cache, started, VOICE_ENV_STATUS_CACHE_TTL, || {
            calls.set(calls.get() + 1);
            Ok((false, false))
        });
        assert_eq!(first.unwrap(), (false, false));

        let stored_at = cache.entry.unwrap().0;

        let second = cached_probe_with(
            &mut cache,
            stored_at + Duration::from_secs(1),
            VOICE_ENV_STATUS_CACHE_TTL,
            || {
                calls.set(calls.get() + 1);
                Ok((true, true))
            },
        );
        assert_eq!(second.unwrap(), (false, false));
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn driver_zip_miss_is_not_cached_permanently() {
        let cache = Mutex::new(None);
        let calls = Cell::new(0);
        assert!(cached_driver_zip_with(&cache, || {
            calls.set(calls.get() + 1);
            None
        })
        .is_none());

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "nexus-prime-driver-cache-{}-{unique}.zip",
            std::process::id()
        ));
        std::fs::write(&path, b"verified by injected test finder").unwrap();

        let found = cached_driver_zip_with(&cache, || {
            calls.set(calls.get() + 1);
            Some(path.clone())
        });
        assert_eq!(found.as_deref(), Some(path.as_path()));
        assert_eq!(
            cached_driver_zip_with(&cache, || panic!("cached path should be reused")).as_deref(),
            Some(path.as_path())
        );
        assert_eq!(calls.get(), 2);

        std::fs::remove_file(&path).unwrap();

        // 缓存文件消失后，一次 finder miss 必须清掉旧路径。即使同一
        // 路径随后被坏内容重建，也必须再走 finder/哈希验证，不能直接信任。
        assert!(cached_driver_zip_with(&cache, || None).is_none());
        std::fs::write(&path, b"damaged replacement").unwrap();
        let recheck_calls = Cell::new(0);
        assert!(cached_driver_zip_with(&cache, || {
            recheck_calls.set(recheck_calls.get() + 1);
            None
        })
        .is_none());
        assert_eq!(recheck_calls.get(), 1);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn older_probe_cannot_overwrite_a_later_repair_result() {
        let cache = Arc::new(Mutex::new(VoiceEnvStatusCache::new()));
        let (old_probe_started_tx, old_probe_started_rx) = mpsc::channel();
        let (release_old_probe_tx, release_old_probe_rx) = mpsc::channel();

        let old_cache = Arc::clone(&cache);
        let old_probe = std::thread::spawn(move || {
            probe_and_store_with(&old_cache, || {
                old_probe_started_tx.send(()).unwrap();
                release_old_probe_rx.recv().unwrap();
                Ok((false, false))
            })
            .unwrap();
        });

        // 旧探测已持有锁时启动修复结果写入。后者必须等待旧探测完成，
        // 因此最终缓存一定是更新的修复结果。
        old_probe_started_rx.recv().unwrap();
        assert!(matches!(
            cache.try_lock(),
            Err(std::sync::TryLockError::WouldBlock)
        ));
        let repair_cache = Arc::clone(&cache);
        let repair_probe = std::thread::spawn(move || {
            probe_and_store_with(&repair_cache, || Ok((true, true))).unwrap();
        });
        release_old_probe_tx.send(()).unwrap();

        old_probe.join().unwrap();
        repair_probe.join().unwrap();
        assert_eq!(
            cache
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .latest(),
            Some((true, true))
        );
    }
}
