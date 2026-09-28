use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::time::Duration;

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("echo") => {
            let mut stdout = std::io::stdout().lock();
            for arg in args {
                stdout.write_all(arg.as_bytes()).unwrap();
                stdout.write_all(&[0]).unwrap();
            }
            std::io::stderr().write_all(b"fixture stderr").unwrap();
        }
        Some("output") => {
            for _ in 0..256 {
                std::io::stdout().write_all(&[b'o'; 8192]).unwrap();
                std::io::stderr().write_all(&[b'e'; 8192]).unwrap();
            }
        }
        Some("stdin") => {
            let mut bytes = Vec::new();
            std::io::stdin().read_to_end(&mut bytes).unwrap();
            assert!(bytes.is_empty());
        }
        Some("input") => {
            // Fill both output pipes before consuming input to expose deadlocks.
            std::io::stdout().write_all(&[b'o'; 65536]).unwrap();
            std::io::stderr().write_all(&[b'e'; 65536]).unwrap();
            let mut bytes = Vec::new();
            std::io::stdin().read_to_end(&mut bytes).unwrap();
            assert_eq!(bytes.len(), 131072);
            assert!(bytes.iter().all(|byte| *byte == b'i'));
        }
        Some("ignore-input") => std::thread::sleep(Duration::from_secs(60)),
        Some("dialogue") => {
            use std::io::BufRead;
            let mut input = std::io::stdin().lock().lines();
            assert_eq!(input.next().unwrap().unwrap(), "initialize");
            println!("initialized");
            assert_eq!(input.next().unwrap().unwrap(), "get_usage");
            println!("quota");
            std::thread::sleep(Duration::from_secs(60));
        }
        Some("exit") => std::process::exit(args.next().unwrap().parse().unwrap()),
        Some(mode @ ("tree" | "exit-tree")) => {
            let ready = args.next().unwrap();
            let _child = Command::new(std::env::current_exe().unwrap())
                .arg("descendant")
                .arg(&ready)
                .spawn()
                .unwrap();
            while !std::path::Path::new(&ready).is_file() {
                std::thread::sleep(Duration::from_millis(5));
            }
            if mode == "tree" {
                std::thread::sleep(Duration::from_secs(60));
            }
        }
        Some("descendant") => {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            std::fs::write(
                args.next().unwrap(),
                listener.local_addr().unwrap().to_string(),
            )
            .unwrap();
            // Keep stdout/stderr and a separately observable listener open.
            std::thread::sleep(Duration::from_secs(60));
            drop(listener);
        }
        _ => panic!("unknown fixture mode"),
    }
}
