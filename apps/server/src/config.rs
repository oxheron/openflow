use crate::error::ServerError;
use std::{
    env,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    time::Duration,
};

#[derive(Debug, Clone)]
pub struct TlsConfig {
    pub certificate_path: PathBuf,
    pub private_key_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub bind_address: SocketAddr,
    pub tls: Option<TlsConfig>,
    pub model_registry_path: Option<PathBuf>,
    pub model_cache_dir: PathBuf,
    pub auth_store_path: PathBuf,
    pub admin_token: Option<String>,
    pub rotate_bootstrap_admin_token: bool,
    pub pairing_ttl: Duration,
    pub max_audio_bytes_per_session: usize,
    pub partial_decode_bytes: usize,
    pub asr_worker_path: Option<PathBuf>,
    pub llm_worker_path: Option<PathBuf>,
    pub worker_backend: String,
    /// Allows a foreground terminal to approve new devices interactively.
    pub interactive_pairing: bool,
    /// Prints finalized transcripts to the server terminal when explicitly enabled.
    pub print_transcripts: bool,
}

impl ServerConfig {
    /// Builds server configuration from the `OPENFLOW_*` environment.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed addresses, incomplete TLS settings, or
    /// another unsafe configuration.
    pub fn from_env() -> Result<Self, ServerError> {
        let state_dir = dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("openflow");
        let cache_dir = dirs::cache_dir()
            .unwrap_or_else(|| state_dir.join("cache"))
            .join("openflow")
            .join("models");

        let bind_address = env::var("OPENFLOW_BIND")
            .unwrap_or_else(|_| "127.0.0.1:8765".into())
            .parse()
            .map_err(|error| {
                ServerError::Configuration(format!("invalid OPENFLOW_BIND: {error}"))
            })?;
        let certificate_path = env::var_os("OPENFLOW_TLS_CERT").map(PathBuf::from);
        let private_key_path = env::var_os("OPENFLOW_TLS_KEY").map(PathBuf::from);
        let tls = match (certificate_path, private_key_path) {
            (Some(certificate_path), Some(private_key_path)) => Some(TlsConfig {
                certificate_path,
                private_key_path,
            }),
            (None, None) => None,
            _ => {
                return Err(ServerError::Configuration(
                    "OPENFLOW_TLS_CERT and OPENFLOW_TLS_KEY must be provided together".into(),
                ));
            }
        };

        let config = Self {
            bind_address,
            tls,
            model_registry_path: env::var_os("OPENFLOW_MODEL_REGISTRY").map(PathBuf::from),
            model_cache_dir: env::var_os("OPENFLOW_MODEL_CACHE").map_or(cache_dir, PathBuf::from),
            auth_store_path: env::var_os("OPENFLOW_AUTH_STORE")
                .map_or_else(|| state_dir.join("auth.json"), PathBuf::from),
            admin_token: env::var("OPENFLOW_ADMIN_TOKEN").ok(),
            rotate_bootstrap_admin_token: env::var_os("OPENFLOW_ROTATE_BOOTSTRAP_ADMIN_TOKEN")
                .is_some(),
            pairing_ttl: Duration::from_secs(10 * 60),
            // 60 seconds of 16 kHz mono PCM S16LE. This also keeps the
            // JSON-expanded native worker frame below its 16 MiB ceiling.
            max_audio_bytes_per_session: 60 * 16_000 * 2,
            // Decode a rolling partial every two seconds of PCM. Whisper works
            // on a much larger internal window, so decoding every 500 ms adds
            // substantial GPU work without useful transcript stability.
            partial_decode_bytes: 2 * 16_000 * 2,
            asr_worker_path: env::var_os("OPENFLOW_ASR_WORKER").map(PathBuf::from),
            llm_worker_path: env::var_os("OPENFLOW_LLM_WORKER").map(PathBuf::from),
            worker_backend: env::var("OPENFLOW_WORKER_BACKEND").unwrap_or_else(|_| "auto".into()),
            interactive_pairing: env::var_os("OPENFLOW_INTERACTIVE_PAIRING").is_some(),
            print_transcripts: env_flag("OPENFLOW_PRINT_TRANSCRIPTS"),
        };
        config.validate()?;
        Ok(config)
    }

    /// Rejects unsafe listener and worker settings.
    ///
    /// # Errors
    ///
    /// Returns an error for plaintext non-loopback binds, zero audio limits, or
    /// an unknown worker backend.
    pub fn validate(&self) -> Result<(), ServerError> {
        if !is_loopback(self.bind_address.ip()) && self.tls.is_none() {
            return Err(ServerError::Configuration(
                "non-loopback binds require TLS; configure OPENFLOW_TLS_CERT and OPENFLOW_TLS_KEY"
                    .into(),
            ));
        }
        if self.max_audio_bytes_per_session == 0 || self.partial_decode_bytes == 0 {
            return Err(ServerError::Configuration(
                "audio limits must be greater than zero".into(),
            ));
        }
        if !matches!(
            self.worker_backend.as_str(),
            "auto" | "mock" | "whisper.cpp" | "llama.cpp"
        ) {
            return Err(ServerError::Configuration(
                "OPENFLOW_WORKER_BACKEND must be auto, mock, whisper.cpp, or llama.cpp".into(),
            ));
        }
        Ok(())
    }
}

fn is_loopback(ip: IpAddr) -> bool {
    ip.is_loopback()
}

fn env_flag(name: &str) -> bool {
    env::var_os(name).is_some_and(|value| {
        let value = value.to_string_lossy();
        !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
    })
}
