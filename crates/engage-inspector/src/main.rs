#[cfg(any(target_arch = "wasm32", test))]
mod content;

#[cfg(target_arch = "wasm32")]
mod api;
#[cfg(target_arch = "wasm32")]
mod app;
#[cfg(target_arch = "wasm32")]
mod composer;
#[cfg(target_arch = "wasm32")]
mod settings;
#[cfg(target_arch = "wasm32")]
mod state;
#[cfg(target_arch = "wasm32")]
mod thread;
#[cfg(target_arch = "wasm32")]
mod ui;

#[cfg(target_arch = "wasm32")]
fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(app::App);
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    eprintln!("Build the inspector for the browser with `trunk build --release`");
}
