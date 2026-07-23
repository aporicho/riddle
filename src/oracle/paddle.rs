//! PaddleOCR job submission, polling, confidence, and result extraction.

use std::sync::atomic::{AtomicBool, Ordering};

use super::local_route;
use super::paddle_wire::{
    extract_paddle_scores, extract_paddle_text, json_str_field_loose, paddle_http_error,
    paddle_multipart, read_response_limited, sleep_cancellable,
};

const MAX_CONTROL_RESPONSE_BYTES: usize = 256 * 1024;
const MAX_RESULT_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_RECOGNIZED_TEXT_BYTES: usize = 64 * 1024;

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
        crate::runtime_env::require_external_integrations("PaddleOCR")?;
        let token = match std::env::var("MAGICPAPER_OCR_TOKEN") {
            Ok(token) if !token.trim().is_empty() => token,
            _ => return Ok(None),
        };
        let provider = std::env::var("MAGICPAPER_OCR_PROVIDER")
            .unwrap_or_else(|_| "paddle".into())
            .to_ascii_lowercase();
        if provider != "paddle" && provider != "paddleocr" {
            return Err(std::io::Error::other(format!(
                "unsupported MAGICPAPER_OCR_PROVIDER {provider}"
            )));
        }
        let job_url = std::env::var("MAGICPAPER_OCR_URL")
            .unwrap_or_else(|_| "https://paddleocr.aistudio-app.com/api/v2/ocr/jobs".into())
            .trim_end_matches('/')
            .to_string();
        if !valid_http_url(&job_url) {
            return Err(std::io::Error::other(
                "MAGICPAPER_OCR_URL must be a valid http(s) URL",
            ));
        }
        let model = std::env::var("MAGICPAPER_OCR_MODEL").unwrap_or_else(|_| "PP-OCRv6".into());
        let poll_ms = std::env::var("MAGICPAPER_OCR_POLL_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(250)
            .clamp(250, 5000);
        let timeout_secs = std::env::var("MAGICPAPER_OCR_TIMEOUT_SECONDS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(60)
            .clamp(10, 115);
        let speculative = matches!(
            std::env::var("MAGICPAPER_OCR_SPECULATIVE")
                .unwrap_or_else(|_| "on".into())
                .to_ascii_lowercase()
                .as_str(),
            "on" | "true" | "yes" | "1"
        );
        let agent = ureq::AgentBuilder::new()
            // Honor a manager-controlled HTTPS_PROXY when the tablet is on a
            // restricted network (and in USB-tethered acceptance tests).
            // CONNECT keeps the OCR token and image inside end-to-end TLS.
            .try_proxy_from_env(true)
            .timeout_connect(std::time::Duration::from_secs(10))
            .timeout_read(std::time::Duration::from_secs(30))
            .timeout_write(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(timeout_secs + 5))
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
        request_id: u64,
        domain: &str,
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
        let started = std::time::Instant::now();
        eprintln!(
            "magic-paper: event=ocr-submit request={}:{} domain={domain} provider=paddle model={} bytes={}",
            std::process::id(),
            request_id,
            self.model,
            png.len()
        );
        let job_id = self.submit_job(png, &boundary)?;
        eprintln!(
            "magic-paper: event=ocr-accepted request={}:{} domain={domain} latency_ms={}",
            std::process::id(),
            request_id,
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
            let response = self
                .agent
                .get(&status_url)
                .set("Authorization", &format!("bearer {}", self.token))
                .call()
                .map_err(|error| paddle_http_error("poll", error))?;
            let status = read_response_limited(response, "status", MAX_CONTROL_RESPONSE_BYTES)?;
            match json_str_field_loose(&status, "state").as_deref() {
                Some("pending" | "running") => {
                    sleep_cancellable(self.poll_every, cancelled);
                }
                Some("done") => {
                    return self.finish_job(&status, request_id, domain, started);
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

    fn submit_job(&self, png: &[u8], boundary: &str) -> Result<String, String> {
        let body = paddle_multipart(png, &self.model, boundary);
        let response = self
            .agent
            .post(&self.job_url)
            .set("Authorization", &format!("bearer {}", self.token))
            .set(
                "Content-Type",
                &format!("multipart/form-data; boundary={boundary}"),
            )
            .send_bytes(&body)
            .map_err(|error| paddle_http_error("submit", error))?;
        let response = read_response_limited(response, "submit", MAX_CONTROL_RESPONSE_BYTES)?;
        let job_id = json_str_field_loose(&response, "jobId")
            .ok_or_else(|| "PaddleOCR submit response has no jobId".to_string())?;
        valid_job_id(&job_id)
            .then_some(job_id)
            .ok_or_else(|| "PaddleOCR submit response has an invalid jobId".to_string())
    }

    fn finish_job(
        &self,
        status: &str,
        request_id: u64,
        domain: &str,
        started: std::time::Instant,
    ) -> Result<OcrResult, String> {
        let result_url = json_str_field_loose(status, "jsonUrl")
            .ok_or_else(|| "PaddleOCR completed without a jsonUrl".to_string())?;
        if !valid_http_url(&result_url) {
            return Err("PaddleOCR completed with an invalid jsonUrl".into());
        }
        let response = self
            .agent
            .get(&result_url)
            .call()
            .map_err(|error| paddle_http_error("download", error))?;
        let jsonl = read_response_limited(response, "result", MAX_RESULT_RESPONSE_BYTES)?;
        let text = extract_paddle_text(&jsonl);
        if text.trim().is_empty() {
            return Err("PaddleOCR returned no recognized text".into());
        }
        if text.len() > MAX_RECOGNIZED_TEXT_BYTES {
            return Err("PaddleOCR recognized text exceeds the single-page limit".into());
        }
        let min_confidence = extract_paddle_scores(&jsonl).into_iter().reduce(f32::min);
        eprintln!(
            "magic-paper: event=ocr-done request={}:{} domain={domain} latency_ms={} chars={} min_confidence={}",
            std::process::id(),
            request_id,
            started.elapsed().as_millis(),
            text.chars().count(),
            min_confidence
                .map(|score| format!("{score:.3}"))
                .unwrap_or_else(|| "unknown".into())
        );
        Ok(OcrResult {
            text,
            min_confidence,
        })
    }
}

fn valid_job_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn valid_http_url(value: &str) -> bool {
    if value.len() > 2048
        || value
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
    {
        return false;
    }
    let authority = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))
        .and_then(|rest| rest.split('/').next());
    authority.is_some_and(|authority| !authority.is_empty() && !authority.contains('@'))
}

#[cfg(test)]
mod tests {
    use super::{valid_http_url, valid_job_id};

    #[test]
    fn remote_job_identifiers_cannot_escape_the_status_endpoint() {
        assert!(valid_job_id("job_123-ABC"));
        assert!(!valid_job_id("../status"));
        assert!(!valid_job_id("job?token=secret"));
        assert!(!valid_job_id(""));
    }

    #[test]
    fn configured_and_returned_urls_are_bounded_http_without_credentials() {
        assert!(valid_http_url("https://paddle.example/api/jobs"));
        assert!(valid_http_url("http://127.0.0.1:8080/jobs"));
        assert!(!valid_http_url("file:///etc/shadow"));
        assert!(!valid_http_url("https://user:secret@example.test/result"));
        assert!(!valid_http_url("https://example.test/has space"));
    }
}
