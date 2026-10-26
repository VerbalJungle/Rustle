use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// Struktur für Fortschritts-Updates an das Flutter-Frontend
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferProgress {
    pub file_id: String,
    pub bytes_transferred: u64,
    pub total_bytes: u64,
    pub current_chunk: u32,
    pub total_chunks: u32,
    pub progress_percent: f32,
    pub speed_bytes_per_sec: u64,
    pub eta_seconds: u64,
    pub is_finished: bool,
}

pub struct ProgressTracker {
    pub file_id: String,
    pub total_bytes: u64,
    pub total_chunks: u32,
    pub bytes_transferred: u64,
    pub current_chunk: u32,
    start_time: Instant,
    last_update_time: Instant,
    last_update_bytes: u64,
    current_speed: u64,
    update_interval: Duration,
}

impl ProgressTracker {
    pub fn new(file_id: String, total_bytes: u64, total_chunks: u32) -> Self {
        let now = Instant::now();
        Self {
            file_id,
            total_bytes,
            total_chunks,
            bytes_transferred: 0,
            current_chunk: 0,
            start_time: now,
            last_update_time: now,
            last_update_bytes: 0,
            current_speed: 0,
            update_interval: Duration::from_millis(100), // Maximal 10 Updates pro Sekunde an die UI
        }
    }

    /// Wird nach jedem gesendeten/empfangenen Chunk aufgerufen.
    /// Gibt `Some(TransferProgress)` zurück, wenn das Drosselungsintervall erreicht ist oder der Transfer fertig ist.
    pub fn update(&mut self, chunk_idx: u32, bytes_added: usize) -> Option<TransferProgress> {
        self.bytes_transferred += bytes_added as u64;
        self.current_chunk = chunk_idx + 1;

        let now = Instant::now();
        let elapsed_since_update = now.duration_since(self.last_update_time);
        let is_finished = self.bytes_transferred >= self.total_bytes;

        if elapsed_since_update >= self.update_interval || is_finished {
            let bytes_delta = self.bytes_transferred.saturating_sub(self.last_update_bytes);
            let secs = elapsed_since_update.as_secs_f64();
            if secs > 0.001 {
                let instant_speed = (bytes_delta as f64 / secs) as u64;
                // Exponentieller Glättungsfilter (EMA) für stabilere Geschwindigkeitsanzeige
                self.current_speed = if self.current_speed == 0 {
                    instant_speed
                } else {
                    ((self.current_speed as f64 * 0.7) + (instant_speed as f64 * 0.3)) as u64
                };
            }

            self.last_update_time = now;
            self.last_update_bytes = self.bytes_transferred;

            let remaining_bytes = self.total_bytes.saturating_sub(self.bytes_transferred);
            let eta = if self.current_speed > 0 {
                remaining_bytes / self.current_speed
            } else {
                0
            };

            let percent = if self.total_bytes > 0 {
                ((self.bytes_transferred as f64 / self.total_bytes as f64) * 100.0) as f32
            } else {
                100.0
            };

            Some(TransferProgress {
                file_id: self.file_id.clone(),
                bytes_transferred: self.bytes_transferred,
                total_bytes: self.total_bytes,
                current_chunk: self.current_chunk,
                total_chunks: self.total_chunks,
                progress_percent: percent.clamp(0.0, 100.0),
                speed_bytes_per_sec: self.current_speed,
                eta_seconds: eta,
                is_finished,
            })
        } else {
            None
        }
    }
}
