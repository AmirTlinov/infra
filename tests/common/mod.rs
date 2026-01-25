use once_cell::sync::Lazy;
use tokio::sync::Mutex;

pub static ENV_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));
