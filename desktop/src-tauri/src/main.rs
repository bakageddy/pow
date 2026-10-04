#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn cli_action(args: &[String]) -> Result<bool, String> {
    match args {
        [] => Ok(true),
        [flag] if flag == "--version" || flag == "-V" => {
            println!("pow {}", env!("CARGO_PKG_VERSION"));
            Ok(false)
        }
        [flag] if flag == "--help" || flag == "-h" => {
            println!("Pow · PostgreSQL Query Studio\n\nUsage: pow [--help | --version]\n\nRun `pow` to open the desktop query workspace.\nSaved workflows, connection settings and plans live in the local app data directory.\nPasswords are held in memory only; reconnect after restarting.");
            Ok(false)
        }
        _ => Err("Unknown argument. Run `pow --help` for usage.".into()),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match cli_action(&args) {
        Ok(true) => pow_desktop_lib::run(),
        Ok(false) => {}
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_usage() {
        assert!(cli_action(&[]).unwrap());
        assert!(!cli_action(&["--help".into()]).unwrap());
        assert!(!cli_action(&["--version".into()]).unwrap());
        assert!(cli_action(&["--invalid".into()]).is_err());
    }
}
