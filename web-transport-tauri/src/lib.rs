use tauri::{
	plugin::{Builder, TauriPlugin},
	Manager, Runtime,
};

pub use rpc::*;

#[cfg(desktop)]
mod desktop;
#[cfg(mobile)]
mod mobile;

mod commands;
mod error;
mod rpc;

pub use error::{Error, Result};

#[cfg(desktop)]
use desktop::WebTransport;
#[cfg(mobile)]
use mobile::WebTransport;

/// Extensions to [`tauri::App`], [`tauri::AppHandle`] and [`tauri::Window`] to access the web-transport APIs.
pub trait WebTransportExt<R: Runtime> {
	fn web_transport(&self) -> &WebTransport;
}

impl<R: Runtime, T: Manager<R>> crate::WebTransportExt<R> for T {
	fn web_transport(&self) -> &WebTransport {
		self.state::<WebTransport>().inner()
	}
}

/// Initializes the plugin.
pub fn init<R: Runtime>() -> TauriPlugin<R> {
	Builder::new("web-transport")
		.invoke_handler(tauri::generate_handler![
			commands::connect,
			commands::close,
			commands::closed,
			commands::accept,
			commands::open,
			commands::read,
			commands::write,
			commands::reset,
		])
		.setup(|app, api| {
			#[cfg(mobile)]
			let web_transport = mobile::init(app, api)?;
			#[cfg(desktop)]
			let web_transport = desktop::init(app, api)?;
			app.manage(web_transport);
			Ok(())
		})
		.build()
}
