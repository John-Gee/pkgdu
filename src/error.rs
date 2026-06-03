use std::fmt;

#[derive(Debug)]
pub enum PkgduError {
    Config(String),
    Io(std::io::Error),
    ParsePacmanConf(String),
}

impl fmt::Display for PkgduError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PkgduError::Config(msg) => write!(f, "config error: {}", msg),
            PkgduError::Io(err) => write!(f, "io error: {}", err),
            PkgduError::ParsePacmanConf(msg) => write!(f, "parse pacman conf error: {}", msg),
        }
    }
}

impl std::error::Error for PkgduError {}

impl From<std::io::Error> for PkgduError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<regex::Error> for PkgduError {
    fn from(value: regex::Error) -> Self {
        Self::Config(format!("regex parse error: {}", value))
    }
}

pub type Result<T> = std::result::Result<T, PkgduError>;
