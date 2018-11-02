mod sync;

use failure::Error;

fn main() {
    env_logger::init();
    if let Err(e) = run() {
        eprintln!("error: {}", e);
        for e in e.iter_causes() {
            eprintln!("  cause: {}", e);
        }
        std::process::exit(1);
    }
}

fn run() -> Result<(), Error> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 1 {
        usage();
    }

    match args[0].as_str() {
        "sync" => {
            sync::lists::run()?;
        }
        _ => usage(),
    }

    Ok(())
}

fn usage() {
    eprintln!("usage: {} <mode>", std::env::args().next().unwrap());
    eprintln!("available modes:");
    eprintln!("- sync: synchronize local state with the remote providers");
    std::process::exit(1);
}
