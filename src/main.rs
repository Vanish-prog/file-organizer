mod ui;
use std::path::PathBuf;

fn main() {
    let mut args = std::env::args_os().skip(1);
    let root = match args.next() {
        Some(arg) if arg == "--help" || arg == "-h" => {
            println!(
                "file-organizer [DIRECTORY]\n\nNative photo organizer. Choose a directory, review duplicates, and organize\nroot-level files into named categories. Local indexing and model downloads\nrequire separate permission. Duplicate removal uses the system trash."
            );
            return;
        }
        Some(arg) if arg == "--version" => {
            println!("file-organizer {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Some(arg) => Some(PathBuf::from(arg)),
        None => None,
    };
    if args.next().is_some() {
        eprintln!("Usage: file-organizer [DIRECTORY]");
        std::process::exit(2);
    }
    ui::launch(root);
}
