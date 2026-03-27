use std::fmt;
use std::fs;
use std::io;

/// Why a secret could not be resolved.
#[derive(Debug)]
pub enum ResolveError {
    /// FD was provided but reading it failed.
    Fd {
        name: String,
        fd: i32,
        source: io::Error,
    },
    /// No FD flag and env var is missing.
    Missing { name: String },
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResolveError::Fd { name, fd, source } => {
                write!(f, "failed to read {name} from fd {fd}: {source}")
            }
            ResolveError::Missing { name } => {
                write!(f, "{name}: provide via fd flag or set env var")
            }
        }
    }
}

/// Resolve a secret: try FD first, fall back to env var.
pub fn try_resolve(name: &str, fd: Option<i32>) -> Result<String, ResolveError> {
    match fd {
        Some(fd) => read_from_fd(fd).map_err(|source| ResolveError::Fd {
            name: name.to_string(),
            fd,
            source,
        }),
        None => std::env::var(name).map_err(|_| ResolveError::Missing {
            name: name.to_string(),
        }),
    }
}

/// Read a secret from an inherited file descriptor via `/proc/self/fd`.
fn read_from_fd(fd: i32) -> io::Result<String> {
    if fd < 3 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("secret fd must be >= 3, got {fd}"),
        ));
    }
    let contents = fs::read_to_string(format!("/proc/self/fd/{fd}"))?;
    Ok(contents.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    #[test]
    fn rejects_stdio_fds() {
        for fd in 0..3 {
            let err = read_from_fd(fd).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        }
    }

    #[test]
    fn reads_and_trims_trailing_newline() {
        let path = std::env::temp_dir().join("mdk-secret-test-read");
        fs::write(&path, "hunter2\n").unwrap();

        let file = fs::File::open(&path).unwrap();
        let fd = file.as_raw_fd();

        let secret = read_from_fd(fd).unwrap();
        assert_eq!(secret, "hunter2");

        drop(file);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn reads_value_without_trailing_newline() {
        let path = std::env::temp_dir().join("mdk-secret-test-notrim");
        fs::write(&path, "exact").unwrap();

        let file = fs::File::open(&path).unwrap();
        let secret = read_from_fd(file.as_raw_fd()).unwrap();
        assert_eq!(secret, "exact");

        drop(file);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn nonexistent_fd_errors() {
        let err = read_from_fd(9999).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn try_resolve_reads_from_fd() {
        let path = std::env::temp_dir().join("mdk-secret-test-resolve-fd");
        fs::write(&path, "fd-secret\n").unwrap();

        let file = fs::File::open(&path).unwrap();
        let fd = file.as_raw_fd();

        let secret = try_resolve("UNUSED_NAME", Some(fd)).unwrap();
        assert_eq!(secret, "fd-secret");

        drop(file);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn try_resolve_falls_back_to_env() {
        let var = "MDK_TEST_SECRET_FALLBACK_8392";
        std::env::set_var(var, "env-secret");

        let secret = try_resolve(var, None).unwrap();
        assert_eq!(secret, "env-secret");

        std::env::remove_var(var);
    }

    #[test]
    fn try_resolve_fd_takes_precedence_over_env() {
        let var = "MDK_TEST_SECRET_PRECEDENCE_7201";
        std::env::set_var(var, "from-env");

        let path = std::env::temp_dir().join("mdk-secret-test-precedence");
        fs::write(&path, "from-fd").unwrap();

        let file = fs::File::open(&path).unwrap();
        let secret = try_resolve(var, Some(file.as_raw_fd())).unwrap();
        assert_eq!(secret, "from-fd");

        drop(file);
        let _ = fs::remove_file(&path);
        std::env::remove_var(var);
    }

    #[test]
    fn try_resolve_missing_returns_error() {
        let var = "MDK_TEST_SECRET_MISSING_4810";
        std::env::remove_var(var);

        let err = try_resolve(var, None).unwrap_err();
        assert!(matches!(err, ResolveError::Missing { .. }));
    }

    #[test]
    fn try_resolve_bad_fd_returns_error() {
        let err = try_resolve("WHATEVER", Some(9999)).unwrap_err();
        assert!(matches!(err, ResolveError::Fd { .. }));
    }
}
