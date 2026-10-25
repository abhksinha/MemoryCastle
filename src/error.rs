use std::fmt;

/// One error type for the whole crate. Errors are cold: a `String` is fine.
#[derive(Debug)]
pub enum Error {
    Db(rusqlite::Error),
    Io(std::io::Error),
    /// A user-facing message; printed as-is.
    Msg(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Db(e) => write!(f, "database: {e}"),
            Error::Io(e) => write!(f, "io: {e}"),
            Error::Msg(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for Error {}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Db(e)
    }
}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Msg(format!("json: {e}"))
    }
}
impl From<String> for Error {
    fn from(m: String) -> Self {
        Error::Msg(m)
    }
}
impl From<&str> for Error {
    fn from(m: &str) -> Self {
        Error::Msg(m.to_string())
    }
}

/// `err!("...")` builds an `Error::Msg` with `format!` arguments.
#[macro_export]
macro_rules! err {
    ($($arg:tt)*) => { $crate::Error::Msg(format!($($arg)*)) };
}
