use std::{
	collections::{hash_map, HashMap},
	sync::{
		atomic::{self, AtomicUsize},
		Arc, Mutex,
	},
};

use bytes::Bytes;
use serde::de::DeserializeOwned;
use tauri::{plugin::PluginApi, AppHandle, Runtime};

use crate::rpc::*;

pub fn init<R: Runtime, C: DeserializeOwned>(
	_app: &AppHandle<R>,
	_api: PluginApi<R, C>,
) -> crate::Result<WebTransport> {
	Ok(WebTransport {
		sessions: Default::default(),
		next_session: Default::default(),
	})
}

/// Access to the web-transport APIs.
pub struct WebTransport {
	sessions: Arc<Mutex<HashMap<usize, Session>>>,
	next_session: AtomicUsize,
}

impl WebTransport {
	pub async fn connect(&self, payload: ConnectRequest) -> crate::Result<ConnectResponse> {
		let builder = web_transport::ClientBuilder::new();

		let builder = builder.with_congestion_control(match payload.congestion_control {
			Some(CongestionControl::LowLatency) => web_transport::CongestionControl::LowLatency,
			Some(CongestionControl::Throughput) => web_transport::CongestionControl::Throughput,
			_ => web_transport::CongestionControl::Default,
		});

		let client = match payload.server_certificate_hashes {
			Some(hashes) => {
				let hashes = hashes.iter().map(hex::decode).collect::<Result<Vec<_>, _>>()?;
				builder.with_server_certificate_hashes(hashes)
			}
			None => builder.with_system_roots(),
		}?;

		let transport = client.connect(payload.url).await?;
		let session = self.next_session.fetch_add(1, atomic::Ordering::Relaxed);

		match self.sessions.lock().unwrap().entry(session) {
			hash_map::Entry::Vacant(e) => e.insert(Session::new(transport)),
			hash_map::Entry::Occupied(_) => return Err(crate::Error::DuplicateSession),
		};

		Ok(ConnectResponse { session })
	}

	fn get(&self, id: usize) -> crate::Result<Session> {
		self.sessions
			.lock()
			.unwrap()
			.get(&id)
			.cloned()
			.ok_or(crate::Error::NoSession)
	}

	pub fn close(&self, payload: CloseRequest) -> crate::Result<CloseResponse> {
		let mut session = self
			.sessions
			.lock()
			.unwrap()
			.remove(&payload.session)
			.ok_or(crate::Error::NoSession)?;
		session
			.inner
			.close(payload.code, payload.reason.as_deref().unwrap_or(""));

		Ok(CloseResponse {})
	}

	pub async fn closed(&self, payload: ClosedRequest) -> crate::Result<ClosedResponse> {
		let session = self.get(payload.session)?;
		session.inner.closed().await;

		Ok(ClosedResponse {})
	}

	pub async fn accept(&self, payload: AcceptRequest) -> crate::Result<AcceptResponse> {
		let mut session = self.get(payload.session)?;
		let stream = if payload.bidirectional {
			session.accept_bi().await?
		} else {
			session.accept_uni().await?
		};
		Ok(AcceptResponse { stream })
	}

	pub async fn open(&self, payload: OpenRequest) -> crate::Result<OpenResponse> {
		let mut session = self.get(payload.session)?;
		let stream = if payload.bidirectional {
			session.create_bi(payload.send_order).await?
		} else {
			session.create_uni(payload.send_order).await?
		};
		Ok(OpenResponse { stream })
	}

	pub async fn read(&self, payload: ReadRequest) -> crate::Result<ReadResponse> {
		let mut session = self.get(payload.session)?;
		let data = session.recv(payload.stream).await?;
		Ok(ReadResponse { data })
	}

	pub async fn write(&self, payload: WriteRequest) -> crate::Result<WriteResponse> {
		let mut session = self.get(payload.session)?;
		session.send(payload.stream, payload.data).await?;
		Ok(WriteResponse {})
	}

	pub fn reset(&self, payload: ResetRequest) -> crate::Result<ResetResponse> {
		let mut session = self.get(payload.session)?;
		// TODO support reset in the middle of a read.
		session.reset(payload.stream, payload.code)?;
		Ok(ResetResponse {})
	}
}

#[derive(Clone)]
pub struct Session {
	pub inner: web_transport::Session,
	send: Arc<Mutex<HashMap<usize, web_transport::SendStream>>>,
	recv: Arc<Mutex<HashMap<usize, web_transport::RecvStream>>>,
	streams: Arc<AtomicUsize>,
}

impl Session {
	pub fn new(inner: web_transport::Session) -> Self {
		Self {
			inner,
			send: Default::default(),
			recv: Default::default(),
			streams: Default::default(),
		}
	}

	pub async fn accept_bi(&mut self) -> crate::Result<usize> {
		let (send, recv) = self.inner.accept_bi().await?;
		let id = self.streams.fetch_add(1, atomic::Ordering::Relaxed);
		self.send.lock().unwrap().insert(id, send);
		self.recv.lock().unwrap().insert(id, recv);
		Ok(id)
	}

	pub async fn accept_uni(&mut self) -> crate::Result<usize> {
		let recv = self.inner.accept_uni().await?;
		let id = self.streams.fetch_add(1, atomic::Ordering::Relaxed);
		self.recv.lock().unwrap().insert(id, recv);
		Ok(id)
	}

	pub async fn create_bi(&mut self, send_order: Option<i32>) -> crate::Result<usize> {
		let (mut send, recv) = self.inner.open_bi().await?;
		if let Some(send_order) = send_order {
			send.set_priority(send_order);
		}
		let id = self.streams.fetch_add(1, atomic::Ordering::Relaxed);
		self.send.lock().unwrap().insert(id, send);
		self.recv.lock().unwrap().insert(id, recv);
		Ok(id)
	}

	pub async fn create_uni(&mut self, send_order: Option<i32>) -> crate::Result<usize> {
		let mut send = self.inner.open_uni().await?;
		if let Some(send_order) = send_order {
			send.set_priority(send_order);
		}
		let id = self.streams.fetch_add(1, atomic::Ordering::Relaxed);
		self.send.lock().unwrap().insert(id, send);
		Ok(id)
	}

	pub async fn recv(&mut self, id: usize) -> crate::Result<Option<Bytes>> {
		let mut recv = self.recv.lock().unwrap().remove(&id).ok_or(crate::Error::NoStream)?;

		let buf = recv.read(usize::MAX).await?;
		if buf.is_some() {
			self.recv.lock().unwrap().insert(id, recv);
		}

		Ok(buf)
	}

	pub async fn send(&mut self, id: usize, mut data: Bytes) -> crate::Result<()> {
		let mut send = self.send.lock().unwrap().remove(&id).ok_or(crate::Error::NoStream)?;
		if data.is_empty() {
			send.finish()?;
			return Ok(());
		}

		while !data.is_empty() {
			send.write_buf(&mut data).await?;
		}

		self.send.lock().unwrap().insert(id, send);
		Ok(())
	}

	pub fn reset(&mut self, id: usize, code: u32) -> crate::Result<()> {
		if let Some(mut send) = self.send.lock().unwrap().remove(&id) {
			send.reset(code);
		}

		if let Some(mut recv) = self.recv.lock().unwrap().remove(&id) {
			recv.stop(code);
		}

		Ok(())
	}
}
