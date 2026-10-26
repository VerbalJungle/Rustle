use super::progress::{ProgressTracker, TransferProgress};
use super::protocol::{
    read_exact_payload, read_frame_header, write_frame, FileMetadata, FrameHeader, MsgType,
    TransferResponse,
};
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub struct FileReceiver;

impl FileReceiver {
    /// Empfängt eine Datei über einen TCP-Stream und speichert sie in `save_directory`.
    pub fn receive_file<S, P, F>(
        stream: &mut S,
        save_directory: P,
        mut on_progress: F,
    ) -> Result<PathBuf, String>
    where
        S: Read + Write,
        P: AsRef<Path>,
        F: FnMut(&TransferProgress),
    {
        let save_dir = save_directory.as_ref();
        if !save_dir.exists() {
            fs::create_dir_all(save_dir)
                .map_err(|e| format!("Konnte Zielverzeichnis nicht erstellen: {}", e))?;
        }

        // 1. Handshake: Metadaten lesen
        let meta_header = read_frame_header(stream)
            .map_err(|e| format!("Fehler beim Lesen des Meta-Headers: {}", e))?;

        if meta_header.msg_type != MsgType::FileMeta {
            return Err(format!(
                "Erster Frame war nicht FileMeta, sondern {:?}",
                meta_header.msg_type
            ));
        }

        let meta_payload = read_exact_payload(stream, meta_header.payload_len as usize)
            .map_err(|e| format!("Fehler beim Lesen der Metadaten: {}", e))?;

        let metadata: FileMetadata = serde_json::from_slice(&meta_payload)
            .map_err(|e| format!("Konnte Metadaten nicht parsen: {}", e))?;

        let final_path = save_dir.join(&metadata.file_name);
        let part_path = save_dir.join(format!("{}.part", metadata.file_name));

        // 2. Resume-Prüfung: Prüfe, ob bereits eine unfertige .part Datei vorliegt
        let mut start_chunk = 0u32;
        let mut start_offset = 0u64;

        if part_path.exists() {
            if let Ok(part_meta) = part_path.metadata() {
                let existing_len = part_meta.len();
                if existing_len > 0 && existing_len < metadata.file_size {
                    // Auf ganze Chunks abrunden
                    start_chunk = (existing_len / metadata.chunk_size as u64) as u32;
                    start_offset = start_chunk as u64 * metadata.chunk_size as u64;
                    println!(
                        "[RECEIVER] Resume aktiv: Setze bei Chunk {} (Offset {}) fort",
                        start_chunk, start_offset
                    );
                }
            }
        }

        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .open(&part_path)
            .map_err(|e| format!("Konnte Part-Datei '{}' nicht anlegen: {}", part_path.display(), e))?;

        if start_offset > 0 {
            file.seek(SeekFrom::Start(start_offset))
                .map_err(|e| format!("Fehler beim Seek in Part-Datei: {}", e))?;
        }

        // 3. Antwort an Sender schicken (Zusage & ggf. Start-Chunk)
        let response = TransferResponse {
            file_id: metadata.file_id.clone(),
            accepted: true,
            start_chunk,
            message: None,
        };

        let resp_json = serde_json::to_vec(&response)
            .map_err(|e| format!("Fehler beim Serialisieren der TransferResponse: {}", e))?;

        let resp_header = FrameHeader::new(MsgType::Control, 0, resp_json.len() as u32);
        write_frame(stream, &resp_header, &resp_json)
            .map_err(|e| format!("Fehler beim Senden der TransferResponse: {}", e))?;

        // 4. Chunks empfangen
        let mut tracker = ProgressTracker::new(
            metadata.file_id.clone(),
            metadata.file_size,
            metadata.total_chunks,
        );

        // Falls wir bei Resume mitten drin starten, Tracker anpassen
        if start_offset > 0 {
            tracker.bytes_transferred = start_offset;
            tracker.current_chunk = start_chunk;
        }

        let mut expected_chunk = start_chunk;

        while expected_chunk < metadata.total_chunks {
            let chunk_header = read_frame_header(stream)
                .map_err(|e| format!("Fehler beim Lesen von Chunk-Header {}: {}", expected_chunk, e))?;

            if chunk_header.msg_type != MsgType::DataChunk {
                return Err(format!(
                    "Erwartete DataChunk {}, erhielt aber {:?}",
                    expected_chunk, chunk_header.msg_type
                ));
            }

            let payload = read_exact_payload(stream, chunk_header.payload_len as usize)
                .map_err(|e| format!("Fehler beim Lesen von Chunk-Payload {}: {}", expected_chunk, e))?;

            file.write_all(&payload)
                .map_err(|e| format!("Fehler beim Schreiben von Chunk {}: {}", expected_chunk, e))?;

            if let Some(progress) = tracker.update(expected_chunk, payload.len()) {
                on_progress(&progress);
            }

            expected_chunk += 1;
        }

        file.flush()
            .map_err(|e| format!("Fehler beim Flushen der Datei: {}", e))?;
        drop(file);

        // 5. Part-Datei in finalen Namen umbenennen
        if final_path.exists() {
            let _ = fs::remove_file(&final_path);
        }
        fs::rename(&part_path, &final_path)
            .map_err(|e| format!("Konnte Part-Datei nicht final umbenennen: {}", e))?;

        // 6. TransferAck an Sender schicken
        let ack_header = FrameHeader::new(MsgType::TransferAck, 0, 0);
        write_frame(stream, &ack_header, &[])
            .map_err(|e| format!("Fehler beim Senden des finalen TransferAck: {}", e))?;

        Ok(final_path)
    }
}
