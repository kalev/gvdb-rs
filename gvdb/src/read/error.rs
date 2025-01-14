use std::error::Error;
use std::fmt::{Display, Formatter};
use std::num::TryFromIntError;
use std::path::{Path, PathBuf};
use std::string::FromUtf8Error;

/// An error that can occur during GVDB file reading
#[derive(Debug)]
pub enum GvdbReaderError {
    /// Error converting a string to UTF-8
    Utf8(FromUtf8Error),

    /// Generic I/O error. Path contains an optional filename if applicable
    Io(std::io::Error, Option<PathBuf>),

    /// An error occured when deserializing variant data with zvariant
    ZVariant(zvariant::Error),

    /// Tried to access an invalid data offset
    DataOffset,

    /// Tried to read unaligned data
    DataAlignment,

    /// Unexpected data
    InvalidData,

    /// Like InvalidData but with context information in the provided string
    DataError(String),

    /// The item with the specified key does not exist in the hash table
    KeyError(String),
}

impl GvdbReaderError {
    pub(crate) fn from_io_with_filename(
        filename: &Path,
    ) -> impl FnOnce(std::io::Error) -> GvdbReaderError {
        let path = filename.to_path_buf();
        move |err| GvdbReaderError::Io(err, Some(path))
    }
}

impl Error for GvdbReaderError {}

impl From<FromUtf8Error> for GvdbReaderError {
    fn from(err: FromUtf8Error) -> Self {
        Self::Utf8(err)
    }
}

impl From<zvariant::Error> for GvdbReaderError {
    fn from(err: zvariant::Error) -> Self {
        Self::ZVariant(err)
    }
}

impl From<TryFromIntError> for GvdbReaderError {
    fn from(_err: TryFromIntError) -> Self {
        Self::DataOffset
    }
}

impl<S, T> From<safe_transmute::Error<'_, S, T>> for GvdbReaderError {
    fn from(err: safe_transmute::Error<S, T>) -> Self {
        match err {
            safe_transmute::Error::Guard(guard_err) => {
                if guard_err.actual > guard_err.required {
                    Self::DataError(format!(
                        "Found {} unexpected trailing bytes at the end while reading data",
                        guard_err.actual - guard_err.required
                    ))
                } else {
                    Self::DataError(format!(
                        "Missing {} bytes to read data",
                        guard_err.required - guard_err.actual
                    ))
                }
            }
            safe_transmute::Error::Unaligned(..) => {
                Self::DataError("Unaligned data read".to_string())
            }
            _ => Self::InvalidData,
        }
    }
}

impl Display for GvdbReaderError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            GvdbReaderError::Utf8(err) => write!(f, "Error converting string to UTF-8: {}", err),
            GvdbReaderError::Io(err, path) => {
                if let Some(path) = path {
                    write!(
                        f,
                        "I/O error while reading file '{}': {}",
                        path.display(),
                        err
                    )
                } else {
                    write!(f, "I/O error: {}", err)
                }
            }
            GvdbReaderError::ZVariant(err) => write!(f, "Error parsing ZVariant data: {}", err),
            GvdbReaderError::DataOffset => {
                write!(f, "Tried to access an invalid data offset. Most likely reason is a corrupted GVDB file")
            }
            GvdbReaderError::DataAlignment => {
                write!(
                    f,
                    "Tried to read unaligned data. Most likely reason is a corrupted GVDB file"
                )
            }
            GvdbReaderError::InvalidData => {
                write!(
                    f,
                    "Unexpected data. Most likely reason is a corrupted GVDB file"
                )
            }
            GvdbReaderError::DataError(msg) => {
                write!(
                    f,
                    "A data inconsistency error occured while reading gvdb file: {}",
                    msg
                )
            }
            GvdbReaderError::KeyError(key) => {
                write!(f, "The item with the key '{}' does not exist", key)
            }
        }
    }
}

/// The Result type for [`GvdbReaderError`]
pub type GvdbReaderResult<T> = Result<T, GvdbReaderError>;

#[cfg(test)]
mod test {
    use crate::read::{GvdbHeader, GvdbPointer, GvdbReaderError};
    use matches::assert_matches;
    use safe_transmute::{transmute_one_pedantic, transmute_one_to_bytes, transmute_vec};
    use std::num::TryFromIntError;

    #[test]
    fn derives() {
        let err = GvdbReaderError::InvalidData;
        assert!(format!("{:?}", err).contains("InvalidData"));
        assert!(format!("{}", err).contains("Unexpected data"));
    }

    #[test]
    fn from() {
        let io_res = std::fs::File::open("test/invalid_file_name");
        let err = GvdbReaderError::Io(io_res.unwrap_err(), None);
        assert!(format!("{}", err).contains("I/O"));

        let utf8_err = String::from_utf8([0xC3, 0x28].to_vec()).unwrap_err();
        let err = GvdbReaderError::from(utf8_err);
        assert!(format!("{}", err).contains("UTF-8"));

        let res: Result<u16, TryFromIntError> = u32::MAX.try_into();
        let err = GvdbReaderError::from(res.unwrap_err());
        assert_matches!(err, GvdbReaderError::DataOffset);
        assert!(format!("{}", err).contains("data offset"));

        let err = GvdbReaderError::DataError("my data error".to_string());
        assert!(format!("{}", err).contains("my data error"));

        let err = GvdbReaderError::KeyError("test".to_string());
        assert!(format!("{}", err).contains("test"));

        let err = GvdbReaderError::from(zvariant::Error::Message("test".to_string()));
        assert!(format!("{}", err).contains("test"));

        let to_transmute = GvdbHeader::new(false, 0, GvdbPointer::NULL);
        let mut bytes = transmute_one_to_bytes(&to_transmute).to_vec();
        bytes.extend_from_slice(b"fail");
        let res = transmute_one_pedantic::<GvdbHeader>(&bytes);
        let err = GvdbReaderError::from(res.unwrap_err());
        assert_matches!(err, GvdbReaderError::DataError(_));
        assert!(format!("{}", err).contains("unexpected trailing bytes"));

        let to_transmute = GvdbHeader::new(false, 0, GvdbPointer::NULL);
        let mut bytes = transmute_one_to_bytes(&to_transmute).to_vec();
        bytes.remove(bytes.len() - 1);
        let res = transmute_one_pedantic::<GvdbHeader>(&bytes);
        let err = GvdbReaderError::from(res.unwrap_err());
        assert_matches!(err, GvdbReaderError::DataError(_));
        assert!(format!("{}", err).contains("Missing 1 bytes"));

        let to_transmute = GvdbHeader::new(false, 0, GvdbPointer::NULL);
        let mut bytes = b"unalign".to_vec();
        bytes.extend_from_slice(transmute_one_to_bytes(&to_transmute));
        let res = transmute_one_pedantic::<GvdbHeader>(&bytes[7..]);
        let err = GvdbReaderError::from(res.unwrap_err());
        assert_matches!(err, GvdbReaderError::DataError(_));
        assert!(format!("{}", err).contains("Unaligned"));

        let bytes = vec![0u8; 5];
        let res = transmute_vec::<u8, GvdbHeader>(bytes);
        let err = GvdbReaderError::from(res.unwrap_err());
        assert_matches!(err, GvdbReaderError::InvalidData);
    }
}
