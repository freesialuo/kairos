#[cfg(any(target_arch = "wasm32", test))]
mod api;
#[cfg(target_arch = "wasm32")]
mod app;
#[cfg(any(target_arch = "wasm32", test))]
mod b64;
#[cfg(any(target_arch = "wasm32", test))]
mod calendar_grid;
#[cfg(target_arch = "wasm32")]
mod pages;
#[cfg(target_arch = "wasm32")]
mod passkey;
#[cfg(target_arch = "wasm32")]
mod strings;
#[cfg(target_arch = "wasm32")]
fn main() {
    leptos::mount::mount_to_body(app::App);
}
#[cfg(not(target_arch = "wasm32"))]
fn main() {}
