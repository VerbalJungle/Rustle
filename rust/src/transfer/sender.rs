use super::progress::{ProgressTracker, TransferProgress};
use super::protocol::{
    read_exact_payload, read_frame_header, write_frame, write_frame_and_flush, FileMetadata,
    FrameHeader, MsgType, TransferResponse, DEFAULT_CHUNK_SIZE,
};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

pub struct FileSender;

impl FileSender {
    /// Sendet eine Datei über einen beliebigen Read+Write-Stream (z. B. TCP-Stream)
    pub fn send_file<S, P, F>(
        stream: &mut S,
        file_path: P,
        file_id: String,
        mut on_progress: F,
    ) -> Result<u64, String>
    where
        S: Read + Write,
        P: AsRef<Path>,
        F: FnMut(&TransferProgress),
    {
        let path = file_path.as_ref();
        let mut file = File::open(path)
            .map_err(|e| format!("Konnte Datei '{}' nicht öffnen: {}", path.display(), e))?;

        let file_size = file
            .metadata()
            .map_err(|e| format!("Konnte Dateigröße nicht lesen: {}", e))?
            .len();

        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("datei.bin")
            .to_string();

        let metadata = FileMetadata::from_file_info(file_id, file_name, file_size, DEFAULT_CHUNK_SIZE);

        Self::send_stream(stream, &mut file, metadata, &mut on_progress)
    }

    /// Universeller Sender für beliebige Streams & Reader (ermöglicht 100% Mocking & Tests ohne Dateisystem)
    pub fn send_stream<S, R, F>(
        stream: &mut S,
        reader: &mut R,
        metadata: FileMetadata,
        on_progress: &mut F,
    ) -> Result<u64, String>
    where
        S: Read + Write,
        R: Read + Seek,
        F: FnMut(&TransferProgress),
    {
        // 1. Handshake: Metadaten als JSON im ersten Frame senden und direkt flushen
        let meta_json = serde_json::to_vec(&metadata)
            .map_err(|e| format!("Fehler beim Serialisieren der Metadaten: {}", e))?;

        let meta_header = FrameHeader::new(MsgType::FileMeta, 0, meta_json.len() as u32);
        write_frame_and_flush(stream, &meta_header, &meta_json)
            .map_err(|e| format!("Fehler beim Senden der Metadaten: {}", e))?;

        // 2. Antwort des Empfängers abwarten (TransferResponse)
        let resp_header = read_frame_header(stream)
            .map_err(|e| format!("Fehler beim Lesen der Empfänger-Antwort: {}", e))?;

        let resp_payload = read_exact_payload(stream, resp_header.payload_len as usize)
            .map_err(|e| format!("Fehler beim Lesen der Empfänger-Payload: {}", e))?;

        let response: TransferResponse = serde_json::from_slice(&resp_payload)
            .map_err(|e| format!("Ungültige Empfänger-Antwort: {}", e))?;

        if !response.accepted {
            let reason = response.message.unwrap_or_else(|| "Vom Empfänger abgelehnt".into());
            return Err(format!("Transfer wurde abgelehnt: {}", reason));
        }

        // 3. Optionaler Resume-Support: An Start-Chunk springen
        let start_chunk = response.start_chunk;
        if start_chunk > 0 {
            let offset = start_chunk as u64 * metadata.chunk_size as u64;
            reader
                .seek(SeekFrom::Start(offset))
                .map_err(|e| format!("Fehler beim Seek auf Offset {}: {}", offset, e))?;
        }

        // 4. Streaming der Chunks mit 2 MB I/O BufWriter für maximale Pipeline-Auslastung
        let mut tracker = ProgressTracker::new(
            metadata.file_id.clone(),
            metadata.file_size,
            metadata.total_chunks,
        );

        let mut buffer = vec![0u8; metadata.chunk_size as usize];
        let mut current_chunk = start_chunk;
        let mut buffered_reader = BufReader::with_capacity(1024 * 1024, reader);
        let mut buffered_writer = BufWriter::with_capacity(2 * 1024 * 1024, stream);

        while current_chunk < metadata.total_chunks {
            let bytes_read = buffered_reader
                .read(&mut buffer)
                .map_err(|e| format!("Fehler beim Lesen von Chunk {}: {}", current_chunk, e))?;

            if bytes_read == 0 {
                break; // EOF erreicht
            }

            let chunk_header =
                FrameHeader::new(MsgType::DataChunk, current_chunk, bytes_read as u32);

            write_frame(&mut buffered_writer, &chunk_header, &buffer[..bytes_read])
                .map_err(|e| format!("Fehler beim Senden von Chunk {}: {}", current_chunk, e))?;

            if let Some(progress) = tracker.update(current_chunk, bytes_read) {
                on_progress(&progress);
            }

            current_chunk += 1;
        }

        buffered_writer
            .flush()
            .map_err(|e| format!("Fehler beim abschließenden Flushen des Streams: {}", e))?;

        // 5. Finales Warten auf TransferAck vom Empfänger (über den darunterliegenden Stream)
        let underlying_stream = buffered_writer.into_inner().map_err(|e| e.to_string())?;
        let ack_header = read_frame_header(underlying_stream)
            .map_err(|e| format!("Fehler beim Warten auf Abschlussbestätigung: {}", e))?;

        if ack_header.msg_type != MsgType::TransferAck {
            return Err(format!(
                "Unerwarteter Abschluss-Frame: {:?}",
                ack_header.msg_type
            ));
        }

        Ok(tracker.bytes_transferred)
    }
}
