use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tauri::Url;

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")] // matches the WebTransport API
pub enum CongestionControl {
	Default,
	LowLatency,
	Throughput,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectRequest {
	pub url: Url,
	pub congestion_control: Option<CongestionControl>,
	// Hex-encoded sha256 hashes.
	pub server_certificate_hashes: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectResponse {
	pub session: usize,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloseRequest {
	pub session: usize,
	pub code: u32,
	pub reason: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloseResponse {}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClosedRequest {
	pub session: usize,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClosedResponse {
	// TODO error code
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceptRequest {
	pub session: usize,
	pub bidirectional: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceptResponse {
	pub stream: usize,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenRequest {
	pub session: usize,
	pub bidirectional: bool,
	pub send_order: Option<i32>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenResponse {
	pub stream: usize,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadRequest {
	pub session: usize,
	pub stream: usize,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadResponse {
	// If None, then the stream is closed.
	pub data: Option<Bytes>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteRequest {
	pub session: usize,
	pub stream: usize,
	pub data: Bytes,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteResponse {}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FinishRequest {
	pub session: usize,
	pub stream: usize,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FinishResponse {}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResetRequest {
	pub session: usize,
	pub stream: usize,
	pub code: u32,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResetResponse {}
