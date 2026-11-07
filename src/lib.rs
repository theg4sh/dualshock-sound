pub mod capture;
pub mod hid;
pub mod protocol;
pub mod resampler;
pub mod sbc;
pub mod stream;

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;

pub fn invalid_input(message: impl Into<String>) -> Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, message.into()).into()
}
