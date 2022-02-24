#[derive(Debug)]
pub struct Error(Repr);

#[derive(Debug)]
pub enum ErrorKind {
    BadFormat,
    BadPointer,
    IO,
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

#[derive(Debug)]
enum Repr {
    Simple(ErrorKind),
    SimpleMessage(ErrorKind, &'static str),
    Custom(Box<dyn std::error::Error>)
}