use std::{error::Error, fmt, io};

#[derive(Debug)]
pub enum JournalError {
    Io(io::Error),
    NotAJournal,
    Locked,
    UnsupportedVersion(u32),
    RecordTooLarge { len: usize, max: u32 },
    SeqCompacted { requested: u64, first: u64 },
    AckBeyondEnd { ack: u64, last: u64 },
    Sealed { from_seq: u64 },
}

impl fmt::Display for JournalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "journal I/O error: {error}"),
            Self::NotAJournal => f.write_str("not a valid journal"),
            Self::Locked => f.write_str("journal is locked"),
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported journal version: {version}")
            }
            Self::RecordTooLarge { len, max } => {
                write!(f, "record length {len} exceeds limit {max}")
            }
            Self::SeqCompacted { requested, first } => {
                write!(
                    f,
                    "sequence {requested} was compacted; first available is {first}"
                )
            }
            Self::AckBeyondEnd { ack, last } => {
                write!(f, "acknowledgement {ack} exceeds last sequence {last}")
            }
            Self::Sealed { from_seq } => {
                write!(f, "journal is sealed from sequence {from_seq}")
            }
        }
    }
}

impl Error for JournalError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for JournalError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
