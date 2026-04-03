use notify::{RecommendedWatcher, RecursiveMode, Result, Watcher, Event};
use std::sync::mpsc::channel;
use std::path::PathBuf;

fn main() -> Result<()> {
    env_logger::init();
    println!("organon-core watcher PoC starting");

    let (tx, rx) = channel();
    // RecommendedWatcher uses the best available watcher for platform
    let mut watcher: RecommendedWatcher = Watcher::new(tx, std::time::Duration::from_secs(2))?;

    let path = std::env::args().nth(1).unwrap_or(".".to_string());
    println!("watching: {}", path);
    watcher.watch(PathBuf::from(&path), RecursiveMode::Recursive)?;

    loop {
        match rx.recv() {
            Ok(event) => {
                println!("event: {:?}", event);
            }
            Err(e) => println!("watch error: {:?}", e),
        }
    }
}
