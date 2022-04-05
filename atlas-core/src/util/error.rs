#[derive(Debug)]
pub struct Error(Repr);

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    BadFormat,
    BadType,
    BadPointer,
    IO,
    Interrupted,
    NotFound,
    Filesystem,
    Compile,
    Internal,
    IncorrectType,
    Custom
}

impl Error {
    pub fn new<E>(error: E) -> Error
    where
        E: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        Error(Repr::Custom(ErrorKind::Custom, error.into()))
    }

    pub fn kind(&self) -> ErrorKind {
        match &self.0 {
            Repr::Custom(c, _) => c.clone(),
            Repr::Simple(c) => c.clone(),
            Repr::SimpleMessage(c, _) => c.clone()
        }
    }

    pub fn new_const(kind : ErrorKind, message: &'static str) -> Self {
        Error(Repr::SimpleMessage(kind, message))
    }
}

impl From<ErrorKind> for Error {
    fn from(e: ErrorKind) -> Self {
        Error(Repr::Simple(e))
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::new(e)
    }
}

#[derive(Debug)]
enum Repr {
    Simple(ErrorKind),
    SimpleMessage(ErrorKind, &'static str),
    Custom(ErrorKind, Box<dyn std::error::Error + Send>)
}