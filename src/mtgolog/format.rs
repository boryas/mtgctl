//! Binary framing of MTGO's `Match_GameLog_<guid>.dat` / `Match_GameChat_<guid>.dat` files.
//!
//! Layout (reverse-engineered, validated against every file on disk):
//!
//! ```text
//! u16 LE version (1)
//! str  match guid            ; str = 7-bit-encoded length (.NET BinaryWriter) + UTF-8 bytes
//! u16 LE kind                ; 4 = game log, 3 = game chat
//! str  match guid (again)
//! record*                    ; until EOF
//!   i64 LE .NET DateTime ticks (100ns since 0001-01-01)
//!   str  sender              ; "" for system lines, a username for chat lines
//!   str  text
//! ```

use chrono::{NaiveDate, NaiveDateTime, TimeDelta};
use std::fmt;
use std::fs;
use std::path::Path;

#[derive(Debug)]
pub enum FormatError {
    Io(std::io::Error),
    Truncated { offset: usize, what: &'static str },
    Utf8 { offset: usize },
    BadVersion(u16),
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FormatError::Io(e) => write!(f, "io: {e}"),
            FormatError::Truncated { offset, what } => write!(f, "truncated reading {what} at {offset}"),
            FormatError::Utf8 { offset } => write!(f, "invalid utf-8 at {offset}"),
            FormatError::BadVersion(v) => write!(f, "unexpected version {v}"),
        }
    }
}

impl From<std::io::Error> for FormatError {
    fn from(e: std::io::Error) -> Self {
        FormatError::Io(e)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogKind {
    GameLog,
    GameChat,
    Other(u16),
}

#[derive(Debug, Clone)]
pub struct RawRecord {
    pub ts: NaiveDateTime,
    pub sender: String,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct RawLog {
    pub version: u16,
    pub guid: String,
    pub kind: LogKind,
    pub records: Vec<RawRecord>,
}

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn u16(&mut self) -> Result<u16, FormatError> {
        let b = self.take(2, "u16")?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn i64(&mut self) -> Result<i64, FormatError> {
        let b = self.take(8, "i64")?;
        Ok(i64::from_le_bytes(b.try_into().unwrap()))
    }

    fn take(&mut self, n: usize, what: &'static str) -> Result<&'a [u8], FormatError> {
        if self.pos + n > self.buf.len() {
            return Err(FormatError::Truncated { offset: self.pos, what });
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    /// .NET BinaryWriter.Write(string): 7-bit encoded length prefix, then UTF-8.
    fn str(&mut self) -> Result<String, FormatError> {
        let mut len: usize = 0;
        let mut shift = 0;
        loop {
            let b = self.take(1, "string length")?[0];
            len |= ((b & 0x7f) as usize) << shift;
            shift += 7;
            if b & 0x80 == 0 {
                break;
            }
        }
        let offset = self.pos;
        let bytes = self.take(len, "string body")?;
        String::from_utf8(bytes.to_vec()).map_err(|_| FormatError::Utf8 { offset })
    }

    fn at_end(&self) -> bool {
        self.pos >= self.buf.len()
    }
}

/// .NET DateTime ticks (100ns since 0001-01-01T00:00:00) → NaiveDateTime.
pub fn ticks_to_datetime(ticks: i64) -> NaiveDateTime {
    let epoch = NaiveDate::from_ymd_opt(1, 1, 1).unwrap().and_hms_opt(0, 0, 0).unwrap();
    epoch + TimeDelta::microseconds(ticks / 10)
}

pub fn parse_bytes(buf: &[u8]) -> Result<RawLog, FormatError> {
    let mut c = Cursor { buf, pos: 0 };
    let version = c.u16()?;
    if version != 1 {
        return Err(FormatError::BadVersion(version));
    }
    let guid = c.str()?;
    let kind = match c.u16()? {
        4 => LogKind::GameLog,
        3 => LogKind::GameChat,
        k => LogKind::Other(k),
    };
    let _guid2 = c.str()?;
    let mut records = Vec::new();
    while !c.at_end() {
        let ts = ticks_to_datetime(c.i64()?);
        let sender = c.str()?;
        let text = c.str()?;
        records.push(RawRecord { ts, sender, text });
    }
    Ok(RawLog { version, guid, kind, records })
}

pub fn read_log(path: &Path) -> Result<RawLog, FormatError> {
    let buf = fs::read(path)?;
    parse_bytes(&buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(bytes: &mut Vec<u8>, text: &str) {
        let mut len = text.len();
        loop {
            let b = (len & 0x7f) as u8;
            len >>= 7;
            if len == 0 {
                bytes.push(b);
                break;
            }
            bytes.push(b | 0x80);
        }
        bytes.extend_from_slice(text.as_bytes());
    }

    #[test]
    fn roundtrip_minimal_log() {
        let mut b = Vec::new();
        b.extend_from_slice(&1u16.to_le_bytes());
        s(&mut b, "abc-guid");
        b.extend_from_slice(&4u16.to_le_bytes());
        s(&mut b, "abc-guid");
        // 2026-01-01 00:00:00 in ticks
        let epoch = NaiveDate::from_ymd_opt(1, 1, 1).unwrap().and_hms_opt(0, 0, 0).unwrap();
        let ticks = (NaiveDate::from_ymd_opt(2026, 1, 1).unwrap().and_hms_opt(0, 0, 0).unwrap() - epoch).num_microseconds().unwrap() * 10;
        b.extend_from_slice(&ticks.to_le_bytes());
        s(&mut b, "");
        s(&mut b, &"@PTurn 1: boryas".repeat(1)); 
        b.extend_from_slice(&(ticks + 10_000_000).to_le_bytes());
        s(&mut b, "boryas");
        s(&mut b, &"x".repeat(200)); // exercises 2-byte length
        let log = parse_bytes(&b).unwrap();
        assert_eq!(log.guid, "abc-guid");
        assert_eq!(log.kind, LogKind::GameLog);
        assert_eq!(log.records.len(), 2);
        assert_eq!(log.records[0].text, "@PTurn 1: boryas");
        assert_eq!(log.records[0].ts.to_string(), "2026-01-01 00:00:00");
        assert_eq!(log.records[1].sender, "boryas");
        assert_eq!(log.records[1].text.len(), 200);
        assert_eq!(log.records[1].ts.to_string(), "2026-01-01 00:00:01");
    }
}
