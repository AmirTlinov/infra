use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
}

impl LogLevel {
    fn from_env() -> Self {
        match std::env::var("LOG_LEVEL")
            .unwrap_or_else(|_| "info".to_string())
            .to_lowercase()
            .as_str()
        {
            "error" => LogLevel::Error,
            "warn" => LogLevel::Warn,
            "debug" => LogLevel::Debug,
            _ => LogLevel::Info,
        }
    }

    fn allows(self, other: LogLevel) -> bool {
        use LogLevel::*;
        let rank = match self {
            Error => 0,
            Warn => 1,
            Info => 2,
            Debug => 3,
        };
        let other_rank = match other {
            Error => 0,
            Warn => 1,
            Info => 2,
            Debug => 3,
        };
        other_rank <= rank
    }
}

#[derive(Debug, Default)]
struct Counters {
    error: u64,
    warn: u64,
    info: u64,
    debug: u64,
}

#[derive(Debug, Clone)]
pub struct Logger {
    context: String,
    level: LogLevel,
    counters: std::sync::Arc<Mutex<Counters>>,
}

impl Logger {
    pub fn new(context: &str) -> Self {
        Self {
            context: context.to_string(),
            level: LogLevel::from_env(),
            counters: std::sync::Arc::new(Mutex::new(Counters::default())),
        }
    }

    pub fn child(&self, suffix: &str) -> Self {
        let context = if suffix.is_empty() {
            self.context.clone()
        } else {
            format!("{}:{}", self.context, suffix)
        };
        Self {
            context,
            level: self.level,
            counters: self.counters.clone(),
        }
    }

    pub fn set_level(&mut self, level: LogLevel) {
        self.level = level;
    }

    fn log(&self, level: LogLevel, message: &str, meta: Option<&serde_json::Value>) {
        if !self.level.allows(level) {
            return;
        }
        if let Ok(mut counters) = self.counters.lock() {
            match level {
                LogLevel::Error => counters.error += 1,
                LogLevel::Warn => counters.warn += 1,
                LogLevel::Info => counters.info += 1,
                LogLevel::Debug => counters.debug += 1,
            }
        }
        let timestamp = chrono::Utc::now().to_rfc3339();
        let level_str = match level {
            LogLevel::Error => "ERROR",
            LogLevel::Warn => "WARN",
            LogLevel::Info => "INFO",
            LogLevel::Debug => "DEBUG",
        };
        let meta_suffix = meta
            .and_then(|m| if m.is_null() { None } else { Some(m) })
            .map(|m| format!(" {}", m))
            .unwrap_or_default();
        eprintln!(
            "[{}] {} [{}] {}{}",
            timestamp, level_str, self.context, message, meta_suffix
        );
    }

    pub fn error(&self, message: &str, meta: Option<&serde_json::Value>) {
        self.log(LogLevel::Error, message, meta);
    }

    pub fn warn(&self, message: &str, meta: Option<&serde_json::Value>) {
        self.log(LogLevel::Warn, message, meta);
    }

    pub fn info(&self, message: &str, meta: Option<&serde_json::Value>) {
        self.log(LogLevel::Info, message, meta);
    }

    pub fn debug(&self, message: &str, meta: Option<&serde_json::Value>) {
        self.log(LogLevel::Debug, message, meta);
    }

    pub fn stats(&self) -> serde_json::Value {
        let counters = self.counters.lock().unwrap_or_else(|err| err.into_inner());
        serde_json::json!({
            "level": format!("{:?}", self.level).to_lowercase(),
            "context": self.context,
            "error": counters.error,
            "warn": counters.warn,
            "info": counters.info,
            "debug": counters.debug,
        })
    }
}
