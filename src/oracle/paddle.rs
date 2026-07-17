//! PaddleOCR job submission, polling, confidence, and result extraction.

use std::sync::atomic::{AtomicBool, Ordering};

use super::local_route;
use super::paddle_wire::{
    extract_paddle_scores, extract_paddle_text, json_str_field_loose, paddle_http_error,
    paddle_multipart, sleep_cancellable,
};

/// Optional PaddleOCR community-service front end. When configured, the
/// OpenAI-compatible model receives only its text result, never the page PNG.
#[derive(Clone)]
pub(super) struct PaddleOcr {
    job_url: String,
    token: String,
    pub(super) model: String,
    poll_every: std::time::Duration,
    timeout: std::time::Duration,
    pub(super) speculative: bool,
    agent: ureq::Agent,
}

#[derive(Clone, Debug)]
pub(super) struct OcrResult {
    pub(super) text: String,
    pub(super) min_confidence: Option<f32>,
}

impl OcrResult {
    pub(super) fn high_confidence(&self) -> bool {
        self.min_confidence.is_some_and(|score| score >= 0.88)
    }

    pub(super) fn is_fast_commit(&self) -> bool {
        if !self.high_confidence() {
            return false;
        }
        local_route(&self.text).is_some()
            || self
                .text
                .trim_end()
                .ends_with(['。', '！', '？', '.', '!', '?', '＝', '='])
    }
}

impl PaddleOcr {
    pub(super) fn from_env() -> std::io::Result<Option<Self>> {
        let token = match std::env::var("RIDDLE_OCR_TOKEN") {
            Ok(token) if !token.trim().is_empty() => token,
            _ => return Ok(None),
        };
        let provider = std::env::var("RIDDLE_OCR_PROVIDER")
            .unwrap_or_else(|_| "paddle".into())
            .to_ascii_lowercase();
        if provider != "paddle" && provider != "paddleocr" {
            return Err(std::io::Error::other(format!(
                "unsupported RIDDLE_OCR_PROVIDER {provider}"
            )));
        }
        let job_url = std::env::var("RIDDLE_OCR_URL")
            .unwrap_or_else(|_| "https://paddleocr.aistudio-app.com/api/v2/ocr/jobs".into())
            .trim_end_matches('/')
            .to_string();
        let model = std::env::var("RIDDLE_OCR_MODEL").unwrap_or_else(|_| "PP-OCRv6".into());
        let poll_ms = std::env::var("RIDDLE_OCR_POLL_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(250)
            .clamp(250, 5000);
        let timeout_secs = std::env::var("RIDDLE_OCR_TIMEOUT_SECONDS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(60)
            .clamp(10, 115);
        let speculative = matches!(
            std::env::var("RIDDLE_OCR_SPECULATIVE")
                .unwrap_or_else(|_| "on".into())
                .to_ascii_lowercase()
                .as_str(),
            "on" | "true" | "yes" | "1"
        );
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(10))
            .timeout_read(std::time::Duration::from_secs(30))
            .build();
        Ok(Some(Self {
            job_url,
            token,
            model,
            poll_every: std::time::Duration::from_millis(poll_ms),
            timeout: std::time::Duration::from_secs(timeout_secs),
            speculative,
            agent,
        }))
    }

    pub(super) fn recognize(
        &self,
        png: &[u8],
        cancelled: &AtomicBool,
    ) -> Result<OcrResult, String> {
        let boundary = format!(
            "magicpaper-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        );
        let body = paddle_multipart(png, &self.model, &boundary);
        let started = std::time::Instant::now();
        let response = self
            .agent
            .post(&self.job_url)
            .set("Authorization", &format!("bearer {}", self.token))
            .set(
                "Content-Type",
                &format!("multipart/form-data; boundary={boundary}"),
            )
            .send_bytes(&body)
            .map_err(|error| paddle_http_error("submit", error))?
            .into_string()
            .map_err(|error| format!("PaddleOCR submit response: {error}"))?;
        let job_id = json_str_field_loose(&response, "jobId")
            .ok_or_else(|| "PaddleOCR submit response has no jobId".to_string())?;
        eprintln!(
            "riddle: PaddleOCR job accepted +{}ms",
            started.elapsed().as_millis()
        );

        loop {
            if cancelled.load(Ordering::Acquire) {
                return Err("PaddleOCR request cancelled".into());
            }
            if started.elapsed() >= self.timeout {
                return Err(format!(
                    "PaddleOCR timed out after {}s",
                    self.timeout.as_secs()
                ));
            }
            let status_url = format!("{}/{}", self.job_url, job_id);
            let status = self
                .agent
                .get(&status_url)
                .set("Authorization", &format!("bearer {}", self.token))
                .call()
                .map_err(|error| paddle_http_error("poll", error))?
                .into_string()
                .map_err(|error| format!("PaddleOCR status response: {error}"))?;
            match json_str_field_loose(&status, "state").as_deref() {
                Some("pending" | "running") => {
                    sleep_cancellable(self.poll_every, cancelled);
                }
                Some("done") => {
                    let result_url = json_str_field_loose(&status, "jsonUrl")
                        .ok_or_else(|| "PaddleOCR completed without a jsonUrl".to_string())?;
                    let jsonl = self
                        .agent
                        .get(&result_url)
                        .call()
                        .map_err(|error| paddle_http_error("download", error))?
                        .into_string()
                        .map_err(|error| format!("PaddleOCR result response: {error}"))?;
                    let text = extract_paddle_text(&jsonl);
                    if text.trim().is_empty() {
                        return Err("PaddleOCR returned no recognized text".into());
                    }
                    let min_confidence = extract_paddle_scores(&jsonl).into_iter().reduce(f32::min);
                    eprintln!(
                        "riddle: PaddleOCR complete +{}ms ({} chars, min confidence {})",
                        started.elapsed().as_millis(),
                        text.chars().count(),
                        min_confidence
                            .map(|score| format!("{score:.3}"))
                            .unwrap_or_else(|| "unknown".into())
                    );
                    return Ok(OcrResult {
                        text,
                        min_confidence,
                    });
                }
                Some("failed") => {
                    let reason = json_str_field_loose(&status, "errorMsg")
                        .unwrap_or_else(|| "unknown failure".into());
                    return Err(format!("PaddleOCR failed: {reason}"));
                }
                Some(other) => return Err(format!("PaddleOCR unknown job state: {other}")),
                None => return Err("PaddleOCR status response has no state".into()),
            }
        }
    }
}
