pub mod network {
    pub const POSTGRES_DEFAULT_PORT: u16 = 5432;
    pub const SSH_DEFAULT_PORT: u16 = 22;
    pub const TIMEOUT_SSH_READY_MS: u64 = 10_000;
    pub const TIMEOUT_MCP_TOOL_CALL_MS: u64 = 55_000;
    pub const TIMEOUT_SSH_EXEC_DEFAULT_MS: u64 = 45_000;
    pub const TIMEOUT_SSH_EXEC_HARD_GRACE_MS: u64 = 2_000;
    pub const TIMEOUT_SSH_DETACHED_START_MS: u64 = 20_000;
    pub const TIMEOUT_API_REQUEST_MS: u64 = 30_000;
    pub const TIMEOUT_MUTEX_MS: u64 = 30_000;
    pub const TIMEOUT_CONNECTION_MS: u64 = 5_000;
    pub const TIMEOUT_IDLE_MS: u64 = 300_000;
    pub const KEEPALIVE_INTERVAL_MS: u64 = 30_000;
    pub const CLEANUP_INTERVAL_MS: u64 = 300_000;
    pub const DANGEROUS_PORTS: &[u16] = &[22, 23, 25, 53, 135, 139, 445, 593, 636, 993, 995];
}

pub mod limits {
    pub const MAX_CONNECTIONS: usize = 10;
    pub const MAX_PORT: u16 = 65_535;
    pub const MIN_PORT: u16 = 1;
    pub const SAMPLE_DATA_LIMIT: usize = 10;
    pub const LOG_SUBSTRING_LENGTH: usize = 100;
    pub const COMMAND_SUBSTRING_LENGTH: usize = 50;
}

pub mod timeouts {
    pub const BUFFER_FLUSH_MS: u64 = 5_000;
    pub const RATE_LIMIT_WINDOW_MS: u64 = 60_000;
    pub const STATISTICS_WINDOW_MS: u64 = 3_600_000;
    pub const STATISTICS_MINUTE_MS: u64 = 60_000;
    pub const CLEANUP_OLD_LOGS_MS: u64 = 10_000;
    pub const CONNECTION_TIMEOUT_MS: u64 = 2_000;
    pub const IDLE_TIMEOUT_MS: u64 = 30_000;
}

pub mod retry {
    pub const MAX_ATTEMPTS: usize = 3;
    pub const BASE_DELAY_MS: u64 = 250;
    pub const MAX_DELAY_MS: u64 = 5_000;
    pub const JITTER: f64 = 0.2;
    pub const STATUS_CODES: &[u16] = &[408, 429, 500, 502, 503, 504];
}

pub mod pagination {
    pub const MAX_PAGES: usize = 10;
    pub const PAGE_SIZE: usize = 100;
}

pub mod cache {
    pub const DEFAULT_TTL_MS: u64 = 60_000;
}

pub mod buffers {
    pub const LOG_BUFFER_SIZE: usize = 100;
    pub const SLIDING_WINDOW_SIZE: usize = 1_000;
    pub const MAX_LOG_SIZE: usize = 10 * 1024 * 1024;
    pub const MAX_LOG_FILES: usize = 10;
    pub const CRYPTO_KEY_SIZE: usize = 32;
    pub const CRYPTO_IV_SIZE: usize = 12;
    pub const CRYPTO_TAG_SIZE: usize = 16;
    pub const CRYPTO_SALT_SIZE: usize = 32;
}

pub mod crypto {
    pub const ALGORITHM: &str = "aes-256-gcm";
    pub const PBKDF2_ITERATIONS: u32 = 100_000;
    pub const HASH_LENGTH: usize = 64;
    pub const HASH_ALGORITHM: &str = "sha512";
}

pub mod rate_limit {
    pub const WINDOW_MS: u64 = 60_000;
    pub const MAX_REQUESTS: usize = 100;
    pub const CLEANUP_INTERVAL_MS: u64 = 300_000;
}

pub mod localhost {
    pub const NAMES: &[&str] = &["localhost", "127.0.0.1"];
    pub const PRIVATE_PREFIXES: &[&str] = &["192.168.", "10."];
}

pub mod protocols {
    pub const ALLOWED_HTTP: &[&str] = &["http:", "https:"];
}
