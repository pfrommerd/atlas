#[derive(Debug)]
pub struct Error(Repr);

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum ErrorKind {
    BadFormat,
    BadType,
    BadPointer,
    IO,
    NotFound,
    Compile,
    Internal,
    IncorrectType
}

impl Error {
    pub fn new<E>(error: E) -> Error
    where
        E: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        Error(Repr::Custom(error.into()))
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
    Custom(Box<dyn std::error::Error>)
}