//! OpenAI adapter (voice transcription).
//!
//! Uses the OpenAI `audio/transcriptions` endpoint (parity with TS voice handler).

use std::path::Path;

use ctb_core::{errors::Error, Result};

#[derive(Clone, Debug)]
pub struct OpenAiClient {
    pub api_key: String,
    http: reqwest::Client,
}

impl OpenAiClient {
    pub fn new(api_key: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("reqwest client build");
        Self {
            api_key: api_key.into(),
            http,
        }
    }

    pub async fn transcribe_file(&self, path: &Path, prompt: Option<&str>) -> Result<String> {
        let bytes = tokio::fs::read(path).await.map_err(Error::Io)?;

        let file_name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("audio.ogg")
            .to_string();

        let mut form = reqwest::multipart::Form::new()
            .text("model", "gpt-4o-transcribe")
            .part(
                "file",
                reqwest::multipart::Part::bytes(bytes)
                    .file_name(file_name)
                    .mime_str("audio/ogg")
                    .map_err(|e| Error::External(format!("openai multipart error: {e}")))?,
            );

        if let Some(p) = prompt {
            if !p.trim().is_empty() {
                form = form.text("prompt", p.to_string());
            }
        }

        let resp = self
            .http
            .post("https://api.openai.com/v1/audio/transcriptions")
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send()
            .await
            .map_err(|e| Error::External(format!("openai request error: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::External(format!(
                "openai transcription failed: {status} {}",
                body.chars().take(200).collect::<String>()
            )));
        }

        let v: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| Error::External(format!("openai json error: {e}")))?;

        let text = v
            .get("text")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();

        if text.trim().is_empty() {
            return Err(Error::External(
                "openai transcription returned empty text".to_string(),
            ));
        }

        Ok(text)
    }
}
