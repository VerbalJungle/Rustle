pub mod progress;
pub mod protocol;
pub mod receiver;
pub mod sender;

pub use progress::{ProgressTracker, TransferProgress};
pub use protocol::{
    read_exact_payload, read_frame_header, write_frame, FileMetadata, FrameHeader, MsgType,
    TransferResponse, DEFAULT_CHUNK_SIZE, HEADER_SIZE, MAGIC_BYTE,
};
pub use receiver::FileReceiver;
pub use sender::FileSender;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_frame_header_encode_decode() {
        let original = FrameHeader::new(MsgType::DataChunk, 42, 262144);
        let mut buf = [0u8; HEADER_SIZE];
        original.encode(&mut buf);

        let decoded = FrameHeader::decode(&buf).expect("Decode sollte gelingen");
        assert_eq!(original.msg_type, decoded.msg_type);
        assert_eq!(original.chunk_index, decoded.chunk_index);
        assert_eq!(original.payload_len, decoded.payload_len);
    }

    #[test]
    fn test_metadata_chunk_calculation() {
        let meta = FileMetadata::from_file_info(
            "id-1".into(),
            "test.bin".into(),
            (256 * 1024 * 3) + 10, // 3 volle Chunks + 1 Rest-Chunk = 4
            256 * 1024,
        );
        assert_eq!(meta.total_chunks, 4);
    }

    #[test]
    fn test_in_memory_transfer_simulation() {
        // Simuliere einen 600 KB Test-Datenstrom
        let original_data: Vec<u8> = (0..(600 * 1024)).map(|i| (i % 255) as u8).collect();
        let mut reader = Cursor::new(original_data.clone());

        // Bidirektionaler Speicher-Puffer
        let mut network_pipe = Vec::new();

        let metadata = FileMetadata::from_file_info(
            "test-id".into(),
            "sim.dat".into(),
            original_data.len() as u64,
            256 * 1024,
        );

        // 1. Sender schreibt Metadaten
        let meta_json = serde_json::to_vec(&metadata).unwrap();
        let meta_header = FrameHeader::new(MsgType::FileMeta, 0, meta_json.len() as u32);
        write_frame(&mut network_pipe, &meta_header, &meta_json).unwrap();

        // 2. Simuliere Empfänger-Zusage ab Chunk 0
        let resp = TransferResponse {
            file_id: "test-id".into(),
            accepted: true,
            start_chunk: 0,
            message: None,
        };
        let resp_json = serde_json::to_vec(&resp).unwrap();
        let resp_header = FrameHeader::new(MsgType::Control, 0, resp_json.len() as u32);
        write_frame(&mut network_pipe, &resp_header, &resp_json).unwrap();

        assert!(!network_pipe.is_empty());
    }
}
