use std::path::PathBuf;

pub const OK: i32 = 0;
pub const FAILURE: i32 = 1;
pub const USAGE: i32 = 2;
pub const POOL_EXHAUSTED: i32 = 3;
pub const NO_FREE_PORT: i32 = 4;
pub const LOCK_TIMEOUT: i32 = 5;

#[derive(Debug)]
pub struct LockTimeout {
    pub path: PathBuf,
    pub seconds: u64,
}

impl std::fmt::Display for LockTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "timed out after {}s waiting for {}; another simporter command is still running",
            self.seconds,
            self.path.display()
        )
    }
}

impl std::error::Error for LockTimeout {}

#[derive(Debug)]
pub struct Usage {
    pub message: String,
}

impl std::fmt::Display for Usage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Usage {}

pub fn usage(message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(Usage {
        message: message.into(),
    })
}

#[derive(Debug)]
pub struct NoFreePort {
    pub message: String,
}

impl std::fmt::Display for NoFreePort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for NoFreePort {}

pub fn code_for(error: &anyhow::Error) -> i32 {
    if error.downcast_ref::<LockTimeout>().is_some() {
        return LOCK_TIMEOUT;
    }
    if error.downcast_ref::<NoFreePort>().is_some() {
        return NO_FREE_PORT;
    }
    if error.downcast_ref::<Usage>().is_some() {
        return USAGE;
    }
    FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_timeout_maps_to_its_own_code() {
        let error = anyhow::Error::new(LockTimeout {
            path: PathBuf::from("/tmp/x.lock"),
            seconds: 10,
        });
        assert_eq!(code_for(&error), LOCK_TIMEOUT);
    }

    #[test]
    fn port_exhaustion_maps_to_its_own_code() {
        let error = anyhow::Error::new(NoFreePort {
            message: "none".to_string(),
        });
        assert_eq!(code_for(&error), NO_FREE_PORT);
    }

    #[test]
    fn usage_problems_map_to_their_own_code() {
        assert_eq!(code_for(&usage("pass --owner")), USAGE);
    }

    #[test]
    fn other_errors_map_to_generic_failure() {
        assert_eq!(code_for(&anyhow::anyhow!("boom")), FAILURE);
    }
}
