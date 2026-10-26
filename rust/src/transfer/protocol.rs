use serde::{Deserialize, Serialize};
use std::io::{Read, Write};

pub const MAGIC_BYTE: u8 = 0x52; // 'R' für Rustle
pub const DEFAULT_CHUNK_SIZE: u32 = 256 * 1024; // 256 KB
pub const HEADER_SIZE: usize = 12;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgType {
    FileMeta = 0x01,
    DataChunk = 0x02,
    Control = 0x03,     // Pause, Cancel
    TransferAck = 0x04, // Empfangsbestätigung
    Unknown = 0xFF,
}

impl From<u8> for MsgType {
    fn from(v: u8) -> Self {
        match v {
            0x01 => MsgType::FileMeta,
            0x02 => MsgType::DataChunk,
            0x03 => MsgType::Control,
            0x04 => MsgType::TransferAck,
            _ => MsgType::Unknown,
        }
    }
}

/// Universeller 12-Byte Frame-Header:
/// [0]      Magic (0x52)
/// [1]      MsgType (u8)
/// [2..4]   Flags (u16, Big-Endian)
/// [4..8]   Chunk Index (u32, Big-Endian)
/// [8..12]  Payload Length (u32, Big-Endian)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameHeader {
    pub msg_type: MsgType,
    pub flags: u16,
    pub chunk_index: u32,
    pub payload_len: u32,
}

impl FrameHeader {
    pub fn new(msg_type: MsgType, chunk_index: u32, payload_len: u32) -> Self {
        Self {
            msg_type,
            flags: 0,
            chunk_index,
            payload_len,
        }
    }

    pub fn encode(&self, out: &mut [u8; HEADER_SIZE]) {
        out[0] = MAGIC_BYTE;
        out[1] = self.msg_type as u8;
        out[2..4].copy_from_slice(&self.flags.to_be_bytes());
        out[4..8].copy_from_slice(&self.chunk_index.to_be_bytes());
        out[8..12].copy_from_slice(&self.payload_len.to_be_bytes());
    }

    pub fn decode(bytes: &[u8; HEADER_SIZE]) -> Result<Self, String> {
        if bytes[0] != MAGIC_BYTE {
            return Err(format!(
                "Ungültiges Magic Byte: 0x{:02X} (erwartet: 0x{:02X})",
                bytes[0], MAGIC_BYTE
            ));
        }

        let msg_type = MsgType::from(bytes[1]);
        if msg_type == MsgType::Unknown {
            return Err(format!("Unbekannter MsgType: 0x{:02X}", bytes[1]));
        }

        let flags = u16::from_be_bytes([bytes[2], bytes[3]]);
        let chunk_index = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let payload_len = u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);

        Ok(Self {
            msg_type,
            flags,
            chunk_index,
            payload_len,
        })
    }
}

/// Metadaten zu einer Datei, die vor dem Streaming gesendet werden
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileMetadata {
    pub file_id: String,
    pub file_name: String,
    pub file_size: u64,
    pub chunk_size: u32,
    pub total_chunks: u32,
}

impl FileMetadata {
    pub fn from_file_info(file_id: String, file_name: String, file_size: u64, chunk_size: u32) -> Self {
        let chunk_sz = if chunk_size == 0 { DEFAULT_CHUNK_SIZE } else { chunk_size };
        let total_chunks = if file_size == 0 {
            1
        } else {
            ((file_size + chunk_sz as u64 - 1) / chunk_sz as u64) as u32
        };

        Self {
            file_id,
            file_name,
            file_size,
            chunk_size: chunk_sz,
            total_chunks,
        }
    }
}

/// Antwort des Empfängers auf ein FileMeta-Paket (z. B. Start ab Chunk X für Resume)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransferResponse {
    pub file_id: String,
    pub accepted: bool,
    pub start_chunk: u32,
    pub message: Option<String>,
}

/// Schreibt einen kompletten Frame (12-Byte-Header + Rohdaten) in den Writer
pub fn write_frame<W: Write>(
    writer: &mut W,
    header: &FrameHeader,
    payload: &[u8],
) -> std::io::Result<()> {
    let mut header_buf = [0u8; HEADER_SIZE];
    header.encode(&mut header_buf);
    writer.write_all(&header_buf)?;
    if !payload.is_empty() {
        writer.write_all(payload)?;
    }
    writer.flush()?;
    Ok(())
}

/// Liest den nächsten 12-Byte Frame-Header aus dem Reader
pub fn read_frame_header<R: Read>(reader: &mut R) -> std::io::Result<FrameHeader> {
    let mut header_buf = [0u8; HEADER_SIZE];
    reader.read_exact(&mut header_buf)?;
    FrameHeader::decode(&header_buf)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Liest exakt `len` Bytes Payload ein
pub fn read_exact_payload<R: Read>(reader: &mut R, len: usize) -> std::io::Result<Vec<u8>> {
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    Ok(buf)
}
