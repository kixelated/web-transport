use tauri::{command, AppHandle, Runtime};

use crate::rpc::*;
use crate::Result;
use crate::WebTransportExt;

#[command]
pub(crate) async fn connect<R: Runtime>(app: AppHandle<R>, payload: ConnectRequest) -> Result<ConnectResponse> {
	app.web_transport().connect(payload).await
}

#[command]
pub(crate) fn close<R: Runtime>(app: AppHandle<R>, payload: CloseRequest) -> Result<CloseResponse> {
	app.web_transport().close(payload)
}

#[command]
pub(crate) async fn closed<R: Runtime>(app: AppHandle<R>, payload: ClosedRequest) -> Result<ClosedResponse> {
	app.web_transport().closed(payload).await
}

#[command]
pub(crate) async fn accept<R: Runtime>(app: AppHandle<R>, payload: AcceptRequest) -> Result<AcceptResponse> {
	app.web_transport().accept(payload).await
}

#[command]
pub(crate) async fn open<R: Runtime>(app: AppHandle<R>, payload: OpenRequest) -> Result<OpenResponse> {
	app.web_transport().open(payload).await
}

#[command]
pub(crate) async fn read<R: Runtime>(app: AppHandle<R>, payload: ReadRequest) -> Result<ReadResponse> {
	app.web_transport().read(payload).await
}

#[command]
pub(crate) async fn write<R: Runtime>(app: AppHandle<R>, payload: WriteRequest) -> Result<WriteResponse> {
	app.web_transport().write(payload).await
}

#[command]
pub(crate) async fn reset<R: Runtime>(app: AppHandle<R>, payload: ResetRequest) -> Result<ResetResponse> {
	app.web_transport().reset(payload)
}
