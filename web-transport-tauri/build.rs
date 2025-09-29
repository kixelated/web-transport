const COMMANDS: &[&str] = &["connect", "close", "closed", "accept", "open", "read", "write", "reset"];

fn main() {
	tauri_plugin::Builder::new(COMMANDS)
		.android_path("android")
		.ios_path("ios")
		.build();
}
