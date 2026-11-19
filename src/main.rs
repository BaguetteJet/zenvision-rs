mod animation;
mod app;
mod canvas;
mod cli;
mod device;
mod event;
mod ipc;
mod panel;
mod protocol;
mod render;
mod state;

fn main() {
    if let Err(error) = cli::run() {
        eprintln!("zenvision: {error}");
        std::process::exit(1);
    }
}
