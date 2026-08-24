use crate::{
    auth::AuthManager,
    config::ServerConfig,
    error::ServerError,
    inference::InferenceEngine,
    models::ModelManager,
    pairing::{PairingPrompt, TerminalPairingPrompt},
};
use openflow_protocol::{ComputeBackend, HardwareProfile};
#[cfg(target_os = "linux")]
use std::fs;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::process::Command;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<ServerConfig>,
    pub auth: AuthManager,
    pub models: ModelManager,
    pub sessions: SessionGate,
    /// Serializes model activation/deletion with session prewarming so a
    /// verified file cannot disappear while a native worker opens it.
    pub model_lifecycle: Arc<Mutex<()>>,
    pub inference: Arc<dyn InferenceEngine>,
    pub pairing_prompt: Arc<dyn PairingPrompt>,
    pub hardware: HardwareProfile,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppState")
            .field("config", &self.config)
            .field("auth", &self.auth)
            .field("models", &self.models)
            .field("sessions", &self.sessions)
            .field("hardware", &self.hardware)
            .finish_non_exhaustive()
    }
}

impl AppState {
    /// Initializes authentication, model management, and hardware state.
    ///
    /// # Errors
    ///
    /// Returns an error when configuration, credentials, or the model cache
    /// cannot be initialized.
    pub async fn new(
        config: Arc<ServerConfig>,
        inference: Arc<dyn InferenceEngine>,
    ) -> Result<Self, ServerError> {
        config.validate()?;
        let auth = AuthManager::load(
            config.auth_store_path.clone(),
            config.admin_token.clone(),
            config.rotate_bootstrap_admin_token,
        )
        .await?;
        let models = ModelManager::load(
            config.model_cache_dir.clone(),
            config.model_registry_path.as_deref(),
        )
        .await?;
        let mut hardware = detect_hardware();
        let compiled_backends = inference.compute_backends();
        hardware
            .backends
            .retain(|backend| compiled_backends.contains(backend));
        if !hardware.backends.contains(&ComputeBackend::Cpu) {
            hardware.backends.push(ComputeBackend::Cpu);
        }
        if !hardware
            .backends
            .iter()
            .any(|backend| *backend != ComputeBackend::Cpu)
        {
            hardware.accelerator_memory_bytes = None;
        }
        let pairing_prompt = Arc::new(TerminalPairingPrompt::new(config.interactive_pairing));
        Ok(Self {
            config,
            auth,
            models,
            sessions: SessionGate::default(),
            model_lifecycle: Arc::new(Mutex::new(())),
            inference,
            pairing_prompt,
            hardware,
        })
    }
}

#[derive(Clone, Debug, Default)]
pub struct SessionGate {
    occupied: Arc<AtomicBool>,
}

impl SessionGate {
    pub fn try_acquire(&self) -> Option<SessionLease> {
        self.occupied
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| SessionLease {
                occupied: Arc::clone(&self.occupied),
            })
    }

    pub fn is_active(&self) -> bool {
        self.occupied.load(Ordering::Acquire)
    }
}

#[derive(Debug)]
pub struct SessionLease {
    occupied: Arc<AtomicBool>,
}

impl Drop for SessionLease {
    fn drop(&mut self) {
        self.occupied.store(false, Ordering::Release);
    }
}

fn detect_hardware() -> HardwareProfile {
    let mut backends = vec![ComputeBackend::Cpu];
    if cfg!(target_os = "macos") && cfg!(target_arch = "aarch64") {
        backends.push(ComputeBackend::Metal);
    }
    // Vendor GPU and Vulkan availability is finalized by the native worker
    // handshake; environment overrides make headless/container deployments
    // explicit.
    if cuda_runtime_available() {
        backends.push(ComputeBackend::Cuda);
    }
    if rocm_runtime_available() {
        backends.push(ComputeBackend::Rocm);
    }
    if std::env::var_os("OPENFLOW_VULKAN_AVAILABLE").is_some()
        || cfg!(target_os = "linux") && linux_render_node_available()
    {
        backends.push(ComputeBackend::Vulkan);
    }
    HardwareProfile {
        os: std::env::consts::OS.into(),
        architecture: std::env::consts::ARCH.into(),
        logical_cpus: thread::available_parallelism().map_or(1, usize::from),
        system_memory_bytes: system_memory_bytes(),
        accelerator_memory_bytes: accelerator_memory_bytes(),
        backends,
    }
}

fn cuda_runtime_available() -> bool {
    if std::env::var_os("OPENFLOW_CUDA_AVAILABLE").is_some() {
        return true;
    }
    #[cfg(target_os = "linux")]
    {
        return nvidia_vram_bytes().is_some();
    }
    #[allow(unreachable_code)]
    false
}

fn rocm_runtime_available() -> bool {
    if std::env::var_os("OPENFLOW_ROCM_AVAILABLE").is_some() {
        return true;
    }
    #[cfg(target_os = "linux")]
    {
        // ROCr opens the KFD device read/write. Checking the same access here
        // prevents advertising ROCm when the user lacks video/render device
        // permissions and model loading would inevitably fail later.
        return fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/kfd")
            .is_ok();
    }
    #[allow(unreachable_code)]
    false
}

fn accelerator_memory_bytes() -> Option<u64> {
    if let Ok(explicit) = std::env::var("OPENFLOW_ACCELERATOR_MEMORY_BYTES") {
        return explicit.parse().ok().filter(|bytes| *bytes > 0);
    }
    #[cfg(target_os = "linux")]
    return linux_drm_vram_bytes().or_else(nvidia_vram_bytes);
    #[allow(unreachable_code)]
    None
}

#[cfg(target_os = "linux")]
fn linux_render_node_available() -> bool {
    fs::read_dir("/dev/dri").is_ok_and(|entries| {
        entries.filter_map(Result::ok).any(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("renderD"))
        })
    })
}

#[cfg(not(target_os = "linux"))]
fn linux_render_node_available() -> bool {
    false
}

#[cfg(target_os = "linux")]
fn linux_drm_vram_bytes() -> Option<u64> {
    fs::read_dir("/sys/class/drm")
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                return false;
            };
            name.strip_prefix("card").is_some_and(|suffix| {
                !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
            })
        })
        .filter_map(|entry| {
            fs::read_to_string(entry.path().join("device/mem_info_vram_total")).ok()
        })
        .filter_map(|value| value.trim().parse::<u64>().ok())
        .filter(|bytes| *bytes > 0)
        .max()
}

#[cfg(target_os = "linux")]
fn nvidia_vram_bytes() -> Option<u64> {
    let output = Command::new("nvidia-smi")
        .args(["--query-gpu=memory.total", "--format=csv,noheader,nounits"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_nvidia_vram_mib(&String::from_utf8(output.stdout).ok()?)
}

#[cfg(target_os = "linux")]
/// Parses `nvidia-smi` MiB output and returns the largest attached card.
#[doc(hidden)]
#[must_use]
pub fn parse_nvidia_vram_mib(output: &str) -> Option<u64> {
    output
        .lines()
        .filter_map(|line| line.trim().parse::<u64>().ok())
        .filter_map(|mibibytes| mibibytes.checked_mul(1024 * 1024))
        .max()
}

fn system_memory_bytes() -> Option<u64> {
    if let Ok(explicit) = std::env::var("OPENFLOW_SYSTEM_MEMORY_BYTES") {
        return explicit.parse().ok();
    }
    #[cfg(target_os = "linux")]
    {
        let contents = fs::read_to_string("/proc/meminfo").ok()?;
        let kibibytes = contents
            .lines()
            .find_map(|line| line.strip_prefix("MemTotal:"))?
            .split_whitespace()
            .next()?
            .parse::<u64>()
            .ok()?;
        return kibibytes.checked_mul(1024);
    }
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .ok()?;
        return String::from_utf8(output.stdout).ok()?.trim().parse().ok();
    }
    #[allow(unreachable_code)]
    None
}
