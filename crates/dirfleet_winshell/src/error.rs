use std::fmt;

/// Why the shell could not do something, in words fit for a log or a toast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(String);

impl Error {
    pub(crate) fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<Error> for String {
    fn from(error: Error) -> Self {
        error.0
    }
}

/// Turns a Win32 error into this crate's own, keeping the system's text.
pub trait ShellResult<T> {
    fn shell(self) -> Result<T, Error>;
}

impl<T> ShellResult<T> for windows::core::Result<T> {
    fn shell(self) -> Result<T, Error> {
        self.map_err(|error| Error(error.to_string()))
    }
}
